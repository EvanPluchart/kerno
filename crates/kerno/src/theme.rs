//! Kerno's look: the two themes (light, dark) defined as tokens in
//! `assets/themes/kerno.json`, the embedded typefaces, and the tokens the
//! component library has no slot for (connection tags, environments).
//!
//! No colour is written anywhere else in the app: views read
//! `cx.theme()` and the functions here.

use std::borrow::Cow;

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode, ThemeRegistry};
use gpui_kit::{App, Hsla, Window, rgb};
use kerno_core::Environment;
use serde::{Deserialize, Serialize};

/// The theme file: surface levels, text levels, accent, semantic colours,
/// grid lines and selection for both modes.
const THEME_JSON: &str = include_str!("../assets/themes/kerno.json");

const LIGHT: &str = "Kerno Light";
const DARK: &str = "Kerno Dark";

/// Which theme the user chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Follow the operating system.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

impl Appearance {
    /// Every choice, in the order the settings show them.
    pub const ALL: [Appearance; 3] = [Self::System, Self::Light, Self::Dark];

    /// Name shown to the user.
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "Follow system",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

/// Registers the embedded typefaces and Kerno's themes, and applies
/// `appearance`.
pub fn init(appearance: Appearance, cx: &mut App) -> anyhow::Result<()> {
    cx.text_system().add_fonts(vec![
        Cow::Borrowed(include_bytes!("../assets/fonts/inter/Inter-Regular.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../assets/fonts/inter/Inter-Italic.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../assets/fonts/inter/Inter-Medium.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../assets/fonts/inter/Inter-SemiBold.ttf").as_slice()),
        Cow::Borrowed(
            include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf").as_slice(),
        ),
        Cow::Borrowed(
            include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-Italic.ttf").as_slice(),
        ),
        Cow::Borrowed(
            include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-Bold.ttf").as_slice(),
        ),
    ])?;

    ThemeRegistry::global_mut(cx).load_themes_from_str(THEME_JSON)?;
    let themes = ThemeRegistry::global(cx).themes();
    let (light, dark) = match (themes.get(LIGHT), themes.get(DARK)) {
        (Some(light), Some(dark)) => (light.clone(), dark.clone()),
        _ => anyhow::bail!("the embedded theme file does not define {LIGHT} and {DARK}"),
    };
    Theme::update(cx, |theme| {
        theme.light_theme = light;
        theme.dark_theme = dark;
    });
    apply(appearance, None, cx);
    Ok(())
}

/// Switches to the theme `appearance` asks for.
pub fn apply(appearance: Appearance, window: Option<&mut Window>, cx: &mut App) {
    match appearance {
        Appearance::System => Theme::sync_system_appearance(window, cx),
        Appearance::Light => Theme::change(ThemeMode::Light, window, cx),
        Appearance::Dark => Theme::change(ThemeMode::Dark, window, cx),
    }
}

/// Night, the brand's dark surface (`assets/brand/BRAND.md`): the reversed
/// lockup is drawn on it, as its file is. The other brand colours (Lime,
/// Ink, Olive, Lime wash) live in the theme file.
pub const NIGHT: u32 = 0x0E1116;

/// A colour tag a connection can carry: `(key, light mode, dark mode)`.
/// Keys are what is stored in the connection's settings.
pub const TAG_COLORS: [(&str, u32, u32); 8] = [
    ("red", 0xC62F3D, 0xF26D78),
    ("orange", 0xC2570C, 0xF39A52),
    ("yellow", 0x9A6B00, 0xE6C04A),
    ("green", 0x1B7F4B, 0x4CC38A),
    ("teal", 0x0B7A75, 0x2FBFB3),
    ("blue", 0x1D6FB8, 0x5AA9F0),
    ("purple", 0x7445C7, 0xA98BEF),
    ("pink", 0xB8327F, 0xEE7FBB),
];

/// The colour of a connection tag, or `None` for an unknown key.
pub fn tag_color(key: &str, cx: &App) -> Option<Hsla> {
    let dark = cx.theme().is_dark();
    TAG_COLORS
        .iter()
        .find(|(name, _, _)| *name == key)
        .map(|(_, light, dark_value)| rgb(if dark { *dark_value } else { *light }).into())
}

/// The colour that marks an environment: quiet for local, amber for
/// staging, red for production.
pub fn environment_color(environment: Environment, cx: &App) -> Hsla {
    match environment {
        Environment::Local => cx.theme().muted_foreground,
        Environment::Staging => cx.theme().warning,
        Environment::Production => cx.theme().danger,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Deserialize)]
    struct ThemeFile {
        themes: Vec<ThemeEntry>,
    }

    #[derive(Deserialize)]
    struct ThemeEntry {
        name: String,
        mode: String,
        colors: BTreeMap<String, String>,
        /// Editor and syntax colours.
        highlight: BTreeMap<String, serde_json::Value>,
    }

    impl ThemeEntry {
        /// An editor colour (`editor.background`, ...).
        fn editor(&self, key: &str) -> String {
            self.highlight[key].as_str().unwrap().to_string()
        }

        /// Syntax colours by token kind.
        fn syntax(&self) -> BTreeMap<String, String> {
            self.highlight["syntax"]
                .as_object()
                .unwrap()
                .iter()
                .filter_map(|(kind, style)| {
                    Some((kind.clone(), style.get("color")?.as_str()?.to_string()))
                })
                .collect()
        }
    }

    /// `#RRGGBB` or `#RRGGBBAA` to linear-light relative luminance (WCAG 2).
    fn luminance(hex: &str) -> f64 {
        let hex = hex.trim_start_matches('#');
        assert!(hex.len() == 6 || hex.len() == 8, "not a hex colour: {hex}");
        let channel = |index: usize| {
            let value = u8::from_str_radix(&hex[index..index + 2], 16).unwrap() as f64 / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(0) + 0.7152 * channel(2) + 0.0722 * channel(4)
    }

    fn contrast(a: &str, b: &str) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        let (lighter, darker) = if a > b { (a, b) } else { (b, a) };
        (lighter + 0.05) / (darker + 0.05)
    }

    fn themes() -> Vec<ThemeEntry> {
        serde_json::from_str::<ThemeFile>(THEME_JSON)
            .unwrap()
            .themes
    }

    #[test]
    fn the_file_defines_one_light_and_one_dark_theme() {
        let themes = themes();
        let found: Vec<_> = themes
            .iter()
            .map(|t| (t.name.as_str(), t.mode.as_str()))
            .collect();
        assert_eq!(found, [(LIGHT, "light"), (DARK, "dark")]);
        // Both modes define the same tokens.
        let keys = |t: &ThemeEntry| t.colors.keys().cloned().collect::<Vec<_>>();
        assert_eq!(keys(&themes[0]), keys(&themes[1]));
        let editor_keys = |t: &ThemeEntry| t.highlight.keys().cloned().collect::<Vec<_>>();
        assert_eq!(editor_keys(&themes[0]), editor_keys(&themes[1]));
        assert_eq!(
            themes[0].syntax().keys().collect::<Vec<_>>(),
            themes[1].syntax().keys().collect::<Vec<_>>()
        );
    }

    /// Text on its background: WCAG AA asks for 4.5:1.
    #[test]
    fn text_contrast_meets_wcag_aa() {
        let pairs = [
            ("foreground", "background"),
            ("muted.foreground", "background"),
            ("muted.foreground", "muted.background"),
            ("foreground", "muted.background"),
            ("accent.foreground", "accent.background"),
            ("primary.foreground", "primary.background"),
            ("primary.foreground", "primary.hover.background"),
            ("primary.foreground", "primary.active.background"),
            // A primary button's label on the button itself (Ink on Lime).
            ("button.primary.foreground", "button.primary.background"),
            (
                "button.primary.foreground",
                "button.primary.hover.background",
            ),
            (
                "button.primary.foreground",
                "button.primary.active.background",
            ),
            ("secondary.foreground", "secondary.background"),
            ("danger.foreground", "danger.background"),
            ("success.foreground", "success.background"),
            ("warning.foreground", "warning.background"),
            ("info.foreground", "info.background"),
            ("popover.foreground", "popover.background"),
            ("sidebar.foreground", "sidebar.background"),
            ("sidebar.accent.foreground", "sidebar.accent.background"),
            ("sidebar.primary.foreground", "sidebar.primary.background"),
            ("muted.foreground", "sidebar.background"),
            // The selected connection of the sidebar.
            ("foreground", "sidebar.accent.background"),
            ("muted.foreground", "sidebar.accent.background"),
            ("muted.foreground", "popover.background"),
            ("tab.foreground", "tab_bar.background"),
            ("tab.active.foreground", "tab.active.background"),
            ("muted.foreground", "title_bar.background"),
            ("foreground", "status_bar.background"),
            ("muted.foreground", "status_bar.background"),
            ("table.head.foreground", "table.head.background"),
            ("foreground", "table.background"),
            ("foreground", "table.even.background"),
            ("foreground", "table.hover.background"),
            ("foreground", "table.active.background"),
            ("muted.foreground", "table.even.background"),
            ("muted.foreground", "table.hover.background"),
            ("muted.foreground", "table.active.background"),
            ("foreground", "list.hover.background"),
            ("foreground", "list.active.background"),
            ("foreground", "selection.background"),
            // Semantic colours used as text (error messages, links).
            ("danger.background", "background"),
            ("success.background", "background"),
            ("warning.background", "background"),
            ("info.background", "background"),
            ("link", "background"),
            ("link", "muted.background"),
            ("link", "sidebar.background"),
            ("link", "popover.background"),
            ("link", "status_bar.background"),
            ("link", "table.active.background"),
            // The accent as text (an outline primary button's label).
            ("primary.background", "background"),
            ("primary.background", "popover.background"),
            ("danger.background", "sidebar.background"),
            ("danger.background", "status_bar.background"),
            ("warning.background", "status_bar.background"),
        ];
        let mut failures = Vec::new();
        for theme in themes() {
            for (text, surface) in pairs {
                let ratio = contrast(&theme.colors[text], &theme.colors[surface]);
                if ratio < 4.5 {
                    failures.push(format!(
                        "{}: {text} on {surface} is {ratio:.2}:1",
                        theme.name
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "below 4.5:1:\n{}", failures.join("\n"));
    }

    /// Things that are not text (tag dots, focus rings, borders of inputs):
    /// WCAG asks for 3:1 against what is next to them.
    #[test]
    fn non_text_contrast_meets_wcag() {
        let mut failures = Vec::new();
        for theme in themes() {
            let dark = theme.mode == "dark";
            for surface in ["background", "sidebar.background", "tab_bar.background"] {
                let surface_hex = &theme.colors[surface];
                for (name, light, dark_value) in TAG_COLORS {
                    let hex = format!("#{:06X}", if dark { dark_value } else { light });
                    let ratio = contrast(&hex, surface_hex);
                    if ratio < 3.0 {
                        failures.push(format!(
                            "{}: tag {name} on {surface} is {ratio:.2}:1",
                            theme.name
                        ));
                    }
                }
            }
            // Focus rings, input borders, the selected-row marker and the
            // drag marker, on every surface they are drawn on.
            let indicators = [
                (
                    "ring",
                    &[
                        "background",
                        "sidebar.background",
                        "tab_bar.background",
                        "muted.background",
                        "popover.background",
                        "status_bar.background",
                    ][..],
                ),
                (
                    "input.border",
                    &["background", "sidebar.background", "popover.background"],
                ),
                (
                    "list.active.border",
                    &["background", "list.active.background"],
                ),
                (
                    "table.active.border",
                    &["background", "table.active.background"],
                ),
                ("drag.border", &["background", "sidebar.background"]),
            ];
            for (indicator, surfaces) in indicators {
                for surface in surfaces {
                    let ratio = contrast(&theme.colors[indicator], &theme.colors[*surface]);
                    if ratio < 3.0 {
                        failures.push(format!(
                            "{}: {indicator} on {surface} is {ratio:.2}:1",
                            theme.name
                        ));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "below 3:1:\n{}", failures.join("\n"));
    }

    /// The SQL editor: its text, its line numbers and every syntax colour
    /// must be readable on the editor background and on the active line.
    #[test]
    fn editor_colours_meet_wcag_aa() {
        let mut failures = Vec::new();
        for theme in themes() {
            // The editor sits on the content surface: no seam between them.
            assert_eq!(
                theme.editor("editor.background").to_lowercase(),
                theme.colors["background"].to_lowercase(),
                "{}",
                theme.name
            );
            let mut texts = theme.syntax();
            for key in [
                "editor.foreground",
                "editor.line_number",
                "editor.active_line_number",
            ] {
                texts.insert(key.to_string(), theme.editor(key));
            }
            assert!(texts.len() > 20, "syntax colours are defined");
            for surface in ["editor.background", "editor.active_line.background"] {
                let surface_hex = theme.editor(surface);
                for (kind, hex) in &texts {
                    let ratio = contrast(hex, &surface_hex);
                    if ratio < 4.5 {
                        failures.push(format!(
                            "{}: {kind} on {surface} is {ratio:.2}:1",
                            theme.name
                        ));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "below 4.5:1:\n{}", failures.join("\n"));
    }

    /// The brand (`assets/brand/BRAND.md`) is where its rules put it: Lime
    /// buttons with Ink labels in both themes; on light surfaces Olive for
    /// text, links and rings (Lime is 1.3:1 on white) and Lime wash for the
    /// selected row; on dark surfaces Lime for all of it.
    #[test]
    fn the_brand_colours_sit_where_brand_md_puts_them() {
        const LIME: &str = "#C8F03C";
        const INK: &str = "#111111";
        const OLIVE: &str = "#4D6B00";
        const LIME_WASH: &str = "#F1F8D6";
        // The ratios BRAND.md lists.
        assert!(contrast(INK, LIME) >= 14.3, "ink on lime");
        assert!(contrast(OLIVE, "#FFFFFF") >= 6.1, "olive on white");
        assert!(
            contrast(LIME, "#FFFFFF") < 3.0,
            "lime cannot carry text on white"
        );
        assert!(
            contrast(LIME, &format!("#{NIGHT:06X}")) >= 4.5,
            "lime on night"
        );

        let themes = themes();
        let (light, dark) = (&themes[0].colors, &themes[1].colors);
        for colors in [light, dark] {
            assert_eq!(colors["button.primary.background"], LIME);
            assert_eq!(colors["button.primary.foreground"], INK);
        }
        for key in [
            "ring",
            "link",
            "caret",
            "primary.background",
            "list.active.border",
        ] {
            assert_eq!(light[key], OLIVE, "light {key}");
            assert_eq!(dark[key], LIME, "dark {key}");
        }
        assert_eq!(dark["primary.foreground"], INK);
        assert_eq!(light["list.active.background"], LIME_WASH);
        assert_eq!(light["table.active.background"], LIME_WASH);
        // Lime never carries text or a ring on a light surface.
        for (key, hex) in light {
            if key.starts_with("button.primary") {
                continue;
            }
            assert_ne!(hex.to_uppercase(), LIME, "light {key} is lime");
        }
        for (kind, hex) in themes[0].syntax() {
            assert_ne!(hex.to_uppercase(), LIME, "light syntax {kind} is lime");
        }
    }

    /// Hue of `#RRGGBB` in degrees.
    fn hue(hex: &str) -> f64 {
        let hex = hex.trim_start_matches('#');
        let [r, g, b] = [0, 2, 4]
            .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap() as f64 / 255.0);
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let delta = max - min;
        let hue = if delta == 0.0 {
            0.0
        } else if max == r {
            ((g - b) / delta).rem_euclid(6.0)
        } else if max == g {
            (b - r) / delta + 2.0
        } else {
            (r - g) / delta + 4.0
        };
        hue * 60.0
    }

    /// "Success" stays semantic: its green is far enough in hue from the
    /// lime accent (connected dot next to a selected connection, new rows
    /// next to the selected row) not to read as the same colour.
    #[test]
    fn success_cannot_be_mistaken_for_the_accent() {
        for theme in themes() {
            let success = hue(&theme.colors["success.background"]);
            for accent in ["primary.background", "button.primary.background"] {
                let distance = (success - hue(&theme.colors[accent])).abs();
                assert!(
                    distance.min(360.0 - distance) >= 60.0,
                    "{}: success and {accent} are {distance:.0} degrees apart",
                    theme.name
                );
            }
        }
    }

    #[test]
    fn contrast_formula_matches_known_values() {
        assert!((contrast("#000000", "#FFFFFF") - 21.0).abs() < 0.01);
        assert!((contrast("#FFFFFF", "#FFFFFF") - 1.0).abs() < 0.01);
        // #767676 on white is the classic 4.54:1 boundary grey.
        assert!((contrast("#767676", "#FFFFFF") - 4.54).abs() < 0.01);
    }

    #[test]
    fn tag_keys_are_unique() {
        let mut keys: Vec<_> = TAG_COLORS.iter().map(|(key, _, _)| *key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), TAG_COLORS.len());
    }
}
