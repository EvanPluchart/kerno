//! The HTML document: one file that asks the network for nothing (inline
//! CSS, the diagram as inline SVG, no script, no web font), with a table of
//! contents of anchors, and print rules (a page per table, header rows that
//! repeat, the dark style printed on white).
//!
//! Every text from the database goes through [`esc`]; it never lands in a
//! `<script>`, a `<style>`, an attribute or a URL: anchors are numbered.

use std::fmt::Write as _;
use std::sync::OnceLock;

use super::{
    CODE_KINDS, Code, Document, Options, Style, Table, column_rows, constraint_label, kind_label,
    primary_key, referenced,
};
use crate::erd;
use crate::schema::ConstraintKind;
use crate::table_draft::present;

/// The app's theme file: the Kerno styles take their colours from its
/// tokens, the one place they are written.
const THEME_JSON: &str = include_str!("../../../kerno/assets/themes/kerno.json");
/// The horizontal lockup, ink on light and reversed (assets/brand/BRAND.md).
const LOCKUP: &str = include_str!("../../../../assets/brand/kerno-horizontal.svg");
const LOCKUP_REVERSED: &str =
    include_str!("../../../../assets/brand/kerno-horizontal-reversed.svg");

/// The colours of a style, as `#rrggbb`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Colours {
    pub background: String,
    /// Table headers, code blocks.
    pub surface: String,
    pub border: String,
    pub text: String,
    /// Labels, types, the footer.
    pub muted: String,
    /// Links and key marks.
    pub accent: String,
    /// The header of a table in the diagram.
    pub header: String,
}

/// A colour token of one of the app's themes.
fn token(theme: &str, key: &str) -> String {
    static THEMES: OnceLock<serde_json::Value> = OnceLock::new();
    let themes = THEMES
        .get_or_init(|| serde_json::from_str(THEME_JSON).expect("the embedded theme file is JSON"));
    themes["themes"]
        .as_array()
        .and_then(|themes| themes.iter().find(|t| t["name"] == theme))
        .and_then(|theme| theme["colors"][key].as_str())
        .unwrap_or_else(|| panic!("the theme file has no {key} in {theme}"))
        .to_ascii_lowercase()
}

/// The colours of `style`: the light or the dark theme's tokens, or black
/// on white with greys.
pub fn colours(style: Style) -> Colours {
    let theme = match style {
        Style::KernoLight => "Kerno Light",
        Style::KernoDark => "Kerno Dark",
        Style::Plain => {
            return Colours {
                background: "#ffffff".into(),
                surface: "#f2f2f2".into(),
                border: "#bdbdbd".into(),
                text: "#000000".into(),
                muted: "#4a4a4a".into(),
                accent: "#000000".into(),
                header: "#eeeeee".into(),
            };
        }
    };
    Colours {
        background: token(theme, "background"),
        surface: token(theme, "list.head.background"),
        border: token(theme, "border"),
        text: token(theme, "foreground"),
        muted: token(theme, "muted.foreground"),
        accent: token(theme, "link"),
        header: token(theme, "table.head.background"),
    }
}

/// `text` for HTML text and attribute values.
pub(super) fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c if c.is_control() && !matches!(c, '\n' | '\t') => {}
            c => out.push(c),
        }
    }
    out
}

fn vars(colours: &Colours) -> String {
    format!(
        "--bg:{};--surface:{};--border:{};--text:{};--muted:{};--accent:{};",
        colours.background,
        colours.surface,
        colours.border,
        colours.text,
        colours.muted,
        colours.accent
    )
}

/// The style sheet: `screen`'s colours, `print`'s on paper.
fn css(screen: &Colours, print: &Colours, dark: bool) -> String {
    format!(
        r#":root{{{screen}color-scheme:{scheme}}}
*{{box-sizing:border-box}}
html{{background:var(--bg)}}
body{{margin:0 auto;max-width:1120px;padding:40px 32px 56px;background:var(--bg);color:var(--text);font:14px/1.5 Inter,-apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif;-webkit-text-size-adjust:100%}}
code,pre,.mono{{font-family:"JetBrains Mono",ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:12.5px}}
h1{{font-size:30px;line-height:1.2;font-weight:600;margin:20px 0 4px;overflow-wrap:anywhere}}
h2{{font-size:20px;font-weight:600;margin:44px 0 12px;padding-bottom:6px;border-bottom:1px solid var(--border)}}
h3{{font-size:16px;font-weight:600;margin:0 0 8px;overflow-wrap:anywhere}}
h4{{font-size:12px;font-weight:600;text-transform:uppercase;letter-spacing:.04em;color:var(--muted);margin:18px 0 6px}}
a{{color:var(--accent)}}
.lockup{{margin-bottom:28px}}
.lockup svg{{display:block;height:32px;width:auto}}
.facts{{display:grid;grid-template-columns:repeat(auto-fit,minmax(170px,1fr));gap:12px 24px;margin:20px 0 0}}
.facts dt{{font-size:12px;color:var(--muted)}}
.facts dd{{margin:0;overflow-wrap:anywhere}}
.toc ol{{margin:4px 0;padding-left:22px}}
.toc li{{overflow-wrap:anywhere}}
.figure{{overflow:auto;border:1px solid var(--border);border-radius:8px}}
.figure svg{{display:block;max-width:100%;height:auto}}
.object{{margin:28px 0 0;padding-top:4px}}
.kind{{font-size:12px;font-weight:400;color:var(--muted)}}
.comment{{white-space:pre-line;overflow-wrap:anywhere;margin:0 0 10px}}
table{{width:100%;border-collapse:collapse}}
th,td{{text-align:left;vertical-align:top;padding:5px 8px;border-bottom:1px solid var(--border);overflow-wrap:anywhere;word-break:break-word}}
thead th{{background:var(--surface);font-size:12px;font-weight:600;color:var(--muted);white-space:nowrap}}
.columns td:nth-child(1){{min-width:8em}}
.columns td:nth-child(3),.columns td:nth-child(5){{white-space:nowrap}}
.columns td:nth-child(6){{min-width:10em}}
td.note{{white-space:pre-line}}
.key{{font-weight:600;color:var(--accent);white-space:nowrap}}
.muted{{color:var(--muted)}}
pre{{margin:0;padding:12px;white-space:pre-wrap;overflow-wrap:anywhere;background:var(--surface);border:1px solid var(--border);border-radius:6px}}
.empty{{color:var(--muted)}}
.generated{{margin-top:56px;font-size:12px;color:var(--muted)}}
.print-only{{display:none}}
@page{{margin:16mm 14mm}}
@media print{{
:root{{{print}color-scheme:light}}
body{{max-width:none;padding:0;font-size:11px}}
code,pre,.mono{{font-size:10px}}
.object,section{{break-before:page}}
h2+.object{{break-before:auto}}
.toc{{break-after:page}}
thead{{display:table-header-group}}
tr,pre{{break-inside:avoid}}
h3,h4{{break-after:avoid}}
.figure{{border:none;overflow:visible}}
a{{color:inherit;text-decoration:none}}
{swap}
}}
"#,
        screen = vars(screen),
        print = vars(print),
        scheme = if dark { "dark" } else { "light" },
        swap = if dark {
            ".screen-only{display:none}.print-only{display:block}"
        } else {
            ""
        },
    )
}

fn palette(colours: &Colours) -> erd::Palette {
    erd::Palette {
        background: colours.background.clone(),
        surface: colours.background.clone(),
        header: colours.header.clone(),
        border: colours.border.clone(),
        text: colours.text.clone(),
        muted: colours.muted.clone(),
        edge: colours.muted.clone(),
    }
}

fn code(text: &str) -> String {
    format!("<code>{}</code>", esc(text))
}

fn names(names: &[impl AsRef<str>]) -> String {
    names
        .iter()
        .map(|name| code(name.as_ref()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A table of rows of cells already written as HTML.
fn grid(out: &mut String, class: &str, header: &[&str], rows: Vec<Vec<String>>) {
    let _ = write!(out, "<table class=\"{class}\"><thead><tr>");
    for cell in header {
        let _ = write!(out, "<th>{cell}</th>");
    }
    out.push_str("</tr></thead><tbody>\n");
    for row in rows {
        out.push_str("<tr>");
        for cell in row {
            out.push_str(&cell);
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</tbody></table>\n");
}

fn td(class: &str, html: String) -> String {
    if class.is_empty() {
        format!("<td>{html}</td>")
    } else {
        format!("<td class=\"{class}\">{html}</td>")
    }
}

fn table_section(out: &mut String, doc: &Document, index: usize, entry: &Table) {
    let Table {
        table,
        kind,
        structure,
    } = entry;
    let _ = write!(
        out,
        "<article class=\"object\" id=\"t{index}\">\n<h3>{}",
        esc(&table.name)
    );
    if let Some(kind) = kind_label(*kind) {
        let _ = write!(out, " <span class=\"kind\">{kind}</span>");
    }
    out.push_str("</h3>\n");
    if let Some(comment) = present(&structure.comment) {
        let _ = writeln!(out, "<p class=\"comment\">{}</p>", esc(comment));
    }
    let rows: Vec<Vec<String>> = column_rows(structure)
        .into_iter()
        .map(|row| {
            vec![
                td("mono", esc(&row.column.name)),
                td("mono", esc(&row.column.data_type)),
                td(
                    "",
                    if row.column.nullable { "yes" } else { "no" }.to_string(),
                ),
                td("mono", row.default.as_deref().map(esc).unwrap_or_default()),
                td("key", row.keys),
                td(
                    "note",
                    present(&row.column.comment).map(esc).unwrap_or_default(),
                ),
            ]
        })
        .collect();
    if rows.is_empty() {
        out.push_str("<p class=\"empty\">No columns.</p>\n");
    } else {
        grid(
            out,
            "columns",
            &["Column", "Type", "Nullable", "Default", "Key", "Comment"],
            rows,
        );
    }
    if let Some((name, columns)) = primary_key(structure) {
        out.push_str("<h4>Primary key</h4>\n<p>");
        if let Some(name) = name {
            let _ = write!(out, "{} ", code(name));
        }
        let _ = writeln!(out, "({})</p>", names(&columns));
    }
    let constraints: Vec<Vec<String>> = structure
        .constraints
        .iter()
        .filter(|constraint| constraint.kind != ConstraintKind::PrimaryKey)
        .map(|constraint| {
            vec![
                td("mono", esc(&constraint.name)),
                td("", constraint_label(constraint.kind).into()),
                td("mono", esc(&constraint.definition)),
            ]
        })
        .collect();
    if !constraints.is_empty() {
        out.push_str("<h4>Constraints</h4>\n");
        grid(
            out,
            "constraints",
            &["Name", "Kind", "Definition"],
            constraints,
        );
    }
    let indexes: Vec<Vec<String>> = structure
        .indexes
        .iter()
        .filter(|index| !index.primary)
        .map(|index| {
            let columns = if index.columns.is_empty() {
                index.definition.as_deref().map(code).unwrap_or_default()
            } else {
                names(&index.columns)
            };
            vec![
                td("mono", esc(&index.name)),
                td("", columns),
                td("", if index.unique { "yes" } else { "no" }.into()),
                td("mono", esc(&index.method)),
            ]
        })
        .collect();
    if !indexes.is_empty() {
        out.push_str("<h4>Indexes</h4>\n");
        grid(
            out,
            "indexes",
            &["Name", "Columns", "Unique", "Method"],
            indexes,
        );
    }
    let keys: Vec<Vec<String>> = structure
        .foreign_keys
        .iter()
        .map(|key| {
            vec![
                td("mono", esc(&key.name)),
                td("", names(&key.columns)),
                td(
                    "",
                    format!(
                        "{} ({})",
                        code(&referenced(doc, &key.referenced_table)),
                        names(&key.referenced_columns)
                    ),
                ),
                td("", esc(&key.on_delete)),
                td("", esc(&key.on_update)),
            ]
        })
        .collect();
    if !keys.is_empty() {
        out.push_str("<h4>Foreign keys</h4>\n");
        grid(
            out,
            "keys",
            &["Name", "Columns", "References", "On delete", "On update"],
            keys,
        );
    }
    out.push_str("</article>\n");
}

fn code_section(out: &mut String, prefix: char, index: usize, object: &Code) {
    let _ = writeln!(
        out,
        "<article class=\"object\" id=\"{prefix}{index}\">\n<h3>{} <span class=\"kind\">{}</span></h3>\n<pre><code>{}</code></pre>\n</article>",
        esc(&object.name),
        object.kind.label(),
        esc(object.ddl.trim_end())
    );
}

/// A diagram's SVG; for the dark style, the light one too, shown on paper.
fn diagram(doc: &Document, screen: &Colours, print: Option<&Colours>) -> String {
    let diagram = doc.diagram();
    let layout = erd::layout(&diagram);
    let mut out = format!(
        "<div class=\"figure{}\">{}</div>",
        if print.is_some() { " screen-only" } else { "" },
        erd::to_svg(&diagram, &layout, &palette(screen))
    );
    if let Some(print) = print {
        let _ = write!(
            out,
            "<div class=\"figure print-only\">{}</div>",
            erd::to_svg(&diagram, &layout, &palette(print))
        );
    }
    out
}

pub(super) fn to_html(doc: &Document, options: &Options) -> String {
    let style = options.style;
    let screen = colours(style);
    let dark = style == Style::KernoDark;
    let print = match style {
        Style::KernoDark => colours(Style::KernoLight),
        other => colours(other),
    };
    let tables = doc.tables();
    let views = doc.views();
    let code_objects = doc.code(&CODE_KINDS);
    let with_diagram = options.diagram && !tables.is_empty();

    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    if style != Style::Plain {
        out.push_str("<meta name=\"generator\" content=\"Kerno\">\n");
    }
    let _ = writeln!(out, "<title>{}</title>", esc(&doc.title));
    let _ = writeln!(out, "<style>\n{}</style>", css(&screen, &print, dark));
    out.push_str("</head>\n<body>\n<header class=\"cover\">\n");
    match style {
        Style::KernoLight => {
            let _ = writeln!(out, "<div class=\"lockup\">{LOCKUP}</div>");
        }
        Style::KernoDark => {
            let _ = writeln!(
                out,
                "<div class=\"lockup screen-only\">{LOCKUP_REVERSED}</div>\n<div class=\"lockup print-only\">{LOCKUP}</div>"
            );
        }
        Style::Plain => {}
    }
    let _ = writeln!(out, "<h1>{}</h1>\n<dl class=\"facts\">", esc(&doc.title));
    for (label, value) in doc.facts() {
        let _ = writeln!(out, "<div><dt>{label}</dt><dd>{}</dd></div>", esc(&value));
    }
    out.push_str("</dl>\n</header>\n");

    // Contents.
    out.push_str("<nav class=\"toc\">\n<h2>Contents</h2>\n<ol>\n");
    if with_diagram {
        out.push_str("<li><a href=\"#diagram\">Diagram</a></li>\n");
    }
    let sections: [(&str, &str, char, &[Code]); 2] = [
        ("views", "Views", 'v', &views),
        (
            "code",
            "Routines, sequences, types and triggers",
            'c',
            &code_objects,
        ),
    ];
    if !tables.is_empty() {
        out.push_str("<li><a href=\"#tables\">Tables</a>\n<ol>\n");
        for (index, entry) in tables.iter().enumerate() {
            let _ = writeln!(
                out,
                "<li><a href=\"#t{index}\">{}</a></li>",
                esc(&entry.table.name)
            );
        }
        out.push_str("</ol></li>\n");
    }
    for (id, heading, prefix, objects) in sections {
        if objects.is_empty() {
            continue;
        }
        let _ = writeln!(out, "<li><a href=\"#{id}\">{heading}</a>\n<ol>");
        for (index, object) in objects.iter().enumerate() {
            let _ = writeln!(
                out,
                "<li><a href=\"#{prefix}{index}\">{}</a> <span class=\"muted\">{}</span></li>",
                esc(&object.name),
                object.kind.label()
            );
        }
        out.push_str("</ol></li>\n");
    }
    out.push_str("</ol>\n");
    if tables.is_empty() && views.is_empty() && code_objects.is_empty() {
        out.push_str("<p class=\"empty\">Nothing was picked: the document lists no object.</p>\n");
    }
    out.push_str("</nav>\n");

    if with_diagram {
        let _ = writeln!(
            out,
            "<section class=\"diagram\" id=\"diagram\">\n<h2>Diagram</h2>\n{}\n</section>",
            diagram(doc, &screen, dark.then_some(&print))
        );
    }
    if !tables.is_empty() {
        out.push_str("<section id=\"tables\">\n<h2>Tables</h2>\n");
        for (index, entry) in tables.iter().enumerate() {
            table_section(&mut out, doc, index, entry);
        }
        out.push_str("</section>\n");
    }
    for (id, heading, prefix, objects) in sections {
        if objects.is_empty() {
            continue;
        }
        let _ = writeln!(out, "<section id=\"{id}\">\n<h2>{heading}</h2>");
        for (index, object) in objects.iter().enumerate() {
            code_section(&mut out, prefix, index, object);
        }
        out.push_str("</section>\n");
    }
    if style != Style::Plain {
        out.push_str("<footer class=\"generated\">Generated by Kerno</footer>\n");
    }
    out.push_str("</body>\n</html>\n");
    out
}

/// The WCAG contrast ratio of two `#rrggbb` colours.
#[cfg(test)]
pub(super) fn contrast(a: &str, b: &str) -> f64 {
    let luminance = |hex: &str| {
        let channel = |at: usize| {
            let value = u8::from_str_radix(&hex[at..at + 2], 16).expect("hex") as f64 / 255.;
            if value <= 0.039_28 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5)
    };
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}
