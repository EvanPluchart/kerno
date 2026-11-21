//! Schema documents (K31) of the fixture schema `shop`, read only: its
//! structure is read, nothing is created or changed, no row is read.
//! Skipped with a message when the `kerno-postgres` container is down.

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use kerno_core::Session;
use kerno_core::schema_doc::{self, Document, Format, Options, Pick, Style};
use kerno_core::testing;

async fn document(session: &Arc<dyn Session>, pick: &Pick) -> Document {
    let side = schema_doc::load(
        session.clone(),
        "shop",
        pick,
        |_| {},
        &AtomicBool::new(false),
    )
    .await
    .unwrap_or_else(|error| panic!("shop: {error}"));
    Document {
        title: schema_doc::default_title("kerno", Some("shop")),
        engine: session.engine(),
        server: schema_doc::server_label(session.engine(), session.server()),
        database: "kerno".into(),
        schema: Some("shop".into()),
        date: schema_doc::today(),
        side,
    }
}

fn rendered(doc: &Document, format: Format, session: &Arc<dyn Session>) -> String {
    schema_doc::render(doc, format, &Options::default(), session.dialect())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_document_names_the_tables_there_and_only_those_picked() {
    let Some(session) = common::session().await else {
        return;
    };
    let objects = session.objects("shop").await.unwrap();
    let all: BTreeSet<String> = schema_doc::pickable(&objects)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    for name in [
        "customers",
        "orders",
        "order_items",
        "order_totals",
        "product_sales",
    ] {
        assert!(all.contains(name), "{name}: {all:?}");
    }
    let whole = document(
        &session,
        &Pick {
            relations: all.clone(),
            code: true,
        },
    )
    .await;
    let tables = [
        "customers",
        "categories",
        "products",
        "orders",
        "order_items",
        "reviews",
    ];
    for format in Format::ALL {
        let text = rendered(&whole, format, &session);
        for table in tables {
            assert!(text.contains(table), "{format:?}: {table}");
        }
        // Views are written where the format has a place for them.
        if matches!(format, Format::Html | Format::Markdown | Format::Sql) {
            assert!(text.contains("order_totals"), "{format:?}");
            assert!(text.contains("product_sales"), "{format:?}");
        }
        // Nothing of the connection: no host, no port, no password.
        let params = testing::params(session.engine());
        for secret in [params.host.as_str(), "55432", "kerno_test_pw"] {
            assert!(!text.contains(secret), "{format:?}: {secret}");
        }
        if let Some(dir) = std::env::var_os("K31_SAMPLES") {
            let path =
                std::path::Path::new(&dir).join(format!("postgres-shop.{}", format.extension()));
            std::fs::write(path, &text).unwrap();
            let styles = if format == Format::Html {
                vec![(Style::KernoDark, "dark"), (Style::Plain, "plain")]
            } else {
                Vec::new()
            };
            for (style, name) in styles {
                let options = Options {
                    style,
                    ..Options::default()
                };
                let page = schema_doc::render(&whole, Format::Html, &options, session.dialect());
                let path = std::path::Path::new(&dir).join(format!("postgres-shop-{name}.html"));
                std::fs::write(path, page).unwrap();
            }
        }
    }
    let sql = rendered(&whole, Format::Sql, &session);
    // The keys come after both tables of each.
    let created = |table: &str| {
        sql.find(&format!("CREATE TABLE \"shop\".\"{table}\""))
            .unwrap()
    };
    let key = sql.find("FOREIGN KEY").unwrap();
    assert!(created("orders") < key && created("customers") < key);

    // A subset holds only what was picked.
    let picked = document(
        &session,
        &Pick {
            relations: ["customers", "orders"].map(String::from).into(),
            code: false,
        },
    )
    .await;
    let names: Vec<&str> = picked
        .side
        .objects
        .iter()
        .map(|object| object.object.name())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.contains(&"customers") && names.contains(&"orders"));
    let html = rendered(&picked, Format::Html, &session);
    assert!(html.contains("<h3>orders</h3>") && html.contains("<h3>customers</h3>"));
    for left_out in ["<h3>reviews", "<h3>order_items", "order_totals"] {
        assert!(!html.contains(left_out), "{left_out}");
    }
    session.close().await;
}
