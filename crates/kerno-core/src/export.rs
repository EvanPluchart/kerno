//! Writing rows out as CSV or TSV, JSON, Markdown or SQL `INSERT`
//! statements: clipboard copies and file exports share this code.
//!
//! An [`Exporter`] writes each row as it is given: it holds one row of
//! text at most, whatever the number of rows.

use std::borrow::Cow;
use std::collections::HashSet;
use std::io::{self, Write};

use crate::dialect::{Dialect, hex};
use crate::error::{Error, Result};
use crate::format::{compact_json, is_json_number, parse_bool};
use crate::result::{Column, TypeKind};
use crate::schema::{ColumnInfo, TableRef};
use crate::sql::edits::CellValue;

/// How lines end in delimited text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    /// `\n`.
    Lf,
    /// `\r\n`.
    CrLf,
}

impl LineEnding {
    fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
        }
    }
}

/// The shape of delimited text (CSV, TSV).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelimitedOptions {
    /// What separates the fields of a row.
    pub delimiter: char,
    /// Whether the first line names the columns.
    pub header: bool,
    /// What is written for NULL, unquoted. A value that reads the same
    /// (the empty string, when this is empty) is quoted, so the two stay
    /// distinct.
    pub null_text: String,
    /// What ends each line.
    pub line_ending: LineEnding,
}

impl DelimitedOptions {
    /// CSV as RFC 4180 describes it: commas, a header, `\r\n` line
    /// endings, nothing for NULL.
    pub fn csv() -> Self {
        Self {
            delimiter: ',',
            header: true,
            null_text: String::new(),
            line_ending: LineEnding::CrLf,
        }
    }

    /// Tab-separated values, as spreadsheets paste them: tabs, a header,
    /// `\n` line endings, nothing for NULL.
    pub fn tsv() -> Self {
        Self {
            delimiter: '\t',
            header: true,
            null_text: String::new(),
            line_ending: LineEnding::Lf,
        }
    }
}

/// What rows are written as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportFormat {
    /// Delimited text with RFC 4180 quoting: a field is quoted when it
    /// holds the delimiter, a quote, a line break, or a space at either
    /// end. Binary cells are `\x` and lowercase hex.
    Delimited(DelimitedOptions),
    /// One JSON array with an object per row: `[{…},\n{…}]`, and `[]`
    /// without rows.
    ///
    /// Objects are keyed by column name; a name already used gets a
    /// `_2`, `_3`… suffix. Numbers are bare when JSON can write them and
    /// strings otherwise (`NaN`, `00012`); booleans are `true`/`false`;
    /// JSON columns are embedded, on one line, when they hold JSON; binary
    /// is a `\x…` hex string; everything else is a string.
    JsonArray,
    /// One JSON object per row and per line, built like those of
    /// [`Self::JsonArray`].
    NdJson,
    /// A GitHub Markdown table. Numeric columns are right-aligned, NULL is
    /// `*NULL*`, a `|` or a `\` in a cell is escaped and a line break is
    /// `<br>`. Other Markdown in a cell is left for the reader to render.
    Markdown,
    /// One `INSERT` statement per row, with the dialect's own literals.
    SqlInsert {
        /// The table the statements insert into.
        table: TableRef,
    },
}

/// An [`ExportFormat`] with what is worked out once per export.
enum Layout {
    Delimited(DelimitedOptions),
    Json {
        /// `"name":` for each column, names made unique.
        keys: Vec<Vec<u8>>,
        array: bool,
    },
    Markdown,
    Insert {
        /// The statement up to the opening parenthesis of the values.
        head: String,
        /// The columns as the dialect wants them to write literals.
        columns: Vec<ColumnInfo>,
    },
}

/// Writes rows to `W` in one [`ExportFormat`].
///
/// Give it a buffered writer when `W` is a file: there is one write per
/// row.
pub struct Exporter<'a, W: Write> {
    writer: W,
    layout: Layout,
    columns: &'a [Column],
    dialect: &'a dyn Dialect,
    /// The row being written; reused from row to row.
    line: Vec<u8>,
    rows: u64,
}

impl<'a, W: Write> Exporter<'a, W> {
    /// An exporter of rows of `columns`. Nothing is written before the
    /// first row, or [`Self::finish`].
    pub fn new(
        writer: W,
        format: ExportFormat,
        columns: &'a [Column],
        dialect: &'a dyn Dialect,
    ) -> Self {
        let layout = match format {
            ExportFormat::Delimited(options) => Layout::Delimited(options),
            ExportFormat::JsonArray => Layout::Json {
                keys: json_keys(columns),
                array: true,
            },
            ExportFormat::NdJson => Layout::Json {
                keys: json_keys(columns),
                array: false,
            },
            ExportFormat::Markdown => Layout::Markdown,
            // A statement cannot list no column: the row is all defaults.
            ExportFormat::SqlInsert { table } if columns.is_empty() => Layout::Insert {
                head: format!("{};\n", dialect.insert_defaults(&table)),
                columns: Vec::new(),
            },
            ExportFormat::SqlInsert { table } => {
                let names: Vec<String> = columns
                    .iter()
                    .map(|column| dialect.quote_ident(&column.name))
                    .collect();
                Layout::Insert {
                    head: format!(
                        "INSERT INTO {} ({}) VALUES (",
                        dialect.table_name(&table),
                        names.join(", ")
                    ),
                    columns: columns.iter().map(column_info).collect(),
                }
            }
        };
        Self {
            writer,
            layout,
            columns,
            dialect,
            line: Vec::new(),
            rows: 0,
        }
    }

    /// Writes one row: a cell per column, in the order of the columns,
    /// `None` for NULL.
    ///
    /// Fails when the writer does, or when `cells` does not have one cell
    /// per column; nothing is written in that second case.
    pub fn write_row(&mut self, cells: &[Option<&[u8]>]) -> Result<()> {
        if cells.len() != self.columns.len() {
            return Err(Error::io(
                "export",
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "a row has {} cells for {} columns",
                        cells.len(),
                        self.columns.len()
                    ),
                ),
            ));
        }
        self.line.clear();
        if self.rows == 0 {
            self.push_header();
        }
        let line = &mut self.line;
        let cells = cells.iter().zip(self.columns);
        match &self.layout {
            Layout::Delimited(options) => {
                for (index, (cell, column)) in cells.enumerate() {
                    if index > 0 {
                        push_char(line, options.delimiter);
                    }
                    match cell {
                        Some(bytes) => push_field(line, &cell_text(bytes, column.kind), options),
                        None => line.extend_from_slice(options.null_text.as_bytes()),
                    }
                }
                line.extend_from_slice(options.line_ending.as_str().as_bytes());
            }
            Layout::Json { keys, array } => {
                if *array && self.rows > 0 {
                    line.extend_from_slice(b",\n");
                }
                line.push(b'{');
                for (index, (cell, column)) in cells.enumerate() {
                    if index > 0 {
                        line.push(b',');
                    }
                    line.extend_from_slice(&keys[index]);
                    push_json_value(line, *cell, column.kind);
                }
                line.push(b'}');
                if !*array {
                    line.push(b'\n');
                }
            }
            Layout::Markdown => {
                line.push(b'|');
                for (cell, column) in cells {
                    match cell {
                        Some(bytes) => push_markdown_cell(line, &cell_text(bytes, column.kind)),
                        None => line.extend_from_slice(b" *NULL* |"),
                    }
                }
                line.push(b'\n');
            }
            Layout::Insert { head, columns } => {
                line.extend_from_slice(head.as_bytes());
                for (index, (cell, column)) in cells.enumerate() {
                    if index > 0 {
                        line.extend_from_slice(b", ");
                    }
                    let value = match cell {
                        None => CellValue::Null,
                        Some(bytes) if column.kind == TypeKind::Binary => {
                            CellValue::Bytes(bytes.to_vec())
                        }
                        Some(bytes) => CellValue::Text(String::from_utf8_lossy(bytes).into_owned()),
                    };
                    let literal = self.dialect.literal(&value, &columns[index]);
                    line.extend_from_slice(literal.as_bytes());
                }
                if !columns.is_empty() {
                    line.extend_from_slice(b");\n");
                }
            }
        }
        self.writer.write_all(&self.line).map_err(write_error)?;
        self.rows += 1;
        Ok(())
    }

    /// Ends the export: writes what closes the format (the `]` of a JSON
    /// array, the header of a result without rows), flushes the writer and
    /// returns the number of rows written.
    pub fn finish(mut self) -> Result<u64> {
        self.line.clear();
        if self.rows == 0 {
            self.push_header();
        }
        if let Layout::Json { array: true, .. } = self.layout {
            self.line.push(b']');
        }
        self.writer
            .write_all(&self.line)
            .and_then(|()| self.writer.flush())
            .map_err(write_error)?;
        Ok(self.rows)
    }

    /// Appends what comes before the first row.
    fn push_header(&mut self) {
        let line = &mut self.line;
        match &self.layout {
            Layout::Delimited(options) if options.header => {
                for (index, column) in self.columns.iter().enumerate() {
                    if index > 0 {
                        push_char(line, options.delimiter);
                    }
                    push_field(line, &column.name, options);
                }
                line.extend_from_slice(options.line_ending.as_str().as_bytes());
            }
            Layout::Json { array: true, .. } => line.push(b'['),
            Layout::Markdown => {
                line.push(b'|');
                for column in self.columns {
                    push_markdown_cell(line, &column.name);
                }
                line.extend_from_slice(b"\n|");
                for column in self.columns {
                    line.extend_from_slice(if column.kind.is_numeric() {
                        b" ---: |"
                    } else {
                        b" --- |"
                    });
                }
                line.push(b'\n');
            }
            _ => {}
        }
    }
}

fn write_error(error: io::Error) -> Error {
    Error::io("writing the export", error)
}

/// The rows as one string, for the clipboard.
///
/// Rows are taken one at a time from any iterator, and a row is anything
/// that derefs to a slice of cells (a `Vec`, an array), so the caller
/// needs no copy of the result:
///
/// ```
/// # use kerno_core::export::{DelimitedOptions, ExportFormat, export_to_string};
/// # use kerno_core::{Column, Dialect, ResultStore};
/// fn copy_as_tsv(store: &ResultStore, dialect: &dyn Dialect) -> kerno_core::Result<String> {
///     let columns: &[Column] = store.columns();
///     let rows = (0..store.row_count())
///         .map(|row| (0..columns.len()).map(|column| store.cell(row, column)).collect::<Vec<_>>());
///     export_to_string(ExportFormat::Delimited(DelimitedOptions::tsv()), columns, dialect, rows)
/// }
/// ```
pub fn export_to_string<'c, R>(
    format: ExportFormat,
    columns: &[Column],
    dialect: &dyn Dialect,
    rows: impl IntoIterator<Item = R>,
) -> Result<String>
where
    R: AsRef<[Option<&'c [u8]>]>,
{
    let mut out = Vec::new();
    let mut exporter = Exporter::new(&mut out, format, columns, dialect);
    for row in rows {
        exporter.write_row(row.as_ref())?;
    }
    exporter.finish()?;
    String::from_utf8(out)
        .map_err(|error| Error::io("export", io::Error::new(io::ErrorKind::InvalidData, error)))
}

/// A cell as text: binary as `\x` and hex, bytes that are not UTF-8 as `�`.
fn cell_text(bytes: &[u8], kind: TypeKind) -> Cow<'_, str> {
    if kind == TypeKind::Binary {
        Cow::Owned(format!("\\x{}", hex(bytes)))
    } else {
        String::from_utf8_lossy(bytes)
    }
}

fn push_char(line: &mut Vec<u8>, c: char) {
    line.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
}

/// Appends a field of delimited text, quoted when it must be.
fn push_field(line: &mut Vec<u8>, field: &str, options: &DelimitedOptions) {
    let quoted = field == options.null_text
        || field.starts_with(' ')
        || field.ends_with(' ')
        || field.contains(['"', '\r', '\n', options.delimiter]);
    if !quoted {
        return line.extend_from_slice(field.as_bytes());
    }
    line.push(b'"');
    for part in field.split_inclusive('"') {
        line.extend_from_slice(part.as_bytes());
        if part.ends_with('"') {
            line.push(b'"');
        }
    }
    line.push(b'"');
}

/// The table's view of a result column, for [`Dialect::literal`].
fn column_info(column: &Column) -> ColumnInfo {
    ColumnInfo {
        name: column.name.clone(),
        data_type: column.type_name.clone(),
        kind: column.kind,
        nullable: true,
        default: None,
        generation: None,
        on_update: None,
        invisible: false,
        primary_key_position: None,
        comment: None,
        collation: None,
        enum_values: Vec::new(),
    }
}

/// `"name":` for each column. A name used by an earlier column gets the
/// first `_2`, `_3`… suffix that no column has, so no value is lost.
fn json_keys(columns: &[Column]) -> Vec<Vec<u8>> {
    let mut taken: HashSet<String> = columns.iter().map(|column| column.name.clone()).collect();
    let mut seen = HashSet::new();
    columns
        .iter()
        .map(|column| {
            let mut name = column.name.clone();
            if !seen.insert(column.name.as_str()) {
                let mut suffix = 2;
                while taken.contains(&name) {
                    name = format!("{}_{suffix}", column.name);
                    suffix += 1;
                }
                taken.insert(name.clone());
            }
            let mut key = Vec::new();
            push_json_string(&mut key, &name);
            key.push(b':');
            key
        })
        .collect()
}

/// Appends a cell as a JSON value.
fn push_json_value(line: &mut Vec<u8>, cell: Option<&[u8]>, kind: TypeKind) {
    let Some(bytes) = cell else {
        return line.extend_from_slice(b"null");
    };
    let text = cell_text(bytes, kind);
    let bare = match kind {
        TypeKind::Integer | TypeKind::Float | TypeKind::Decimal => {
            is_json_number(&text).then_some(Cow::Borrowed(&*text))
        }
        TypeKind::Boolean => {
            parse_bool(bytes).map(|value| Cow::Borrowed(if value { "true" } else { "false" }))
        }
        TypeKind::Json => compact_json(&text).map(Cow::Owned),
        _ => None,
    };
    match bare {
        Some(bare) => line.extend_from_slice(bare.as_bytes()),
        None => push_json_string(line, &text),
    }
}

/// Appends `text` as a JSON string.
fn push_json_string(line: &mut Vec<u8>, text: &str) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    line.push(b'"');
    for &byte in text.as_bytes() {
        match byte {
            b'"' => line.extend_from_slice(b"\\\""),
            b'\\' => line.extend_from_slice(b"\\\\"),
            b'\n' => line.extend_from_slice(b"\\n"),
            b'\r' => line.extend_from_slice(b"\\r"),
            b'\t' => line.extend_from_slice(b"\\t"),
            0..=0x1f => line.extend_from_slice(&[
                b'\\',
                b'u',
                b'0',
                b'0',
                HEX[usize::from(byte >> 4)],
                HEX[usize::from(byte & 0x0f)],
            ]),
            _ => line.push(byte),
        }
    }
    line.push(b'"');
}

/// Appends ` text |`, with what would end the cell or the row escaped.
fn push_markdown_cell(line: &mut Vec<u8>, text: &str) {
    line.push(b' ');
    let mut bytes = text.bytes().peekable();
    while let Some(byte) = bytes.next() {
        match byte {
            b'|' => line.extend_from_slice(b"\\|"),
            b'\\' => line.extend_from_slice(b"\\\\"),
            b'\r' => {
                bytes.next_if_eq(&b'\n');
                line.extend_from_slice(b"<br>");
            }
            b'\n' => line.extend_from_slice(b"<br>"),
            _ => line.push(byte),
        }
    }
    line.extend_from_slice(b" |");
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;
    use crate::dialect::test_support::AnsiDialect;
    use crate::error::Error;
    use crate::result::TypeKind;

    type Row<'c> = Vec<Option<&'c [u8]>>;

    fn cell(text: &str) -> Option<&[u8]> {
        Some(text.as_bytes())
    }

    fn bytes(bytes: &[u8]) -> Option<&[u8]> {
        Some(bytes)
    }

    fn export(format: ExportFormat, columns: &[Column], rows: &[Row<'_>]) -> String {
        export_to_string(format, columns, &AnsiDialect, rows).unwrap()
    }

    fn csv(columns: &[Column], rows: &[Row<'_>]) -> String {
        export(
            ExportFormat::Delimited(DelimitedOptions::csv()),
            columns,
            rows,
        )
    }

    fn insert() -> ExportFormat {
        ExportFormat::SqlInsert {
            table: TableRef::new("shop", "people"),
        }
    }

    fn every_format() -> Vec<ExportFormat> {
        vec![
            ExportFormat::Delimited(DelimitedOptions::csv()),
            ExportFormat::Delimited(DelimitedOptions::tsv()),
            ExportFormat::JsonArray,
            ExportFormat::NdJson,
            ExportFormat::Markdown,
            insert(),
        ]
    }

    fn text_columns(names: &[&str]) -> Vec<Column> {
        names
            .iter()
            .map(|name| Column::new(*name, "text", TypeKind::Text))
            .collect()
    }

    /// One text column and one row per value.
    fn text_rows<'c>(values: &[&'c str]) -> Vec<Row<'c>> {
        values
            .iter()
            .map(|value| vec![Some(value.as_bytes())])
            .collect()
    }

    fn sample_columns() -> Vec<Column> {
        vec![
            Column::new("id", "int4", TypeKind::Integer),
            Column::new("name", "text", TypeKind::Text),
            Column::new("price", "numeric", TypeKind::Decimal),
            Column::new("active", "bool", TypeKind::Boolean),
            Column::new("meta", "jsonb", TypeKind::Json),
            Column::new("blob", "bytea", TypeKind::Binary),
        ]
    }

    fn sample_rows() -> Vec<Row<'static>> {
        vec![
            vec![
                cell("1"),
                cell("Ada"),
                cell("12.50"),
                cell("t"),
                cell(r#"{"tags": ["a", "b"]}"#),
                bytes(b"\xde\xad"),
            ],
            vec![
                cell("2"),
                cell(r#"O'Brien, "Bob""#),
                None,
                cell("f"),
                None,
                None,
            ],
            vec![
                cell("3"),
                cell(""),
                cell("NaN"),
                None,
                cell("[1, 2.0]"),
                bytes(b""),
            ],
        ]
    }

    #[test]
    fn sample_as_csv() {
        assert_eq!(
            csv(&sample_columns(), &sample_rows()),
            "id,name,price,active,meta,blob\r\n\
             1,Ada,12.50,t,\"{\"\"tags\"\": [\"\"a\"\", \"\"b\"\"]}\",\\xdead\r\n\
             2,\"O'Brien, \"\"Bob\"\"\",,f,,\r\n\
             3,\"\",NaN,,\"[1, 2.0]\",\\x\r\n"
        );
    }

    #[test]
    fn sample_as_tsv() {
        assert_eq!(
            export(
                ExportFormat::Delimited(DelimitedOptions::tsv()),
                &sample_columns(),
                &sample_rows()
            ),
            "id\tname\tprice\tactive\tmeta\tblob\n\
             1\tAda\t12.50\tt\t\"{\"\"tags\"\": [\"\"a\"\", \"\"b\"\"]}\"\t\\xdead\n\
             2\t\"O'Brien, \"\"Bob\"\"\"\t\tf\t\t\n\
             3\t\"\"\tNaN\t\t[1, 2.0]\t\\x\n"
        );
    }

    const SAMPLE_OBJECTS: [&str; 3] = [
        r#"{"id":1,"name":"Ada","price":12.50,"active":true,"meta":{"tags":["a","b"]},"blob":"\\xdead"}"#,
        r#"{"id":2,"name":"O'Brien, \"Bob\"","price":null,"active":false,"meta":null,"blob":null}"#,
        r#"{"id":3,"name":"","price":"NaN","active":null,"meta":[1,2.0],"blob":"\\x"}"#,
    ];

    #[test]
    fn sample_as_json_array() {
        let json = export(ExportFormat::JsonArray, &sample_columns(), &sample_rows());
        assert_eq!(json, format!("[{}]", SAMPLE_OBJECTS.join(",\n")));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 3);
        assert_eq!(parsed[0]["price"], serde_json::json!(12.5));
        assert_eq!(parsed[0]["meta"]["tags"][1], "b");
        assert_eq!(parsed[0]["blob"], "\\xdead");
        assert_eq!(parsed[1]["name"], "O'Brien, \"Bob\"");
        assert!(parsed[1]["meta"].is_null());
        assert_eq!(parsed[2]["name"], "");
        assert_eq!(parsed[2]["price"], "NaN");
    }

    #[test]
    fn sample_as_ndjson() {
        let ndjson = export(ExportFormat::NdJson, &sample_columns(), &sample_rows());
        assert_eq!(ndjson, format!("{}\n", SAMPLE_OBJECTS.join("\n")));
        for line in ndjson.lines() {
            assert!(
                serde_json::from_str::<serde_json::Value>(line)
                    .unwrap()
                    .is_object()
            );
        }
    }

    #[test]
    fn sample_as_markdown() {
        assert_eq!(
            export(ExportFormat::Markdown, &sample_columns(), &sample_rows()),
            r#"| id | name | price | active | meta | blob |
| ---: | --- | ---: | --- | --- | --- |
| 1 | Ada | 12.50 | t | {"tags": ["a", "b"]} | \\xdead |
| 2 | O'Brien, "Bob" | *NULL* | f | *NULL* | *NULL* |
| 3 |  | NaN | *NULL* | [1, 2.0] | \\x |
"#
        );
    }

    #[test]
    fn sample_as_sql_insert() {
        assert_eq!(
            export(insert(), &sample_columns(), &sample_rows()),
            r#"INSERT INTO "shop"."people" ("id", "name", "price", "active", "meta", "blob") VALUES (1, 'Ada', 12.50, TRUE, '{"tags": ["a", "b"]}', X'dead');
INSERT INTO "shop"."people" ("id", "name", "price", "active", "meta", "blob") VALUES (2, 'O''Brien, "Bob"', NULL, FALSE, NULL, NULL);
INSERT INTO "shop"."people" ("id", "name", "price", "active", "meta", "blob") VALUES (3, '', 'NaN', NULL, '[1, 2.0]', X'');
"#
        );
    }

    #[test]
    fn delimited_fields_are_quoted_only_when_they_must_be() {
        let fields = [
            ("plain", "plain"),
            ("a b", "a b"),
            ("a,b", "\"a,b\""),
            ("a\"b", "\"a\"\"b\""),
            ("\"", "\"\"\"\""),
            ("a\rb", "\"a\rb\""),
            ("a\nb", "\"a\nb\""),
            ("a\r\nb", "\"a\r\nb\""),
            (" a", "\" a\""),
            ("a ", "\"a \""),
            (" ", "\" \""),
            ("", "\"\""),
            ("a\tb", "a\tb"),
            ("a;b", "a;b"),
            ("caf\u{e9} \u{1f600}", "caf\u{e9} \u{1f600}"),
        ];
        let columns = text_columns(&["c"]);
        for (value, written) in fields {
            assert_eq!(
                csv(&columns, &text_rows(&[value])),
                format!("c\r\n{written}\r\n"),
                "{value:?}"
            );
        }
    }

    #[test]
    fn the_delimiter_decides_what_is_quoted() {
        let columns = text_columns(&["a,b", "c\td", "e;f"]);
        let rows = vec![vec![cell("1,2"), cell("3\t4"), cell("5;6")]];
        assert_eq!(
            csv(&columns, &rows),
            "\"a,b\",c\td,e;f\r\n\"1,2\",3\t4,5;6\r\n"
        );
        assert_eq!(
            export(
                ExportFormat::Delimited(DelimitedOptions::tsv()),
                &columns,
                &rows
            ),
            "a,b\t\"c\td\"\te;f\n1,2\t\"3\t4\"\t5;6\n"
        );
        let semicolons = DelimitedOptions {
            delimiter: ';',
            ..DelimitedOptions::csv()
        };
        assert_eq!(
            export(ExportFormat::Delimited(semicolons), &columns, &rows),
            "a,b;c\td;\"e;f\"\r\n1,2;3\t4;\"5;6\"\r\n"
        );
        // A delimiter of several bytes.
        let arrows = DelimitedOptions {
            delimiter: '\u{2192}',
            line_ending: LineEnding::Lf,
            ..DelimitedOptions::csv()
        };
        let columns = text_columns(&["a", "b"]);
        let rows = vec![vec![cell("x\u{2192}y"), cell("z")]];
        assert_eq!(
            export(ExportFormat::Delimited(arrows), &columns, &rows),
            "a\u{2192}b\n\"x\u{2192}y\"\u{2192}z\n"
        );
    }

    #[test]
    fn null_and_empty_stay_distinct_in_delimited_text() {
        let columns = text_columns(&["a", "b", "c"]);
        let rows = vec![vec![None, cell(""), cell("NULL")], vec![None, None, None]];
        assert_eq!(csv(&columns, &rows), "a,b,c\r\n,\"\",NULL\r\n,,\r\n");
        // With a word for NULL, it is that word as a value that is quoted.
        let options = DelimitedOptions {
            null_text: "NULL".into(),
            header: false,
            line_ending: LineEnding::Lf,
            ..DelimitedOptions::csv()
        };
        assert_eq!(
            export(ExportFormat::Delimited(options), &columns, &rows),
            "NULL,,\"NULL\"\nNULL,NULL,NULL\n"
        );
    }

    #[test]
    fn binary_and_broken_text_are_written_as_text() {
        let columns = vec![
            Column::new("b", "bytea", TypeKind::Binary),
            Column::new("t", "text", TypeKind::Text),
        ];
        let rows = vec![vec![bytes(b"\x00\x01\xff,\""), bytes(b"ab\xffcd")]];
        assert_eq!(
            csv(&columns, &rows),
            "b,t\r\n\\x0001ff2c22,ab\u{fffd}cd\r\n"
        );
        assert_eq!(
            export(ExportFormat::NdJson, &columns, &rows),
            "{\"b\":\"\\\\x0001ff2c22\",\"t\":\"ab\u{fffd}cd\"}\n"
        );
        assert_eq!(
            export(ExportFormat::Markdown, &columns, &rows),
            "| b | t |\n| --- | --- |\n| \\\\x0001ff2c22 | ab\u{fffd}cd |\n"
        );
        assert_eq!(
            export(insert(), &columns, &rows),
            "INSERT INTO \"shop\".\"people\" (\"b\", \"t\") VALUES (X'0001ff2c22', 'ab\u{fffd}cd');\n"
        );
    }

    #[test]
    fn a_result_without_rows() {
        let columns = sample_columns();
        let none: [Row<'_>; 0] = [];
        let written: Vec<String> = every_format()
            .into_iter()
            .map(|format| export(format, &columns, &none))
            .collect();
        assert_eq!(
            written,
            [
                "id,name,price,active,meta,blob\r\n",
                "id\tname\tprice\tactive\tmeta\tblob\n",
                "[]",
                "",
                "| id | name | price | active | meta | blob |\n| ---: | --- | ---: | --- | --- | --- |\n",
                "",
            ]
        );
        let no_header = DelimitedOptions {
            header: false,
            ..DelimitedOptions::csv()
        };
        assert_eq!(
            export(ExportFormat::Delimited(no_header), &columns, &none),
            ""
        );
    }

    #[test]
    fn a_result_without_columns() {
        let rows: Vec<Row<'_>> = vec![vec![], vec![]];
        let written: Vec<String> = every_format()
            .into_iter()
            .map(|format| export(format, &[], &rows))
            .collect();
        assert_eq!(
            written,
            [
                "\r\n\r\n\r\n",
                "\n\n\n",
                "[{},\n{}]",
                "{}\n{}\n",
                "|\n|\n|\n|\n",
                "INSERT INTO \"shop\".\"people\" DEFAULT VALUES;\nINSERT INTO \"shop\".\"people\" DEFAULT VALUES;\n",
            ]
        );
    }

    #[test]
    fn json_numbers_are_bare_only_when_json_can_hold_them() {
        let columns = vec![Column::new("n", "numeric", TypeKind::Decimal)];
        let big = "1234567890123456789012345678901234567890";
        let cases = [
            ("0", "0"),
            ("-0", "-0"),
            ("42", "42"),
            ("-12.50", "-12.50"),
            ("1e10", "1e10"),
            ("1.5E-7", "1.5E-7"),
            (big, big),
            (".5", "\".5\""),
            ("5.", "\"5.\""),
            ("00012", "\"00012\""),
            ("+1", "\"+1\""),
            ("NaN", "\"NaN\""),
            ("Infinity", "\"Infinity\""),
            ("-Infinity", "\"-Infinity\""),
            ("$1,234.56", "\"$1,234.56\""),
            ("", "\"\""),
        ];
        for (value, written) in cases {
            let line = export(ExportFormat::NdJson, &columns, &text_rows(&[value]));
            assert_eq!(line, format!("{{\"n\":{written}}}\n"), "{value:?}");
            serde_json::from_str::<serde_json::Value>(&line).unwrap();
        }
        // Digits in a text column are text.
        assert_eq!(
            export(
                ExportFormat::NdJson,
                &text_columns(&["n"]),
                &text_rows(&["42"])
            ),
            "{\"n\":\"42\"}\n"
        );
    }

    #[test]
    fn json_booleans_and_documents() {
        let columns = vec![
            Column::new("b", "bool", TypeKind::Boolean),
            Column::new("j", "json", TypeKind::Json),
        ];
        let cases: [(&str, &str, &str); 6] = [
            ("t", "{\"a\": 1}", r#"{"b":true,"j":{"a":1}}"#),
            ("0", "null", r#"{"b":false,"j":null}"#),
            ("1", "\"text\"", r#"{"b":true,"j":"text"}"#),
            ("FALSE", " 1.50 ", r#"{"b":false,"j":1.50}"#),
            // A document over several lines stays on the line of its row.
            (
                "true",
                "{\n  \"a\": [\n    1,\n    \"x\\ny\"\n  ]\n}",
                r#"{"b":true,"j":{"a":[1,"x\ny"]}}"#,
            ),
            // What is not a boolean or not JSON is a string.
            ("yes", "{broken", r#"{"b":"yes","j":"{broken"}"#),
        ];
        for (boolean, document, written) in cases {
            let rows = vec![vec![Some(boolean.as_bytes()), Some(document.as_bytes())]];
            let line = export(ExportFormat::NdJson, &columns, &rows);
            assert_eq!(line, format!("{written}\n"));
            assert_eq!(line.lines().count(), 1);
            serde_json::from_str::<serde_json::Value>(&line).unwrap();
        }
    }

    #[test]
    fn json_strings_are_escaped() {
        let value = "quote \" backslash \\ slash / newline \n return \r tab \t nul \u{0} esc \u{1b} \
                     del \u{7f} caf\u{e9} \u{1f600} \u{2028}";
        let key = "we\"ird\\key\n";
        let columns = text_columns(&[key]);
        for format in [ExportFormat::JsonArray, ExportFormat::NdJson] {
            let json = export(format.clone(), &columns, &text_rows(&[value]));
            assert!(json.contains(
                r#"quote \" backslash \\ slash / newline \n return \r tab \t nul \u0000 esc \u001b"#
            ));
            assert!(json.contains(r#""we\"ird\\key\n":"#));
            let parsed: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
            let row = if format == ExportFormat::JsonArray {
                &parsed[0]
            } else {
                &parsed
            };
            assert_eq!(row[key], value);
        }
    }

    #[test]
    fn duplicate_column_names_get_a_suffix_in_json() {
        let columns = text_columns(&["a", "a", "a_2", "a", "b", ""]);
        let rows = vec![vec![
            cell("1"),
            cell("2"),
            cell("3"),
            cell("4"),
            cell("5"),
            cell("6"),
        ]];
        let line = export(ExportFormat::NdJson, &columns, &rows);
        assert_eq!(
            line,
            "{\"a\":\"1\",\"a_3\":\"2\",\"a_2\":\"3\",\"a_4\":\"4\",\"b\":\"5\",\"\":\"6\"}\n"
        );
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed.as_object().unwrap().len(), 6);
        // Other formats keep the names as they are.
        assert_eq!(csv(&columns, &rows), "a,a,a_2,a,b,\"\"\r\n1,2,3,4,5,6\r\n");
    }

    #[test]
    fn markdown_cells_stay_in_their_cell() {
        let columns = vec![
            Column::new("a|b", "text", TypeKind::Text),
            Column::new("n", "float8", TypeKind::Float),
        ];
        let rows = vec![
            vec![cell("x|y"), cell("1.5")],
            vec![cell("line 1\nline 2\r\nline 3\rline 4"), None],
            vec![cell("C:\\dir\\|"), cell("-Infinity")],
            vec![cell("NULL"), cell("")],
        ];
        assert_eq!(
            export(ExportFormat::Markdown, &columns, &rows),
            "| a\\|b | n |\n\
             | --- | ---: |\n\
             | x\\|y | 1.5 |\n\
             | line 1<br>line 2<br>line 3<br>line 4 | *NULL* |\n\
             | C:\\\\dir\\\\\\| | -Infinity |\n\
             | NULL |  |\n"
        );
    }

    #[test]
    fn sql_literals_follow_the_column_kind() {
        let columns = vec![
            Column::new("weird \"name\"", "int8", TypeKind::Integer),
            Column::new("d", "date", TypeKind::Date),
            Column::new("b", "bool", TypeKind::Boolean),
            Column::new("t", "text", TypeKind::Text),
        ];
        let rows = vec![
            vec![cell("-0"), cell("2024-02-29"), cell("f"), cell("42")],
            vec![cell("1; DROP TABLE x"), None, cell("1"), cell("it's\n")],
        ];
        assert_eq!(
            export(insert(), &columns, &rows),
            "INSERT INTO \"shop\".\"people\" (\"weird \"\"name\"\"\", \"d\", \"b\", \"t\") \
             VALUES (-0, '2024-02-29', FALSE, '42');\n\
             INSERT INTO \"shop\".\"people\" (\"weird \"\"name\"\"\", \"d\", \"b\", \"t\") \
             VALUES ('1; DROP TABLE x', NULL, '1', 'it''s\n');\n"
        );
    }

    #[test]
    fn a_five_megabyte_cell_is_written_whole() {
        let big = "x\"".repeat(5 * 1024 * 1024 / 2);
        let columns = text_columns(&["c"]);
        let rows = text_rows(&[big.as_str()]);
        let written = csv(&columns, &rows);
        assert_eq!(written.len(), "c\r\n".len() + 2 + big.len() / 2 * 3 + 2);
        let json = export(ExportFormat::JsonArray, &columns, &rows);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["c"].as_str().unwrap().len(), big.len());
    }

    /// Counts what reaches it, the way a file would.
    #[derive(Default)]
    struct Sink {
        bytes: Vec<u8>,
        flushes: usize,
    }

    impl Write for Sink {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn rows_reach_the_writer_as_they_come() {
        let columns = sample_columns();
        for format in every_format() {
            let mut sink = Sink::default();
            let mut exporter = Exporter::new(&mut sink, format.clone(), &columns, &AnsiDialect);
            let mut written = 0;
            for row in sample_rows() {
                exporter.write_row(&row).unwrap();
                // Nothing is held back until the end.
                let now = exporter.writer.bytes.len();
                assert!(now > written, "{format:?}");
                written = now;
            }
            assert_eq!(exporter.finish().unwrap(), 3);
            assert_eq!(sink.flushes, 1);
            assert_eq!(
                String::from_utf8(sink.bytes).unwrap(),
                export(format, &columns, &sample_rows())
            );
        }
    }

    #[test]
    fn rows_can_come_from_any_iterator() {
        let columns = text_columns(&["a", "b"]);
        let values = ["1", "2", "3"];
        // Arrays, built one at a time: nothing is collected.
        let rows = values.iter().map(|value| [Some(value.as_bytes()), None]);
        assert_eq!(
            export_to_string(
                ExportFormat::Delimited(DelimitedOptions::tsv()),
                &columns,
                &AnsiDialect,
                rows
            )
            .unwrap(),
            "a\tb\n1\t\n2\t\n3\t\n"
        );
    }

    #[test]
    fn a_row_of_the_wrong_length_is_refused() {
        let columns = text_columns(&["a", "b"]);
        for format in every_format() {
            let mut out = Vec::new();
            let mut exporter = Exporter::new(&mut out, format, &columns, &AnsiDialect);
            let error = exporter.write_row(&[cell("only one")]).unwrap_err();
            assert!(
                error.to_string().contains("1 cells for 2 columns"),
                "{error}"
            );
            exporter.write_row(&[None, None, None]).unwrap_err();
            // The export goes on with the rows that fit.
            exporter.write_row(&[None, None]).unwrap();
            assert_eq!(exporter.finish().unwrap(), 1);
        }
    }

    /// A disk that is full.
    struct Broken;

    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("disk full"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_failed_write_is_reported() {
        let columns = text_columns(&["a"]);
        for format in every_format() {
            let mut exporter = Exporter::new(Broken, format, &columns, &AnsiDialect);
            let error = exporter.write_row(&[cell("x")]).unwrap_err();
            assert!(matches!(error, Error::Io { .. }), "{error:?}");
            assert!(error.to_string().contains("disk full"), "{error}");
        }
        // Also when the only thing to write is the header or the footer.
        let error = Exporter::new(Broken, ExportFormat::JsonArray, &columns, &AnsiDialect)
            .finish()
            .unwrap_err();
        assert!(matches!(error, Error::Io { .. }), "{error:?}");
    }
}
