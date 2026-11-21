//! Schema documents (K31) of the fixture database `kerno` on MySQL and
//! MariaDB, read only: its structure is read, nothing is created or
//! changed, no row is read. Skipped with a message when the containers are
//! down.

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use common::engines;
use kerno_core::Session;
use kerno_core::schema_doc::{self, Document, Fonts, Format, Options, Pick};
use kerno_core::testing;

/// The fonts the app embeds, read from its assets.
fn fonts() -> Fonts<'static> {
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

fn rendered(doc: &Document, format: Format, session: &Arc<dyn Session>) -> String {
    let bytes = schema_doc::render(
        doc,
        format,
        &Options::default(),
        session.dialect(),
        &fonts(),
    )
    .expect("written");
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn document(session: &Arc<dyn Session>, pick: &Pick) -> Document {
    let side = schema_doc::load(
        session.clone(),
        "kerno",
        pick,
        |_| {},
        &AtomicBool::new(false),
    )
    .await
    .unwrap_or_else(|error| panic!("kerno: {error}"));
    Document {
        title: schema_doc::default_title("kerno", None),
        engine: session.engine(),
        server: schema_doc::server_label(session.engine(), session.server()),
        database: "kerno".into(),
        schema: None,
        date: schema_doc::today(),
        side,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_document_names_the_tables_there_and_only_those_picked() {
    for engine in engines() {
        let session = common::session(engine).await;
        let objects = session.objects("kerno").await.unwrap();
        let all: BTreeSet<String> = schema_doc::pickable(&objects)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let whole = document(
            &session,
            &Pick {
                relations: all.clone(),
                code: true,
            },
        )
        .await;
        let params = testing::params(engine);
        let port = params.port.to_string();
        for format in Format::ALL {
            let text = rendered(&whole, format, &session);
            for table in [
                "customers",
                "categories",
                "products",
                "orders",
                "order_items",
            ] {
                assert!(text.contains(table), "{engine:?} {format:?}: {table}");
            }
            if matches!(
                format,
                Format::Html | Format::Pdf | Format::Markdown | Format::Sql
            ) {
                assert!(text.contains("order_totals"), "{engine:?} {format:?}");
                assert!(text.contains("active_customers"), "{engine:?} {format:?}");
            }
            for secret in [params.host.as_str(), port.as_str(), "kerno_test_pw"] {
                assert!(!text.contains(secret), "{engine:?} {format:?}: {secret}");
            }
            if let Some(dir) = std::env::var_os("K31_SAMPLES") {
                let path = std::path::Path::new(&dir).join(format!(
                    "{}-kerno.{}",
                    engine.label().to_lowercase(),
                    format.extension()
                ));
                let bytes = schema_doc::render(
                    &whole,
                    format,
                    &Options::default(),
                    session.dialect(),
                    &fonts(),
                )
                .expect("written");
                std::fs::write(path, bytes).unwrap();
            }
        }
        // A subset holds only what was picked.
        let picked = document(
            &session,
            &Pick {
                relations: ["orders", "order_totals"].map(String::from).into(),
                code: false,
            },
        )
        .await;
        let mut names: Vec<&str> = picked
            .side
            .objects
            .iter()
            .map(|object| object.object.name())
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["order_totals", "orders"], "{engine:?}");
        let markdown = rendered(&picked, Format::Markdown, &session);
        assert!(markdown.contains("### `orders`"));
        assert!(!markdown.contains("### `customers`"), "{engine:?}");
        session.close().await;
    }
}
