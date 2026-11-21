//! K31 (docs/plans/M12.md): "Export schema…" writes a schema's structure as a document. The
//! dialog lists the schema's tables and views ticked (the table it was opened from alone),
//! searches them, takes a format and the HTML style from cards, the file from the save panel
//! (stubbed), and writes the file; it can be cancelled while it reads. It only reads the
//! fixture schemas: nothing is created in them, a read-only or production connection exports
//! without a question.

use std::path::{Path, PathBuf};

use kerno_core::schema_doc::{Format, PageSize, Style};

use gpui_kit::component::WindowExt as _;

use super::*;
use crate::schema_export_dialog::SchemaExportDialog;

/// The fixture's schema: `shop` on PostgreSQL, the database `kerno` elsewhere.
fn target(app: &Fixture, cx: &mut TestAppContext, engine: Engine) -> Target {
    let connection = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    Target {
        connection,
        database: Some("kerno".into()),
        schema: (engine == Engine::Postgres).then(|| "shop".into()),
    }
}

impl Fixture {
    fn export_dialog(
        &self,
        cx: &mut TestAppContext,
        target: Target,
        tables: Vec<String>,
    ) -> Entity<SchemaExportDialog> {
        let workspace = self.workspace.clone();
        let dialog = self
            .step(cx, move |window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.export_schema(target, tables, window, cx)
                })
            })
            .expect("the dialog");
        self.wait_until(cx, "the tables", |_, cx| {
            cx.update(|cx| dialog.read(cx).listing_text())
                .starts_with("ready")
        });
        dialog
    }

    fn on_export<R>(
        &self,
        cx: &mut TestAppContext,
        dialog: &Entity<SchemaExportDialog>,
        f: impl FnOnce(
            &mut SchemaExportDialog,
            &mut gpui_kit::Window,
            &mut gpui_kit::Context<SchemaExportDialog>,
        ) -> R,
    ) -> R {
        self.step(cx, |window, cx| {
            dialog.update(cx, |dialog, cx| f(dialog, window, cx))
        })
    }

    /// Chooses `path` in the save panel "Choose…" opens.
    fn choose(&self, cx: &mut TestAppContext, path: &Path) {
        self.click(cx, "schema-export-choose");
        let path = path.to_path_buf();
        cx.simulate_new_path_selection(move |_| Some(path));
        cx.run_until_parked();
    }

    /// Clicks Export and waits for the end.
    fn export_now(&self, cx: &mut TestAppContext, dialog: &Entity<SchemaExportDialog>) -> String {
        self.click(cx, "schema-export-start");
        self.wait_until(cx, "the export", |_, cx| {
            !matches!(
                cx.update(|cx| dialog.read(cx).state_text()).as_str(),
                "options" | "running"
            )
        });
        cx.update(|cx| dialog.read(cx).state_text())
    }

    fn painted(&self, cx: &mut TestAppContext, text: &str) -> bool {
        self.step(cx, |window, _| {
            gpui_kit::base::test_support::texts(window)
                .into_iter()
                .any(|painted| painted.text() == text)
        })
    }
}

/// Every format of a schema of each engine: the save panel's file is the one written, its
/// extension picks the format, and the file names the fixture's tables.
#[gpui_kit::test]
fn each_format_writes_its_file(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    for engine in ENGINES {
        if !testing::available(engine) {
            continue;
        }
        let app = connected(cx, engine);
        let target = target(&app, cx, engine);
        let dialog = app.export_dialog(cx, target, Vec::new());
        // Everything ticked; nothing to export before a file is chosen.
        assert!(app.has(cx, "schema-export-dialog"));
        assert!(app.painted(cx, "Choose the file to write."), "{engine:?}");
        assert!(cx.update(|cx| dialog.read(cx).picked()).len() >= 7);
        for format in Format::ALL {
            let path: PathBuf = dir
                .path()
                .join(format!("{engine:?}-doc.{}", format.extension()));
            app.choose(cx, &path);
            assert_eq!(cx.update(|cx| dialog.read(cx).format()), format);
            assert_eq!(
                app.export_now(cx, &dialog),
                format!("done: {}", path.display())
            );
            // The PDF names its tables in its outline, which is not compressed.
            let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
            for table in ["customers", "orders", "order_items", "products"] {
                assert!(text.contains(table), "{engine:?} {format:?}: {table}");
            }
            assert!(app.has(cx, "schema-export-reveal") && app.has(cx, "schema-export-open"));
            app.on_export(cx, &dialog, |dialog, _, cx| dialog.again(cx));
        }
        let html =
            std::fs::read_to_string(dir.path().join(format!("{engine:?}-doc.html"))).unwrap();
        assert!(html.contains("<footer class=\"generated\">Generated by Kerno</footer>"));
        assert!(html.contains("id=\"diagram\""));
    }
}

/// A table of the tree opens the dialog with it ticked alone; the search narrows the list and
/// All / None act on what it shows; the document holds only what is ticked.
#[gpui_kit::test]
fn a_table_is_ticked_alone_and_a_subset_is_exported(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let app = connected(cx, Engine::Postgres);
    let orders = ["kerno", "shop", "Tables", "orders"];
    app.open_path(cx, &orders);
    app.act(cx, Action::ExportSchema, &orders);
    app.wait_until(cx, "the dialog's list", |app, cx| {
        app.has(cx, ("schema-export-object", 0usize))
    });
    assert!(app.painted(cx, "1 of 9 ticked"));
    app.step(cx, |window, cx| window.close_dialog(cx));

    let target = target(&app, cx, Engine::Postgres);
    let dialog = app.export_dialog(cx, target, vec!["orders".into()]);
    assert_eq!(cx.update(|cx| dialog.read(cx).picked()), ["orders"]);
    app.on_export(cx, &dialog, |dialog, window, cx| {
        dialog.search_for("order", window, cx)
    });
    app.click(cx, "schema-export-all");
    assert_eq!(
        cx.update(|cx| dialog.read(cx).picked()),
        ["order_items", "order_totals", "orders"]
    );
    app.on_export(cx, &dialog, |dialog, window, cx| {
        dialog.search_for("items", window, cx)
    });
    app.click(cx, "schema-export-none");
    assert_eq!(
        cx.update(|cx| dialog.read(cx).picked()),
        ["order_totals", "orders"]
    );
    app.on_export(cx, &dialog, |dialog, window, cx| {
        dialog.search_for("zzz", window, cx)
    });
    assert!(app.has(cx, "schema-export-no-match"));
    let path = dir.path().join("subset.md");
    app.choose(cx, &path);
    assert!(app.export_now(cx, &dialog).starts_with("done"));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("### `orders`") && text.contains("### `order_totals` (view)"));
    for left_out in [
        "### `customers`",
        "### `order_items`",
        "### `product_sales`",
    ] {
        assert!(!text.contains(left_out), "{left_out}");
    }
    // Nothing ticked: Export says why and stays greyed.
    app.on_export(cx, &dialog, |dialog, window, cx| {
        dialog.again(cx);
        dialog.search_for("", window, cx);
        dialog.tick_shown(false, cx);
    });
    assert!(app.painted(cx, "Tick at least one table or view."));
    app.click(cx, "schema-export-start");
    assert_eq!(cx.update(|cx| dialog.read(cx).state_text()), "options");
}

/// The style cards choose the HTML's look; the diagram can be left out; the other formats
/// have no style.
#[gpui_kit::test]
fn the_style_cards_choose_the_look_of_the_html(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let app = connected(cx, Engine::Postgres);
    let target = target(&app, cx, Engine::Postgres);
    let dialog = app.export_dialog(cx, target, vec!["customers".into(), "orders".into()]);
    assert!(app.has(cx, crate::settings_ui::check_id("schema-export-style", 0)));
    for (index, style) in Style::ALL.into_iter().enumerate() {
        app.step(cx, |window, cx| {
            window.within("schema-export-style").click(index, cx)
        });
        assert!(
            app.has(
                cx,
                crate::settings_ui::check_id("schema-export-style", index)
            ),
            "{style:?}"
        );
        let path = dir.path().join(format!("{index}.html"));
        app.choose(cx, &path);
        assert!(app.export_now(cx, &dialog).starts_with("done"));
        let html = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            html.contains("Generated by Kerno"),
            style != Style::Plain,
            "{style:?}"
        );
        assert_eq!(
            html.contains("class=\"figure print-only\""),
            style == Style::KernoDark,
            "{style:?}"
        );
        app.on_export(cx, &dialog, |dialog, _, cx| dialog.again(cx));
    }
    // Below the fold of the test window: the option itself, as its checkbox sets it.
    assert!(app.has(cx, "schema-export-diagram"));
    app.on_export(cx, &dialog, |dialog, _, cx| dialog.set_diagram(false, cx));
    let path = dir.path().join("plain-no-diagram.html");
    app.choose(cx, &path);
    assert!(app.export_now(cx, &dialog).starts_with("done"));
    assert!(
        !std::fs::read_to_string(&path)
            .unwrap()
            .contains("id=\"diagram\"")
    );
    app.on_export(cx, &dialog, |dialog, _, cx| dialog.again(cx));
    // The format cards: Markdown has no style.
    app.step(cx, |window, cx| {
        window.within("schema-export-format").click(2usize, cx)
    });
    assert_eq!(cx.update(|cx| dialog.read(cx).format()), Format::Markdown);
    assert!(!app.has(cx, "schema-export-style"));
    // The chosen file follows the format.
    assert_eq!(
        cx.update(|cx| dialog.read(cx).output().map(Path::to_path_buf)),
        Some(dir.path().join("plain-no-diagram.md"))
    );
}

/// Cancel while the structure is read: nothing is written, the dialog says so and goes back
/// to its options.
#[gpui_kit::test]
fn an_export_is_cancelled_while_it_reads(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let app = connected(cx, Engine::Postgres);
    let target = target(&app, cx, Engine::Postgres);
    let dialog = app.export_dialog(cx, target, Vec::new());
    let path = dir.path().join("cancelled.html");
    app.choose(cx, &path);
    app.on_export(cx, &dialog, |dialog, window, cx| {
        dialog.export(window, cx);
        assert_eq!(dialog.state_text(), "running");
        dialog.cancel(cx);
    });
    assert_eq!(cx.update(|cx| dialog.read(cx).state_text()), "cancelled");
    assert!(app.has(cx, "schema-export-cancelled"));
    // The reading stopped: no file arrives later.
    for _ in 0..20 {
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!path.exists());
    assert_eq!(cx.update(|cx| dialog.read(cx).state_text()), "cancelled");
    app.click(cx, "schema-export-back");
    assert_eq!(cx.update(|cx| dialog.read(cx).state_text()), "options");
}

/// The export only reads: a read-only connection and a production one export without a
/// question, and the palette's command opens the dialog on the browsed connection.
#[gpui_kit::test]
fn read_only_and_production_connections_export_without_a_question(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for (index, read_only) in [true, false].into_iter().enumerate() {
        let mut config = testing::config(Engine::Postgres);
        config.read_only = read_only;
        if !read_only {
            config.environment = kerno_core::environment::PRODUCTION.into();
        }
        let app = connected_as(cx, config);
        let target = target(&app, cx, Engine::Postgres);
        let dialog = app.export_dialog(cx, target, vec!["orders".into()]);
        let path = dir.path().join(format!("{index}.sql"));
        app.choose(cx, &path);
        assert!(app.export_now(cx, &dialog).starts_with("done"));
        assert!(!app.has(cx, "sql-confirm") && !app.has(cx, "sql-production"));
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("CREATE TABLE \"shop\".\"orders\"")
        );
        app.step(cx, |window, cx| window.close_dialog(cx));
        app.dispatch(cx, actions::ExportSchema);
        app.wait_until(cx, "the dialog from the palette", |app, cx| {
            app.has(cx, "schema-export-dialog")
        });
        app.step(cx, |window, cx| window.close_dialog(cx));
    }
}

/// Enter opens the save panel while no file is chosen, then exports; on the result it does
/// nothing more.
#[gpui_kit::test]
fn enter_chooses_the_file_then_exports(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let app = connected(cx, Engine::Postgres);
    let target = target(&app, cx, Engine::Postgres);
    let dialog = app.export_dialog(cx, target, vec!["orders".into()]);
    // No file yet: Enter opens the save panel on `<schema>-schema.html`.
    let path = dir.path().join("enter.html");
    let answer = path.clone();
    app.press(cx, &["enter"]);
    cx.simulate_new_path_selection(move |_| Some(answer));
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| dialog.read(cx).output().map(Path::to_path_buf)),
        Some(path.clone())
    );
    assert_eq!(cx.update(|cx| dialog.read(cx).state_text()), "options");
    app.press(cx, &["enter"]);
    app.wait_until(cx, "the export", |_, cx| {
        cx.update(|cx| dialog.read(cx).state_text())
            .starts_with("done")
    });
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("<h3>orders</h3>")
    );
    // Enter on the result does nothing more.
    app.press(cx, &["enter"]);
    assert!(
        cx.update(|cx| dialog.read(cx).state_text())
            .starts_with("done")
    );
}

/// K32: the PDF card writes a `.pdf` file; the paper choice, shown for the PDF only, changes its
/// pages' size.
#[gpui_kit::test]
fn the_pdf_card_writes_a_pdf_on_the_paper_chosen(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let app = connected(cx, Engine::Postgres);
    let target = target(&app, cx, Engine::Postgres);
    let dialog = app.export_dialog(cx, target, vec!["customers".into(), "orders".into()]);
    assert!(!app.has(cx, ("schema-export-page", 0usize)));
    app.step(cx, |window, cx| {
        window.within("schema-export-format").click(1usize, cx)
    });
    assert_eq!(cx.update(|cx| dialog.read(cx).format()), Format::Pdf);
    assert!(app.has(cx, crate::settings_ui::check_id("schema-export-style", 0)));
    assert!(
        app.has(cx, ("schema-export-page", 0usize)) && app.has(cx, ("schema-export-page", 1usize))
    );
    let a4 = dir.path().join("shop.pdf");
    app.choose(cx, &a4);
    assert!(app.export_now(cx, &dialog).starts_with("done"));
    let bytes = std::fs::read(&a4).unwrap();
    assert!(bytes.starts_with(b"%PDF-"));
    let has = |bytes: &[u8], needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    assert!(has(&bytes, b"/MediaBox[0 0 595.276 841.89]"));
    app.on_export(cx, &dialog, |dialog, _, cx| {
        dialog.again(cx);
        // Below the fold of the test window: the choice itself, as its chip sets it.
        dialog.set_page(PageSize::Letter, cx);
    });
    let letter = dir.path().join("shop-letter.pdf");
    app.choose(cx, &letter);
    assert!(app.export_now(cx, &dialog).starts_with("done"));
    let bytes = std::fs::read(&letter).unwrap();
    assert!(has(&bytes, b"/MediaBox[0 0 612 792]"));
    assert!(!has(&bytes, b"/MediaBox[0 0 595.276"));
}
