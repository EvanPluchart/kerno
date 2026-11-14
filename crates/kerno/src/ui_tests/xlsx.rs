//! J12 (DBeaver #18393): an Excel workbook from the export dialog, from a
//! table's data and from a query's result, on the three engines, read back
//! from the file.

use kerno_core::xlsx::read::{ReadCell, read_sheets};

use super::*;

/// The index of "Excel (XLSX)" among the dialog's formats.
const XLSX: usize = 6;

#[gpui_kit::test]
fn a_table_and_a_result_are_exported_as_typed_workbooks(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let (cleanup, create) = scratch_schema(engine, "kerno_ui_xlsx");
        let _scratch = Scratch::new(
            engine,
            &[&cleanup],
            &[
                &create,
                "CREATE TABLE kerno_ui_xlsx.t (id INT PRIMARY KEY, name VARCHAR(20), \
                 price DECIMAL(10,2), day DATE)",
                "INSERT INTO kerno_ui_xlsx.t VALUES (1, 'one', 12.50, '2024-02-29'), \
                 (2, NULL, NULL, NULL)",
            ],
        );
        let app = connected(cx, engine);
        let dir = tempfile::tempdir().unwrap();
        app.open_table(cx, "kerno_ui_xlsx", "t", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–2 of 2");

        let tab = app.data(cx);
        let dialog = app
            .step(cx, |window, cx| {
                tab.update(cx, |tab, cx| tab.export(window, cx))
            })
            .expect("the export dialog");
        assert!(app.has(cx, ("export-encoding", 0usize)));
        app.click(cx, ("export-format", XLSX));
        // A workbook has its own encoding, and a sheet name instead.
        assert!(!app.has(cx, ("export-encoding", 0usize)), "{engine:?}");
        assert!(app.has(cx, "export-sheet-name"));
        let path = dir.path().join("t.xlsx");
        app.export_to(cx, &path);
        assert_eq!(
            cx.update(|cx| dialog.read(cx).state()),
            "done: 2 rows",
            "{engine:?}"
        );
        let sheets = read_sheets(&path);
        assert_eq!(sheets.len(), 1);
        // The sheet is named after the table.
        assert_eq!(sheets[0].name, "t", "{engine:?}");
        let rows = &sheets[0].rows;
        let header: Vec<ReadCell> = rows[0].values().cloned().collect();
        let text = |value: &str| ReadCell::Text(value.into());
        assert_eq!(
            header,
            [text("id"), text("name"), text("price"), text("day")]
        );
        assert_eq!(
            rows[1].values().cloned().collect::<Vec<_>>(),
            [
                ReadCell::Number(1.0),
                text("one"),
                ReadCell::Number(12.5),
                ReadCell::Date(45_351.0, "yyyy-mm-dd".into()),
            ],
            "{engine:?}"
        );
        assert_eq!(rows[2].keys().collect::<Vec<_>>(), [&0], "{engine:?}");
        app.click(cx, "export-close");

        // A query's result, without a header, on a sheet of its own name.
        let target = app.target(cx, None);
        let connections = app.connections.clone();
        let dialog = app
            .step(cx, |window, cx| {
                crate::export_dialog::open(
                    connections,
                    target,
                    crate::export_dialog::Source::Query {
                        sql: "SELECT id, name FROM kerno_ui_xlsx.t ORDER BY id".into(),
                    },
                    window,
                    cx,
                )
            })
            .expect("the export dialog");
        app.click(cx, ("export-format", XLSX));
        app.click(cx, "export-header");
        app.click(cx, "export-sheet-name");
        app.press(cx, &["secondary-a", "backspace"]);
        app.type_text(cx, "my: result");
        let path = dir.path().join("result.xlsx");
        app.export_to(cx, &path);
        assert_eq!(cx.update(|cx| dialog.read(cx).state()), "done: 2 rows");
        let sheets = read_sheets(&path);
        assert_eq!(sheets[0].name, "my_ result", "{engine:?}");
        assert_eq!(sheets[0].rows.len(), 2);
        assert_eq!(sheets[0].rows[0][&1], text("one"));
        app.click(cx, "export-close");
    }
}
