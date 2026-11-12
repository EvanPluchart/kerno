//! Mapping MySQL and MariaDB types to Kerno's type families.
//!
//! How the types without an obvious family are presented:
//! - `TINYINT(1)` (what `BOOLEAN` is stored as) is [`TypeKind::Boolean`].
//!   Its cells hold the number the server sends, normally `0` or `1`; the
//!   column can hold any tinyint.
//! - `BIT(n)` is [`TypeKind::Bit`]. The server sends the bits packed in
//!   bytes; cells hold the `n` binary digits instead (`10101010`), the text
//!   of a `b'...'` literal.
//! - `YEAR` is [`TypeKind::Integer`]: four digits, sorted as a number.
//! - `TIMESTAMP` is [`TypeKind::DateTime`] like `DATETIME`: the server
//!   prints it in the session time zone, without the zone.
//! - `TIME` is [`TypeKind::Time`] although it is a duration: it ranges from
//!   `-838:59:59` to `838:59:59`.
//! - Zero dates (`0000-00-00`) arrive as the server prints them, like any
//!   other date.
//! - `JSON` is [`TypeKind::Json`] on MySQL. On MariaDB it is an alias of
//!   `LONGTEXT`: a result column is text, and the catalog tells it apart by
//!   its `json_valid` check (see `introspect`).
//! - `ENUM` and `SET` are [`TypeKind::Enum`]; a set's cell is its members
//!   separated by commas.
//! - Geometry is [`TypeKind::Binary`]: cells hold the server's internal
//!   form, a 4-byte little-endian SRID followed by WKB. `ST_AsText()` gives
//!   text.

use kerno_core::TypeKind;
use mysql_async::consts::{ColumnFlags, ColumnType};

/// Character set number of binary strings.
const BINARY_CHARSET: u16 = 63;

/// The display name and family of a result column, from what the protocol
/// says about it. `length` is the column's display width in bytes of the
/// connection character set (utf8mb4: up to 4 bytes per character).
pub(crate) fn describe(
    column_type: ColumnType,
    flags: ColumnFlags,
    charset: u16,
    length: u32,
) -> (String, TypeKind) {
    use ColumnType::*;
    let binary = charset == BINARY_CHARSET;
    let integer = |name: &str| {
        let unsigned = if flags.contains(ColumnFlags::UNSIGNED_FLAG) {
            " UNSIGNED"
        } else {
            ""
        };
        (format!("{name}{unsigned}"), TypeKind::Integer)
    };
    let fixed = |name: &str, kind: TypeKind| (name.to_string(), kind);
    // Character and byte strings share type codes; the character set and
    // two flags tell them apart.
    let string = |text: &str, bytes: &str| {
        if flags.contains(ColumnFlags::ENUM_FLAG) {
            fixed("ENUM", TypeKind::Enum)
        } else if flags.contains(ColumnFlags::SET_FLAG) {
            fixed("SET", TypeKind::Enum)
        } else if binary {
            fixed(bytes, TypeKind::Binary)
        } else {
            fixed(text, TypeKind::Text)
        }
    };
    match column_type {
        MYSQL_TYPE_TINY if length == 1 => fixed("TINYINT(1)", TypeKind::Boolean),
        MYSQL_TYPE_TINY => integer("TINYINT"),
        MYSQL_TYPE_SHORT => integer("SMALLINT"),
        MYSQL_TYPE_INT24 => integer("MEDIUMINT"),
        MYSQL_TYPE_LONG => integer("INT"),
        MYSQL_TYPE_LONGLONG => integer("BIGINT"),
        MYSQL_TYPE_YEAR => fixed("YEAR", TypeKind::Integer),
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => {
            let (name, _) = integer("DECIMAL");
            (name, TypeKind::Decimal)
        }
        MYSQL_TYPE_FLOAT => fixed("FLOAT", TypeKind::Float),
        MYSQL_TYPE_DOUBLE => fixed("DOUBLE", TypeKind::Float),
        MYSQL_TYPE_BIT => fixed("BIT", TypeKind::Bit),
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => fixed("DATE", TypeKind::Date),
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => fixed("TIME", TypeKind::Time),
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => fixed("DATETIME", TypeKind::DateTime),
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => fixed("TIMESTAMP", TypeKind::DateTime),
        MYSQL_TYPE_JSON => fixed("JSON", TypeKind::Json),
        MYSQL_TYPE_ENUM => fixed("ENUM", TypeKind::Enum),
        MYSQL_TYPE_SET => fixed("SET", TypeKind::Enum),
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING => string("VARCHAR", "VARBINARY"),
        MYSQL_TYPE_STRING => string("CHAR", "BINARY"),
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB => {
            // One type code for the four sizes: the width tells which.
            let capacity = if binary { length } else { length / 4 };
            let size = match capacity {
                0..=255 => "TINY",
                256..=65_535 => "",
                65_536..=16_777_215 => "MEDIUM",
                _ => "LONG",
            };
            if binary {
                (format!("{size}BLOB"), TypeKind::Binary)
            } else {
                (format!("{size}TEXT"), TypeKind::Text)
            }
        }
        MYSQL_TYPE_GEOMETRY => fixed("GEOMETRY", TypeKind::Binary),
        MYSQL_TYPE_VECTOR => fixed("VECTOR", TypeKind::Binary),
        MYSQL_TYPE_NULL => fixed("NULL", TypeKind::Other),
        MYSQL_TYPE_TYPED_ARRAY | MYSQL_TYPE_UNKNOWN => fixed("", TypeKind::Other),
    }
}

/// The family of a table column, from `information_schema.COLUMNS`:
/// `data_type` is `DATA_TYPE` (`bigint`), `column_type` is `COLUMN_TYPE`
/// (`bigint(20) unsigned`).
pub(crate) fn kind_of_catalog(data_type: &str, column_type: &str) -> TypeKind {
    match data_type.to_ascii_lowercase().as_str() {
        "tinyint" if column_type.to_ascii_lowercase().starts_with("tinyint(1)") => {
            TypeKind::Boolean
        }
        "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" | "year" => {
            TypeKind::Integer
        }
        "decimal" | "numeric" => TypeKind::Decimal,
        "float" | "double" | "real" => TypeKind::Float,
        "bit" => TypeKind::Bit,
        "char" | "varchar" | "tinytext" | "text" | "mediumtext" | "longtext" => TypeKind::Text,
        "binary" | "varbinary" | "tinyblob" | "blob" | "mediumblob" | "longblob" => {
            TypeKind::Binary
        }
        "date" => TypeKind::Date,
        "time" => TypeKind::Time,
        "datetime" | "timestamp" => TypeKind::DateTime,
        "json" => TypeKind::Json,
        "enum" | "set" => TypeKind::Enum,
        // MariaDB 10.7.
        "uuid" => TypeKind::Uuid,
        "geometry" | "point" | "linestring" | "polygon" | "multipoint" | "multilinestring"
        | "multipolygon" | "geometrycollection" | "geomcollection" | "vector" => TypeKind::Binary,
        _ => TypeKind::Other,
    }
}

/// The `width` binary digits of a `BIT(width)` value, which the server
/// sends packed big-endian in as many bytes as the width needs.
pub(crate) fn bit_string(bytes: &[u8], width: u32) -> String {
    let digits: String = bytes.iter().map(|byte| format!("{byte:08b}")).collect();
    let width = width as usize;
    if width == 0 || width > digits.len() {
        // Not a width the bytes can hold: every bit, rather than a guess.
        return digits;
    }
    digits[digits.len() - width..].to_string()
}

/// The labels of an `enum(...)` or `set(...)` column type, as printed in
/// `COLUMN_TYPE`: single-quoted, a quote inside a label doubled. Anything
/// else has no labels.
pub(crate) fn enum_labels(column_type: &str) -> Vec<String> {
    let lower = column_type.to_ascii_lowercase();
    let Some(start) = ["enum(", "set("]
        .iter()
        .find(|prefix| lower.starts_with(**prefix))
        .map(|prefix| prefix.len())
    else {
        return Vec::new();
    };
    let mut labels = Vec::new();
    let mut chars = column_type[start..].chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\'' {
            // A comma between labels, or the closing parenthesis.
            continue;
        }
        let mut label = String::new();
        while let Some(c) = chars.next() {
            if c != '\'' {
                label.push(c);
            } else if chars.peek() == Some(&'\'') {
                chars.next();
                label.push('\'');
            } else {
                break;
            }
        }
        labels.push(label);
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    const UTF8MB4: u16 = 255;
    const NONE: ColumnFlags = ColumnFlags::empty();

    fn named(column_type: ColumnType, flags: ColumnFlags, charset: u16, length: u32) -> String {
        describe(column_type, flags, charset, length).0
    }

    #[test]
    fn numbers_and_their_signs() {
        use ColumnType::*;
        assert_eq!(
            describe(MYSQL_TYPE_LONGLONG, NONE, BINARY_CHARSET, 20),
            ("BIGINT".into(), TypeKind::Integer)
        );
        assert_eq!(
            describe(
                MYSQL_TYPE_LONGLONG,
                ColumnFlags::UNSIGNED_FLAG,
                BINARY_CHARSET,
                20
            ),
            ("BIGINT UNSIGNED".into(), TypeKind::Integer)
        );
        assert_eq!(
            describe(
                MYSQL_TYPE_TINY,
                ColumnFlags::UNSIGNED_FLAG,
                BINARY_CHARSET,
                3
            ),
            ("TINYINT UNSIGNED".into(), TypeKind::Integer)
        );
        assert_eq!(
            describe(
                MYSQL_TYPE_NEWDECIMAL,
                ColumnFlags::UNSIGNED_FLAG,
                BINARY_CHARSET,
                12
            ),
            ("DECIMAL UNSIGNED".into(), TypeKind::Decimal)
        );
        assert_eq!(
            describe(MYSQL_TYPE_FLOAT, NONE, BINARY_CHARSET, 12),
            ("FLOAT".into(), TypeKind::Float)
        );
        assert_eq!(
            describe(
                MYSQL_TYPE_YEAR,
                ColumnFlags::UNSIGNED_FLAG,
                BINARY_CHARSET,
                4
            ),
            ("YEAR".into(), TypeKind::Integer)
        );
    }

    #[test]
    fn tinyint_of_width_one_is_a_boolean() {
        use ColumnType::*;
        assert_eq!(
            describe(MYSQL_TYPE_TINY, NONE, BINARY_CHARSET, 1),
            ("TINYINT(1)".into(), TypeKind::Boolean)
        );
        assert_eq!(
            describe(MYSQL_TYPE_TINY, NONE, BINARY_CHARSET, 4),
            ("TINYINT".into(), TypeKind::Integer)
        );
        assert_eq!(kind_of_catalog("tinyint", "tinyint(1)"), TypeKind::Boolean);
        assert_eq!(
            kind_of_catalog("tinyint", "tinyint(1) unsigned"),
            TypeKind::Boolean
        );
        assert_eq!(kind_of_catalog("tinyint", "tinyint(4)"), TypeKind::Integer);
        assert_eq!(kind_of_catalog("tinyint", "tinyint"), TypeKind::Integer);
    }

    #[test]
    fn strings_are_text_or_bytes_by_character_set() {
        use ColumnType::*;
        assert_eq!(
            describe(MYSQL_TYPE_VAR_STRING, NONE, UTF8MB4, 200),
            ("VARCHAR".into(), TypeKind::Text)
        );
        assert_eq!(
            describe(MYSQL_TYPE_VAR_STRING, NONE, BINARY_CHARSET, 16),
            ("VARBINARY".into(), TypeKind::Binary)
        );
        assert_eq!(
            describe(MYSQL_TYPE_STRING, NONE, UTF8MB4, 20),
            ("CHAR".into(), TypeKind::Text)
        );
        assert_eq!(
            describe(MYSQL_TYPE_STRING, NONE, BINARY_CHARSET, 4),
            ("BINARY".into(), TypeKind::Binary)
        );
        assert_eq!(
            describe(MYSQL_TYPE_STRING, ColumnFlags::ENUM_FLAG, UTF8MB4, 20),
            ("ENUM".into(), TypeKind::Enum)
        );
        assert_eq!(
            describe(MYSQL_TYPE_STRING, ColumnFlags::SET_FLAG, UTF8MB4, 20),
            ("SET".into(), TypeKind::Enum)
        );
    }

    #[test]
    fn blob_sizes_come_from_the_width() {
        use ColumnType::MYSQL_TYPE_BLOB as BLOB;
        assert_eq!(named(BLOB, NONE, BINARY_CHARSET, 255), "TINYBLOB");
        assert_eq!(named(BLOB, NONE, BINARY_CHARSET, 65_535), "BLOB");
        assert_eq!(named(BLOB, NONE, BINARY_CHARSET, 16_777_215), "MEDIUMBLOB");
        assert_eq!(named(BLOB, NONE, BINARY_CHARSET, u32::MAX), "LONGBLOB");
        // Text widths are in bytes of utf8mb4.
        assert_eq!(named(BLOB, NONE, UTF8MB4, 1_020), "TINYTEXT");
        assert_eq!(named(BLOB, NONE, UTF8MB4, 262_140), "TEXT");
        assert_eq!(named(BLOB, NONE, UTF8MB4, 67_108_860), "MEDIUMTEXT");
        assert_eq!(named(BLOB, NONE, UTF8MB4, u32::MAX), "LONGTEXT");
        assert_eq!(
            describe(BLOB, NONE, UTF8MB4, 262_140).1,
            TypeKind::Text,
            "a text column is not bytes"
        );
        assert_eq!(
            describe(BLOB, NONE, BINARY_CHARSET, 255).1,
            TypeKind::Binary
        );
    }

    #[test]
    fn other_types() {
        use ColumnType::*;
        for (column_type, name, kind) in [
            (MYSQL_TYPE_BIT, "BIT", TypeKind::Bit),
            (MYSQL_TYPE_DATE, "DATE", TypeKind::Date),
            (MYSQL_TYPE_TIME, "TIME", TypeKind::Time),
            (MYSQL_TYPE_DATETIME, "DATETIME", TypeKind::DateTime),
            (MYSQL_TYPE_TIMESTAMP, "TIMESTAMP", TypeKind::DateTime),
            (MYSQL_TYPE_JSON, "JSON", TypeKind::Json),
            (MYSQL_TYPE_GEOMETRY, "GEOMETRY", TypeKind::Binary),
            (MYSQL_TYPE_NULL, "NULL", TypeKind::Other),
        ] {
            assert_eq!(
                describe(column_type, NONE, BINARY_CHARSET, 10),
                (name.to_string(), kind)
            );
        }
    }

    #[test]
    fn catalog_types_map_to_their_family() {
        for (data_type, kind) in [
            ("bigint", TypeKind::Integer),
            ("mediumint", TypeKind::Integer),
            ("year", TypeKind::Integer),
            ("decimal", TypeKind::Decimal),
            ("double", TypeKind::Float),
            ("bit", TypeKind::Bit),
            ("varchar", TypeKind::Text),
            ("longtext", TypeKind::Text),
            ("varbinary", TypeKind::Binary),
            ("mediumblob", TypeKind::Binary),
            ("date", TypeKind::Date),
            ("time", TypeKind::Time),
            ("datetime", TypeKind::DateTime),
            ("timestamp", TypeKind::DateTime),
            ("json", TypeKind::Json),
            ("enum", TypeKind::Enum),
            ("set", TypeKind::Enum),
            ("uuid", TypeKind::Uuid),
            ("point", TypeKind::Binary),
            ("inet6", TypeKind::Other),
        ] {
            assert_eq!(kind_of_catalog(data_type, data_type), kind, "{data_type}");
        }
        assert_eq!(kind_of_catalog("BIGINT", "BIGINT"), TypeKind::Integer);
    }

    #[test]
    fn bit_values_become_digit_strings_of_the_declared_width() {
        assert_eq!(bit_string(&[0b1010_1010], 8), "10101010");
        assert_eq!(bit_string(&[0], 8), "00000000");
        assert_eq!(bit_string(&[1], 1), "1");
        assert_eq!(bit_string(&[0], 1), "0");
        // BIT(4) travels in one byte, BIT(12) in two.
        assert_eq!(bit_string(&[0b0000_0101], 4), "0101");
        assert_eq!(bit_string(&[0b0000_1111, 0b0000_0001], 12), "111100000001");
        assert_eq!(bit_string(&[0xff; 8], 64), "1".repeat(64));
        // A width the bytes cannot hold: all the bits there are.
        assert_eq!(bit_string(&[0b11], 0), "00000011");
        assert_eq!(bit_string(&[0b11], 16), "00000011");
        assert_eq!(bit_string(&[], 8), "");
    }

    #[test]
    fn enum_and_set_labels() {
        assert_eq!(
            enum_labels("enum('standard','vip','wholesale')"),
            ["standard", "vip", "wholesale"]
        );
        assert_eq!(enum_labels("set('a','b','c')"), ["a", "b", "c"]);
        // A quote, a comma and a parenthesis inside labels; an empty label.
        assert_eq!(
            enum_labels("enum('a','b''c','d,e','f)','')"),
            ["a", "b'c", "d,e", "f)", ""]
        );
        assert_eq!(enum_labels("ENUM('x')"), ["x"]);
        assert!(enum_labels("varchar(20)").is_empty());
        assert!(enum_labels("int").is_empty());
        assert!(enum_labels("enum(").is_empty());
    }
}
