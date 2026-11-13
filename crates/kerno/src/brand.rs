//! Kerno's own mark and horizontal lockup, from the owner's files in
//! `assets/brand/` (rules in `BRAND.md`). The files are embedded byte for
//! byte and drawn at their own colours (rasterised at the size they are
//! shown, [`crate::engine_logo::svg_image`]): never recoloured, redrawn or
//! stretched. Their viewBoxes already carry the clear space (one
//! disk height, 44 units on the 256 grid).

use gpui_kit::component::{ActiveTheme as _, StyledExt as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Styled as _,
    TestSupportExt as _, div, px, rgb,
};

use crate::theme;

const LOCKUP: &str = "brand/kerno-horizontal.svg";
const LOCKUP_REVERSED: &str = "brand/kerno-horizontal-reversed.svg";
const MARK_OLIVE: &str = "brand/kerno-symbol-mono-4d6b00.svg";
const MARK_LIME: &str = "brand/kerno-symbol-mono-c8f03c.svg";
const MARK: &str = "brand/kerno-symbol.svg";

/// The brand files the app serves, by asset path.
pub const FILES: [(&str, &[u8]); 5] = [
    (
        LOCKUP,
        include_bytes!("../../../assets/brand/kerno-horizontal.svg"),
    ),
    (
        LOCKUP_REVERSED,
        include_bytes!("../../../assets/brand/kerno-horizontal-reversed.svg"),
    ),
    (
        MARK_OLIVE,
        include_bytes!("../../../assets/brand/kerno-symbol-mono-4d6b00.svg"),
    ),
    (
        MARK_LIME,
        include_bytes!("../../../assets/brand/kerno-symbol-mono-c8f03c.svg"),
    ),
    (
        MARK,
        include_bytes!("../../../assets/brand/kerno-symbol.svg"),
    ),
];

/// Width over height of the lockup's viewBox (820.852 × 256).
const LOCKUP_RATIO: f32 = 820.852 / 256.0;

/// The horizontal lockup, `width` wide (BRAND.md: 96 px at least): ink on
/// light, the reversed file on dark. The reversed file paints its own Night
/// background, so on dark it sits on a Night panel that continues it, with
/// room around it: a deliberate band, not a lighter rectangle.
pub fn lockup(width: Pixels, cx: &App) -> impl IntoElement {
    let dark = cx.theme().is_dark();
    div()
        .id("about-lockup")
        .test_support()
        .flex()
        .justify_center()
        .rounded(cx.theme().radius_lg)
        .when(dark, |panel| panel.w_full().bg(rgb(theme::NIGHT)).py_2())
        .child(crate::engine_logo::svg_image(
            if dark { LOCKUP_REVERSED } else { LOCKUP },
            width,
            width / LOCKUP_RATIO,
        ))
}

/// The mark, in a square of `size` that includes its clear space: olive on
/// light surfaces, lime on dark ones.
pub fn mark(size: Pixels, cx: &App) -> impl IntoElement {
    let path = if cx.theme().is_dark() {
        MARK_LIME
    } else {
        MARK_OLIVE
    };
    div()
        .id("brand-mark")
        .test_support()
        .child(crate::engine_logo::svg_image(path, size, size))
}

/// The lines of the About window under the lockup, version first.
fn about_lines() -> [String; 5] {
    [
        format!("Version {}", env!("CARGO_PKG_VERSION")),
        // Two balanced lines, broken where the sentence breaks.
        "A fast, native database client\nfor PostgreSQL, MySQL and MariaDB.".into(),
        "Licensed under MIT or Apache-2.0, at your option.".into(),
        "Nothing leaves your machine: no telemetry, no update checks.".into(),
        "Third-party notices: THIRD_PARTY_NOTICES.md".into(),
    ]
}

/// The body of the About window: the lockup, then what Kerno is.
pub fn about(cx: &App) -> impl IntoElement {
    let [version, what, licence, privacy, notices] = about_lines();
    let muted = cx.theme().muted_foreground;
    v_flex()
        .items_center()
        .text_center()
        .gap_1()
        .child(lockup(px(240.), cx))
        .child(
            div()
                .id("about-version")
                .test_support()
                .mt_2()
                .font_semibold()
                .child(version),
        )
        .child(
            v_flex()
                .id("about-what")
                .test_support()
                .items_center()
                .children(what.lines().map(|line| div().child(line.to_string()))),
        )
        .child(div().text_color(muted).child(licence))
        .child(div().text_color(muted).child(privacy))
        .child(div().text_color(muted).child(notices))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::AssetSource as _;

    #[test]
    fn the_brand_files_are_embedded_unchanged_and_served() {
        let listed = crate::engine_logo::Assets.list("brand").unwrap();
        assert_eq!(listed.len(), FILES.len(), "{listed:?}");
        for (path, bytes) in FILES {
            assert!(listed.iter().any(|listed| listed == path), "{path}");
            let served = crate::engine_logo::Assets.load(path).unwrap().expect(path);
            let file = path.trim_start_matches("brand/");
            let source = std::fs::read(format!(
                "{}/../../assets/brand/{file}",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap();
            // Each path serves the owner's file of the same name, unchanged.
            assert_eq!(bytes, source.as_slice(), "{path}");
            assert_eq!(served.as_ref(), bytes, "{path}");
            assert!(served.starts_with(b"<svg"), "{path}");
        }
    }

    #[test]
    fn the_about_lines_say_what_kerno_is() {
        let lines = about_lines();
        assert_eq!(lines[0], format!("Version {}", env!("CARGO_PKG_VERSION")));
        let text = lines.join("\n");
        for needle in [
            "MIT or Apache-2.0",
            "Nothing leaves your machine",
            "THIRD_PARTY_NOTICES.md",
        ] {
            assert!(text.contains(needle), "{needle}");
        }
        // What Kerno is: two lines of about the same length, no word alone.
        let what: Vec<&str> = lines[1].lines().collect();
        assert_eq!(what.len(), 2, "{what:?}");
        assert!(what[0].len().abs_diff(what[1].len()) <= 6, "{what:?}");
    }
}
