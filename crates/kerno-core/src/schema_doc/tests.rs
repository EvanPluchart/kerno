//! K31: each format on a small fixed schema (golden text), a hostile schema
//! (names, comments and defaults that try to leave their place), the three
//! HTML styles and their contrast.

use super::html::{contrast, esc};
use super::*;
use crate::compare::Object;
use crate::dialect::test_support::AnsiDialect;
use crate::result::TypeKind;
use crate::schema::{ConstraintInfo, ForeignKeyInfo, IndexInfo, RoutineKind};

pub(super) fn column(name: &str, data_type: &str, nullable: bool) -> ColumnInfo {
    ColumnInfo {
        name: name.into(),
        data_type: data_type.into(),
        kind: TypeKind::Text,
        nullable,
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

pub(super) fn table(name: &str, structure: TableStructure) -> Object {
    Object {
        object: ObjectRef::Relation(TableRef::new("shop", name), RelationKind::Table),
        definition: Definition::Table(structure),
    }
}

fn ddl(object: ObjectRef, text: &str) -> Object {
    Object {
        object,
        definition: Definition::Ddl(text.into()),
    }
}

/// Customers and their orders: an identity key, a unique email, a key
/// with ON DELETE CASCADE, a default string and a check, an index, a view
/// and a function.
pub(super) fn shop() -> Document {
    let customers = TableStructure {
        columns: vec![
            ColumnInfo {
                primary_key_position: Some(1),
                generation: Some(ColumnGeneration::IdentityByDefault),
                ..column("id", "bigint", false)
            },
            column("email", "text", false),
            ColumnInfo {
                comment: Some("As printed on invoices".into()),
                ..column("name", "character varying(80)", true)
            },
        ],
        constraints: vec![
            ConstraintInfo {
                name: "customers_pkey".into(),
                kind: ConstraintKind::PrimaryKey,
                columns: vec!["id".into()],
                definition: "PRIMARY KEY (id)".into(),
            },
            ConstraintInfo {
                name: "customers_email_key".into(),
                kind: ConstraintKind::Unique,
                columns: vec!["email".into()],
                definition: "UNIQUE (email)".into(),
            },
        ],
        comment: Some("Who buys".into()),
        ..Default::default()
    };
    let orders = TableStructure {
        columns: vec![
            ColumnInfo {
                primary_key_position: Some(1),
                ..column("id", "bigint", false)
            },
            column("customer_id", "bigint", false),
            ColumnInfo {
                default: Some("0".into()),
                ..column("total", "numeric(10,2)", false)
            },
            ColumnInfo {
                default: Some("'new'::text".into()),
                ..column("status", "text", false)
            },
            ColumnInfo {
                default: Some("now()".into()),
                ..column("placed_at", "timestamp with time zone", true)
            },
        ],
        constraints: vec![
            ConstraintInfo {
                name: "orders_pkey".into(),
                kind: ConstraintKind::PrimaryKey,
                columns: vec!["id".into()],
                definition: "PRIMARY KEY (id)".into(),
            },
            ConstraintInfo {
                name: "orders_total_check".into(),
                kind: ConstraintKind::Check,
                columns: vec!["total".into()],
                definition: "CHECK ((total >= (0)::numeric))".into(),
            },
        ],
        indexes: vec![IndexInfo {
            name: "orders_customer_idx".into(),
            columns: vec!["customer_id".into()],
            unique: false,
            primary: false,
            backs_constraint: false,
            method: "btree".into(),
            predicate: None,
            definition: Some(
                "CREATE INDEX orders_customer_idx ON shop.orders USING btree (customer_id)".into(),
            ),
        }],
        foreign_keys: vec![ForeignKeyInfo {
            name: "orders_customer_fk".into(),
            table: TableRef::new("shop", "orders"),
            columns: vec!["customer_id".into()],
            referenced_table: TableRef::new("shop", "customers"),
            referenced_columns: vec!["id".into()],
            on_update: "NO ACTION".into(),
            on_delete: "CASCADE".into(),
        }],
        ..Default::default()
    };
    Document {
        title: "kerno.shop".into(),
        engine: Engine::Postgres,
        server: "PostgreSQL 17.2".into(),
        database: "kerno".into(),
        schema: Some("shop".into()),
        date: "2026-11-21".into(),
        side: Side {
            schema: "shop".into(),
            objects: vec![
                table("orders", orders),
                table("customers", customers),
                ddl(
                    ObjectRef::Relation(TableRef::new("shop", "big_orders"), RelationKind::View),
                    "CREATE OR REPLACE VIEW big_orders AS\n SELECT id FROM orders WHERE total > 100;",
                ),
                ddl(
                    ObjectRef::Routine {
                        schema: "shop".into(),
                        name: "order_count".into(),
                        kind: RoutineKind::Function,
                        identity_arguments: "bigint".into(),
                    },
                    "CREATE FUNCTION order_count(bigint) RETURNS bigint\nLANGUAGE sql AS $$ SELECT count(*) FROM orders WHERE customer_id = $1 $$;",
                ),
            ],
        },
    }
}

/// A table named to close an HTML attribute and open a script, a comment
/// that ends its line and starts a statement, a column whose name holds a
/// pipe, a backtick and a double quote, a default that holds a backtick.
const HOSTILE_TABLE: &str = "x\"><script>alert(1)</script>";
const HOSTILE_COMMENT: &str = "ok\nDROP TABLE kerno_test_victim;";
const HOSTILE_COLUMN: &str = "a|b`c\"d";

pub(super) fn hostile() -> Document {
    let structure = TableStructure {
        columns: vec![
            ColumnInfo {
                comment: Some(format!(
                    "{HOSTILE_COMMENT}\n| x | [link](https://evil.test) ![i](https://evil.test/i.png)"
                )),
                default: Some("'it''s `here`'".into()),
                ..column(HOSTILE_COLUMN, "text", true)
            },
            ColumnInfo {
                default: Some("x`)\nDROP TABLE kerno_test_victim; --".into()),
                ..column("second", "int", true)
            },
        ],
        comment: Some(format!(
            "{HOSTILE_COMMENT}\n# heading\n<img src=x onerror=alert(1)>"
        )),
        ..Default::default()
    };
    Document {
        title: format!("{HOSTILE_TABLE}\nDROP TABLE kerno_test_victim;"),
        side: Side {
            schema: "shop".into(),
            objects: vec![
                table(HOSTILE_TABLE, structure),
                ddl(
                    ObjectRef::Relation(TableRef::new("shop", "v```"), RelationKind::View),
                    "CREATE VIEW \"v```\" AS SELECT '\n```\n# out' AS x;",
                ),
            ],
        },
        ..shop()
    }
}

/// The fonts the app embeds, read from its assets.
pub(super) fn fonts() -> Fonts<'static> {
    static DATA: std::sync::OnceLock<[Vec<u8>; 3]> = std::sync::OnceLock::new();
    let data = DATA.get_or_init(|| {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../kerno/assets/fonts");
        let read = |file: &str| std::fs::read(dir.join(file)).expect("the app's font");
        [
            read("inter/Inter-Regular.ttf"),
            read("inter/Inter-SemiBold.ttf"),
            read("jetbrains-mono/JetBrainsMono-Regular.ttf"),
        ]
    });
    Fonts {
        regular: &data[0],
        semibold: &data[1],
        mono: &data[2],
    }
}

fn text_of(doc: &Document, format: Format, options: &Options) -> String {
    let bytes = render(doc, format, options, &AnsiDialect, &fonts()).expect("written");
    String::from_utf8(bytes).expect("text")
}

fn rendered(doc: &Document, format: Format) -> String {
    text_of(doc, format, &Options::default())
}

fn html(doc: &Document, style: Style) -> String {
    text_of(
        doc,
        Format::Html,
        &Options {
            style,
            ..Options::default()
        },
    )
}

// Golden text -------------------------------------------------------------------

#[test]
fn dbml_of_a_small_schema() {
    let expected = r#"Project "kerno.shop" {
  database_type: 'PostgreSQL'
  Note: '''
Server: PostgreSQL 17.2
Database: kerno
Schema: shop
Date: 2026-11-21
Contents: 2 tables, 1 view and 1 function
'''
}

Table shop.customers {
  id bigint [pk, increment]
  email text [not null, unique]
  name "character varying(80)" [note: 'As printed on invoices']

  Note: 'Who buys'
}

Table shop.orders {
  id bigint [pk]
  customer_id bigint [not null]
  total numeric(10,2) [not null, default: 0]
  status text [not null, default: 'new']
  placed_at "timestamp with time zone" [default: `now()`]

  indexes {
    customer_id [name: 'orders_customer_idx', type: btree]
  }
}

Ref orders_customer_fk: shop.orders.customer_id > shop.customers.id [delete: cascade]
"#;
    assert_eq!(rendered(&shop(), Format::Dbml), expected);
}

#[test]
fn markdown_of_a_small_schema() {
    let expected = r#"# kerno.shop

- **Server:** PostgreSQL 17.2
- **Database:** `kerno`
- **Schema:** `shop`
- **Date:** 2026-11-21
- **Contents:** 2 tables, 1 view and 1 function

## Tables

### `customers`

Who buys

| Column | Type | Nullable | Default | Key | Comment |
|---|---|---|---|---|---|
| `id` | `bigint` | no | `identity` | PK |  |
| `email` | `text` | no |  | UQ |  |
| `name` | `character varying(80)` | yes |  |  | As printed on invoices |

**Primary key** `customers_pkey`: `id`

**Constraints**

| Name | Kind | Definition |
|---|---|---|
| `customers_email_key` | unique | `UNIQUE (email)` |

### `orders`

| Column | Type | Nullable | Default | Key | Comment |
|---|---|---|---|---|---|
| `id` | `bigint` | no |  | PK |  |
| `customer_id` | `bigint` | no |  | FK |  |
| `total` | `numeric(10,2)` | no | `0` |  |  |
| `status` | `text` | no | `'new'::text` |  |  |
| `placed_at` | `timestamp with time zone` | yes | `now()` |  |  |

**Primary key** `orders_pkey`: `id`

**Constraints**

| Name | Kind | Definition |
|---|---|---|
| `orders_total_check` | check | `CHECK ((total >= (0)::numeric))` |

**Indexes**

| Name | Columns | Unique | Method |
|---|---|---|---|
| `orders_customer_idx` | `customer_id` | no | `btree` |

**Foreign keys**

| Name | Columns | References | On delete | On update |
|---|---|---|---|---|
| `orders_customer_fk` | `customer_id` | `customers` (`id`) | CASCADE | NO ACTION |

## Views

### `big_orders` (view)

```sql
CREATE OR REPLACE VIEW big_orders AS
 SELECT id FROM orders WHERE total > 100;
```

## Routines, sequences, types and triggers

### `order_count(bigint)` (function)

```sql
CREATE FUNCTION order_count(bigint) RETURNS bigint
LANGUAGE sql AS $$ SELECT count(*) FROM orders WHERE customer_id = $1 $$;
```
"#;
    assert_eq!(rendered(&shop(), Format::Markdown), expected);
}

#[test]
fn sql_of_a_small_schema_runs_in_order() {
    // Tables without their keys first, then the keys, the routines, the
    // views: the compare's script against an empty schema.
    let expected = r#"-- kerno.shop
-- Server: PostgreSQL 17.2
-- Database: kerno
-- Schema: shop
-- Date: 2026-11-21
-- Contents: 2 tables, 1 view and 1 function

-- table customers
CREATE TABLE "shop"."customers" (
    "id" bigint GENERATED BY DEFAULT AS IDENTITY NOT NULL,
    "email" text NOT NULL,
    "name" character varying(80),
    PRIMARY KEY ("id")
);

COMMENT ON TABLE "shop"."customers" IS 'Who buys';

COMMENT ON COLUMN "shop"."customers"."name" IS 'As printed on invoices';

-- table orders
CREATE TABLE "shop"."orders" (
    "id" bigint NOT NULL,
    "customer_id" bigint NOT NULL,
    "total" numeric(10,2) DEFAULT 0 NOT NULL,
    "status" text DEFAULT 'new'::text NOT NULL,
    "placed_at" timestamp with time zone DEFAULT now(),
    PRIMARY KEY ("id")
);

CREATE INDEX "orders_customer_idx" ON "shop"."orders" ("customer_id");

-- table customers
ALTER TABLE "shop"."customers" ADD CONSTRAINT "customers_email_key" UNIQUE (email);

-- table orders
ALTER TABLE "shop"."orders" ADD CONSTRAINT "orders_total_check" CHECK ((total >= (0)::numeric));

-- table orders
ALTER TABLE "shop"."orders" ADD CONSTRAINT "orders_customer_fk" FOREIGN KEY ("customer_id") REFERENCES "shop"."customers" ("id") ON UPDATE NO ACTION ON DELETE CASCADE;

-- function order_count(bigint)
CREATE FUNCTION order_count(bigint) RETURNS bigint
LANGUAGE sql AS $$ SELECT count(*) FROM orders WHERE customer_id = $1 $$;

-- view big_orders
CREATE OR REPLACE VIEW big_orders AS
 SELECT id FROM orders WHERE total > 100;
"#;
    assert_eq!(rendered(&shop(), Format::Sql), expected);
}

#[test]
fn mermaid_of_a_small_schema() {
    let expected = r#"erDiagram
    "shop.customers" {
        bigint id PK
        text email
        character_varying(80) name
    }
    "shop.orders" {
        bigint id PK
        bigint customer_id FK
        numeric(10_2) total
        text status
        timestamp_with_time_zone placed_at
    }
    "shop.customers" ||--o{ "shop.orders" : "orders_customer_fk"
"#;
    assert_eq!(rendered(&shop(), Format::Mermaid), expected);
}

#[test]
fn html_of_a_table() {
    let expected = r#"<article class="object" id="t1">
<h3>orders</h3>
<table class="columns"><thead><tr><th>Column</th><th>Type</th><th>Nullable</th><th>Default</th><th>Key</th><th>Comment</th></tr></thead><tbody>
<tr><td class="mono">id</td><td class="mono">bigint</td><td>no</td><td class="mono"></td><td class="key">PK</td><td class="note"></td></tr>
<tr><td class="mono">customer_id</td><td class="mono">bigint</td><td>no</td><td class="mono"></td><td class="key">FK</td><td class="note"></td></tr>
<tr><td class="mono">total</td><td class="mono">numeric(10,2)</td><td>no</td><td class="mono">0</td><td class="key"></td><td class="note"></td></tr>
<tr><td class="mono">status</td><td class="mono">text</td><td>no</td><td class="mono">&#39;new&#39;::text</td><td class="key"></td><td class="note"></td></tr>
<tr><td class="mono">placed_at</td><td class="mono">timestamp with time zone</td><td>yes</td><td class="mono">now()</td><td class="key"></td><td class="note"></td></tr>
</tbody></table>
<h4>Primary key</h4>
<p><code>orders_pkey</code> (<code>id</code>)</p>
<h4>Constraints</h4>
<table class="constraints"><thead><tr><th>Name</th><th>Kind</th><th>Definition</th></tr></thead><tbody>
<tr><td class="mono">orders_total_check</td><td>check</td><td class="mono">CHECK ((total &gt;= (0)::numeric))</td></tr>
</tbody></table>
<h4>Indexes</h4>
<table class="indexes"><thead><tr><th>Name</th><th>Columns</th><th>Unique</th><th>Method</th></tr></thead><tbody>
<tr><td class="mono">orders_customer_idx</td><td><code>customer_id</code></td><td>no</td><td class="mono">btree</td></tr>
</tbody></table>
<h4>Foreign keys</h4>
<table class="keys"><thead><tr><th>Name</th><th>Columns</th><th>References</th><th>On delete</th><th>On update</th></tr></thead><tbody>
<tr><td class="mono">orders_customer_fk</td><td><code>customer_id</code></td><td><code>customers</code> (<code>id</code>)</td><td>CASCADE</td><td>NO ACTION</td></tr>
</tbody></table>
</article>
"#;
    let page = html(&shop(), Style::KernoLight);
    let start = page.find("<article class=\"object\" id=\"t1\">").unwrap();
    let end = start + page[start..].find("</article>\n").unwrap() + "</article>\n".len();
    assert_eq!(&page[start..end], expected);
    // The contents link each object by number, the diagram comes first.
    for anchor in [
        "#diagram", "#tables", "#t0", "#t1", "#views", "#v0", "#code", "#c0",
    ] {
        assert!(page.contains(&format!("<a href=\"{anchor}\">")), "{anchor}");
    }
    assert!(page.contains("<title>kerno.shop</title>"));
    assert!(page.find("id=\"diagram\"").unwrap() < page.find("id=\"tables\"").unwrap());
}

// The HTML styles ---------------------------------------------------------------

#[test]
fn the_html_asks_the_network_for_nothing() {
    for style in Style::ALL {
        let page = html(&shop(), style);
        for asked in [
            "<script", "<link", "@import", "url(", "src=", "<iframe", "<object",
        ] {
            assert!(!page.contains(asked), "{style:?}: {asked}");
        }
        // The SVG namespace is a name, not a request.
        let urls = page.matches("http").count();
        assert_eq!(
            urls,
            page.matches("http://www.w3.org/2000/svg").count(),
            "{style:?}"
        );
        assert!(page.starts_with("<!DOCTYPE html>\n<html lang=\"en\">"));
        assert!(page.contains("thead{display:table-header-group}"));
        assert!(page.contains(".object,section{break-before:page}"));
    }
}

#[test]
fn the_three_styles() {
    let light = html(&shop(), Style::KernoLight);
    let dark = html(&shop(), Style::KernoDark);
    let plain = html(&shop(), Style::Plain);
    let tokens = |style| colours(style);
    // The Kerno styles take the theme's tokens: the light olive accent,
    // the dark lime one, the lockup and the footer line.
    assert!(light.contains(&format!("--accent:{};", tokens(Style::KernoLight).accent)));
    assert_eq!(tokens(Style::KernoLight).accent, "#4d6b00");
    assert_eq!(tokens(Style::KernoDark).accent, "#c8f03c");
    assert_eq!(tokens(Style::KernoDark).background, "#14171a");
    for page in [&light, &dark] {
        assert!(page.contains("<title>Kerno</title>"), "the lockup");
        assert!(page.contains("<footer class=\"generated\">Generated by Kerno</footer>"));
    }
    assert!(light.contains("color-scheme:light}"));
    // Dark on screen, the light theme on paper, its diagram and lockup too.
    assert!(dark.contains(&format!("--bg:{};", tokens(Style::KernoDark).background)));
    let print = &dark[dark.find("@media print").unwrap()..];
    assert!(print.contains(&format!("--bg:{};", tokens(Style::KernoLight).background)));
    assert!(print.contains(".screen-only{display:none}.print-only{display:block}"));
    assert_eq!(
        dark.matches("<div class=\"figure screen-only\">").count(),
        1
    );
    assert_eq!(dark.matches("<div class=\"figure print-only\">").count(), 1);
    assert!(dark.contains("<div class=\"lockup screen-only\">"));
    // Plain: black on white, no brand colour, no logo, no line about Kerno.
    assert!(!plain.contains("Kerno"), "{plain}");
    for brand in ["#c8f03c", "#4d6b00", "#f1f8d6", "#111111"] {
        assert!(!plain.to_ascii_lowercase().contains(brand), "{brand}");
    }
    assert!(plain.contains("--bg:#ffffff;") && plain.contains("--text:#000000;"));
}

#[test]
fn every_style_keeps_text_readable() {
    // WCAG AA: 4.5:1 for text on the page and on table headers and code. The PDF
    // draws the same pairs: text, muted text and the accent on its pages, muted
    // labels and code on the surface, the diagram's headers.
    for style in Style::ALL {
        let c = colours(style);
        for (what, colour) in [
            ("text", &c.text),
            ("muted", &c.muted),
            ("accent", &c.accent),
        ] {
            for (on, background) in [("background", &c.background), ("surface", &c.surface)] {
                let ratio = contrast(colour, background);
                assert!(ratio >= 4.5, "{style:?}: {what} on {on} is {ratio:.2}:1");
            }
        }
        // The diagram's table headers.
        assert!(contrast(&c.text, &c.header) >= 4.5, "{style:?}");
        assert!(contrast(&c.muted, &c.header) >= 4.5, "{style:?}");
    }
}

#[test]
fn the_diagram_is_left_out_when_not_asked_or_without_tables() {
    let without = text_of(
        &shop(),
        Format::Html,
        &Options {
            diagram: false,
            ..Options::default()
        },
    );
    assert!(!without.contains("<svg xmlns=\"http://www.w3.org/2000/svg\" width="));
    assert!(!without.contains("href=\"#diagram\""));
    let empty = Document {
        side: Side {
            schema: "shop".into(),
            objects: Vec::new(),
        },
        ..shop()
    };
    let page = html(&empty, Style::KernoLight);
    assert!(!page.contains("id=\"diagram\""));
    assert!(page.contains("Nothing was picked"));
    assert!(page.contains("<dd>nothing</dd>"));
    assert!(rendered(&empty, Format::Sql).ends_with("-- Nothing was picked.\n"));
    assert!(!rendered(&empty, Format::Dbml).contains("Table "));
}

// The hostile schema ------------------------------------------------------------

#[test]
fn html_escapes_every_name_comment_and_definition() {
    for style in Style::ALL {
        let page = html(&hostile(), style);
        let lower = page.to_ascii_lowercase();
        assert!(!lower.contains("<script"), "{style:?}");
        assert!(!lower.contains("<img"), "{style:?}");
        assert!(!page.contains(HOSTILE_TABLE), "{style:?}");
        assert!(!page.contains(HOSTILE_COLUMN), "{style:?}");
        assert!(page.contains("x&quot;&gt;&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(page.contains("a|b`c&quot;d"));
        // The comment keeps its lines as text of its paragraph.
        assert!(page.contains("<p class=\"comment\">ok\nDROP TABLE kerno_test_victim;"));
    }
    assert_eq!(esc("<a href='x'>&"), "&lt;a href=&#39;x&#39;&gt;&amp;");
}

/// The cells of a Markdown table row, split on the pipes that are not
/// escaped, as GFM splits them.
fn cells(line: &str) -> Vec<String> {
    let mut cells = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let last = cells.last_mut().expect("a cell");
                last.push(c);
                if let Some(next) = chars.next() {
                    last.push(next);
                }
            }
            '|' => cells.push(String::new()),
            c => cells.last_mut().expect("a cell").push(c),
        }
    }
    cells
}

#[test]
fn markdown_keeps_hostile_text_in_its_cell_and_its_block() {
    let text = rendered(&hostile(), Format::Markdown);
    // No line starts what the comment holds: a heading, an image tag.
    for line in text.lines() {
        assert!(!line.starts_with("# heading"), "{line}");
        assert!(!line.starts_with("<img"), "{line}");
    }
    // Each column is one row of six cells.
    let rows: Vec<&str> = text
        .lines()
        .skip_while(|line| !line.starts_with("| Column |"))
        .skip(2)
        .take_while(|line| line.starts_with('|'))
        .collect();
    assert_eq!(rows.len(), 2, "{text}");
    for row in &rows {
        assert_eq!(cells(row).len(), 8, "{row}");
    }
    // No link, image or HTML survives.
    assert!(!text.contains("[link]("));
    assert!(!text.contains("![i]("));
    assert_eq!(text.matches("<img").count(), text.matches(r"\<img").count());
    // The table's name is code in its heading, nowhere raw.
    let raw = |line: &&str| line.matches("<script").count() > line.matches(r"\<script").count();
    for line in text.lines().filter(raw) {
        assert_eq!(line, "### `x\"><script>alert(1)</script>`");
    }
    assert!(text.contains(r"\[link\](https\://evil.test)"));
    // A definition holding a fence of three backticks sits in a fence of
    // four: its `# out` line stays inside.
    let block = text.find("````sql\n").expect("a longer fence");
    let closed = block + 8 + text[block + 8..].find("\n````").unwrap();
    assert!(text[block..closed].contains("\n```\n# out"));
}

#[test]
fn markdown_escapes_what_would_start_markup() {
    assert_eq!(
        md_text("a_b *c* `d` <e> [f](g)", false),
        r"a\_b \*c\* \`d\` \<e\> \[f\](g)"
    );
    assert_eq!(md_text("# title", false), r"\# title");
    assert_eq!(
        md_text("- item\n1. one\n2) two", false),
        "\\- item\\\n1\\. one\\\n2\\) two"
    );
    assert_eq!(
        md_text("see www.evil.test or a@b.c", false),
        r"see www\.evil.test or a\@b.c"
    );
    assert_eq!(md_text("x | y\nz", true), r"x \| y<br>z");
    assert_eq!(md_code("a`b", false), "``a`b``");
    assert_eq!(md_code("`a", false), "`` `a ``");
    assert_eq!(md_code("a|b", true), r"a\|b");
    assert_eq!(md_code("a\\|b", true), r"a\\\|b");
    assert_eq!(cells(&format!("| {} |", md_code("a\\|b", true))).len(), 3);
}

/// Where a DBML reader is at each byte: outside, in a `'…'` or `'''…'''`
/// string, in a `"…"` name, in a `` `…` `` expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum In {
    Outside,
    String,
    Multi,
    Name,
    Expression,
}

/// The state of a DBML reader before each byte, and at the end.
fn dbml_states(text: &str) -> (Vec<In>, In) {
    let bytes = text.as_bytes();
    let mut states = Vec::with_capacity(bytes.len());
    let mut state = In::Outside;
    let mut at = 0;
    while at < bytes.len() {
        states.push(state);
        let rest = &text[at..];
        let step = match (state, bytes[at]) {
            (In::Outside, b'\'') if rest.starts_with("'''") => {
                state = In::Multi;
                3
            }
            (In::Outside, b'\'') => {
                state = In::String;
                1
            }
            (In::Outside, b'"') => {
                state = In::Name;
                1
            }
            (In::Outside, b'`') => {
                state = In::Expression;
                1
            }
            (In::String | In::Multi | In::Name, b'\\') => 2,
            (In::Multi, b'\'') if rest.starts_with("'''") => {
                state = In::Outside;
                3
            }
            (In::String, b'\'') | (In::Name, b'"') | (In::Expression, b'`') => {
                state = In::Outside;
                1
            }
            (In::String | In::Name, b'\n') => panic!("a line break at {at} in {state:?}"),
            _ => 1,
        };
        for _ in 1..step.min(bytes.len() - at) {
            states.push(state);
        }
        at += step;
    }
    (states, state)
}

#[test]
fn dbml_quotes_names_and_escapes_strings() {
    let text = rendered(&hostile(), Format::Dbml);
    let (states, end) = dbml_states(&text);
    assert_eq!(end, In::Outside, "{text}");
    // The statement of the comment and of the default is in a string.
    for (at, _) in text.match_indices("DROP TABLE kerno_test_victim") {
        assert!(
            matches!(states[at], In::String | In::Multi | In::Name),
            "at {at}: {:?}\n{text}",
            states[at]
        );
    }
    // Braces outside strings balance.
    let depth = text
        .bytes()
        .zip(&states)
        .filter(|(_, state)| **state == In::Outside)
        .fold(0i32, |depth, (byte, _)| match byte {
            b'{' => depth + 1,
            b'}' => depth - 1,
            _ => depth,
        });
    assert_eq!(depth, 0);
    assert!(text.contains(r#"Table shop."x\"><script>alert(1)</script>" {"#));
    assert!(text.contains(r#""a|b`c\"d" text [default: 'it\'s `here`'"#));
    assert_eq!(dbml_name("note"), "\"note\"");
    assert_eq!(dbml_name("a\\b"), r#""a\\b""#);
    assert_eq!(dbml_string("it's"), r"'it\'s'");
    assert_eq!(dbml_string("a\nb'''c"), "'''\na\nb\\'\\'\\'c\n'''");
    assert_eq!(dbml_default("now()"), "`now()`");
    assert_eq!(dbml_default("'a''b'::text"), r"'a\'b'");
    assert_eq!(dbml_default("-1.5"), "-1.5");
    assert_eq!(dbml_default("NULL"), "null");
}

#[test]
fn sql_comments_never_end_early() {
    // The server's default is an expression: SQL by nature, written as
    // the dialect writes it. What must not run is what a name or a comment
    // holds.
    let mut doc = hostile();
    if let Definition::Table(structure) = &mut doc.side.objects[0].definition {
        structure.columns[1].default = None;
    }
    let text = rendered(&doc, Format::Sql);
    let header: Vec<&str> = text.lines().take_while(|line| !line.is_empty()).collect();
    assert!(header.len() >= 6);
    assert!(header.iter().all(|line| line.starts_with("-- ")), "{text}");
    // Every DROP of the comments is inside a string literal: an odd number
    // of quotes comes before it.
    // The title's is on a comment line.
    let drops: Vec<usize> = text
        .match_indices("DROP TABLE kerno_test_victim")
        .map(|(at, _)| at)
        .filter(|at| {
            !text[..*at]
                .rsplit('\n')
                .next()
                .unwrap_or_default()
                .starts_with("--")
        })
        .collect();
    assert_eq!(drops.len(), 2, "{text}");
    for at in drops {
        let before: usize = text[..at]
            .split('\n')
            .filter(|line| !line.starts_with("--"))
            .map(|line| line.matches('\'').count())
            .sum();
        assert_eq!(before % 2, 1, "at {at}\n{text}");
    }
    assert!(text.contains(r#"CREATE TABLE "shop"."x""><script>alert(1)</script>" ("#));
}

#[test]
fn mermaid_names_stay_on_their_line() {
    let doc = Document {
        side: Side {
            schema: "shop".into(),
            objects: vec![table(
                "t\nDROP TABLE kerno_test_victim;",
                TableStructure {
                    columns: vec![column("c\"\n}", "text", true)],
                    ..Default::default()
                },
            )],
        },
        ..shop()
    };
    let text = rendered(&doc, Format::Mermaid);
    assert_eq!(text.lines().count(), 4, "{text}");
    assert!(text.lines().all(|line| !line.starts_with("DROP")));
    assert!(text.contains("\"shop.t DROP TABLE kerno_test_victim;\" {"));
}

// The rest ----------------------------------------------------------------------

#[test]
fn names_of_the_file_the_title_and_the_server() {
    assert_eq!(file_name("shop", Format::Html), "shop-schema.html");
    assert_eq!(file_name("../a b", Format::Dbml), "_a_b-schema.dbml");
    assert_eq!(file_name("", Format::Markdown), "schema-schema.md");
    assert_eq!(default_title("kerno", Some("shop")), "kerno.shop");
    assert_eq!(default_title("main", None), "main");
    let server = ServerInfo {
        version: "PostgreSQL 17.2 on aarch64-unknown-linux-musl, compiled by gcc".into(),
        version_number: (17, 2, 0),
    };
    assert_eq!(server_label(Engine::Postgres, &server), "PostgreSQL 17.2");
    let today = today();
    assert_eq!(today.len(), 10);
    assert_eq!(&today[4..5], "-");
}

#[test]
fn the_tables_and_views_a_document_can_pick() {
    use crate::schema::RelationInfo;
    let relation = |name: &str, kind, is_partition| RelationInfo {
        name: name.into(),
        kind,
        row_estimate: None,
        size_bytes: None,
        comment: None,
        engine: None,
        is_partition,
    };
    let objects = SchemaObjects {
        relations: vec![
            relation("orders", RelationKind::PartitionedTable, false),
            relation("orders_2026", RelationKind::Table, true),
            relation("remote", RelationKind::ForeignTable, false),
            relation("big_orders", RelationKind::View, false),
            relation("totals", RelationKind::MaterializedView, false),
        ],
        ..Default::default()
    };
    assert_eq!(
        pickable(&objects),
        [
            ("orders".to_string(), RelationKind::PartitionedTable),
            ("big_orders".to_string(), RelationKind::View),
            ("totals".to_string(), RelationKind::MaterializedView),
        ]
    );
}

#[test]
fn a_wide_table_and_long_text_stay_in_their_cells() {
    // Sixty columns, a long type and a long default: one row each, the
    // text whole (the page wraps it, the CSS breaks long words).
    let long = "x".repeat(300);
    let structure = TableStructure {
        columns: (0..60)
            .map(|index| ColumnInfo {
                default: Some(long.clone()),
                ..column(&format!("c{index}"), &long, true)
            })
            .collect(),
        ..Default::default()
    };
    let doc = Document {
        side: Side {
            schema: "shop".into(),
            objects: vec![table("wide", structure)],
        },
        ..shop()
    };
    let page = html(&doc, Style::KernoLight);
    assert_eq!(page.matches("<tr><td class=\"mono\">c").count(), 60);
    assert!(page.contains("overflow-wrap:anywhere"));
    let markdown = rendered(&doc, Format::Markdown);
    assert_eq!(
        markdown
            .lines()
            .filter(|line| line.starts_with("| `c"))
            .count(),
        60
    );
}
