//! Kerno's look: the two themes (light, dark) defined as tokens in
//! `assets/themes/kerno.json`, the embedded typefaces, and the tokens the
//! component library has no slot for (connection tags, environments).
//!
//! No colour is written anywhere else in the app: views read
//! `cx.theme()` and the functions here.

use std::borrow::Cow;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode, ThemeRegistry};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Div, ElementId, Hsla, InteractiveElement as _, Stateful, Styled as _, Window, div, px, rgb,
};
use kerno_core::ConnectionConfig;
use kerno_core::environment::{self, Tint};
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
    // A switch loads the theme file's sizes: the settings' go back on top.
    apply_fonts(cx);
}

/// Gives the interface and the SQL editors the text sizes of the settings
/// (the editors read the theme's monospace size; the grids read theirs from
/// the settings).
pub fn apply_fonts(cx: &mut App) {
    let Some((ui, code)) = cx
        .try_global::<crate::state::AppState>()
        .map(|state| (state.settings.ui_font_size, state.settings.editor_font_size))
    else {
        return;
    };
    let theme = cx.theme();
    if theme.font_size == px(ui) && theme.mono_font_size == px(code) {
        return;
    }
    Theme::update(cx, |theme| {
        theme.font_size = px(ui);
        theme.mono_font_size = px(code);
    });
}

/// Night, the brand's dark surface (`assets/brand/BRAND.md`): the reversed
/// lockup is drawn on it, as its file is. The other brand colours (Lime,
/// Ink, Olive, Lime wash) live in the theme file.
pub const NIGHT: u32 = 0x0E1116;

/// An option chip (a format, an encoding, a column of a key): selected, it
/// is drawn as a primary button, Lime with Ink, the one selected style of
/// every chip; otherwise outlined.
pub fn chip(id: impl Into<ElementId>, selected: bool) -> Button {
    let chip = Button::new(id);
    if selected {
        chip.primary()
    } else {
        chip.outline()
    }
}

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

/// A round swatch of the tag colour `key` (`None`: no colour), ringed when
/// selected: one choice of the colour rows of the connection and folder
/// dialogs. The caller adds what a click does.
pub fn swatch(
    id: impl Into<ElementId>,
    key: Option<&str>,
    selected: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .size(px(20.))
        .rounded_full()
        .border_2()
        .border_color(if selected { theme.ring } else { theme.border })
        .when_some(key.and_then(|key| tag_color(key, cx)), |this, color| {
            this.bg(color)
        })
        .cursor_pointer()
}

/// The colour tag of a connection, drawn as a small dot wherever it is
/// named (its sidebar row, its tabs, the status bar). `None` when it has no
/// tag: no marker then, the environment is shown its own way.
pub fn connection_color(config: &ConnectionConfig, cx: &App) -> Option<Hsla> {
    config.color.as_deref().and_then(|key| tag_color(key, cx))
}

/// WCAG 2 relative luminance of `0xRRGGBB`.
pub fn luminance(rgb: u32) -> f64 {
    let channel = |shift: u32| {
        let value = ((rgb >> shift) & 0xFF) as f64 / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

/// WCAG 2 contrast ratio of two `0xRRGGBB` colours.
pub fn contrast(a: u32, b: u32) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// `color` laid over `surface` at `amount` (0 to 1), channel by channel.
pub fn mix(surface: u32, color: u32, amount: f64) -> u32 {
    let amount = amount.clamp(0.0, 1.0);
    [16, 8, 0].iter().fold(0, |rgb, shift| {
        let (s, c) = (
            ((surface >> shift) & 0xFF) as f64,
            ((color >> shift) & 0xFF) as f64,
        );
        rgb | (((s + (c - s) * amount).round() as u32) << shift)
    })
}

/// Text for `surface`: `preferred` (the theme's) when it reaches WCAG AA
/// (4.5:1), else black or white, whichever contrasts more (one of them
/// always reaches 4.58:1).
pub fn text_on(surface: u32, preferred: u32) -> u32 {
    if contrast(preferred, surface) >= 4.5 {
        preferred
    } else if contrast(0x000000, surface) >= contrast(0xFFFFFF, surface) {
        0x000000
    } else {
        0xFFFFFF
    }
}

/// The colours an environment gives the chrome, as `0xRRGGBB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChromeColors {
    /// The colour itself: the frame around the editor and the grid.
    pub frame: u32,
    /// The status bar and the tab titles: the colour over the status bar's
    /// surface, at its tint's strength.
    pub bar: u32,
    /// Text on `bar`.
    pub bar_text: u32,
    /// The tab's toolbar: a wash light enough that the theme's text and
    /// muted text stay readable on it.
    pub wash: u32,
}

/// The chrome colours of an environment of colour `color` and `tint`, over
/// a theme whose status bar, background, text and muted text are given.
/// `None` when the tint is none.
pub fn chrome_colors(
    tint: Tint,
    color: u32,
    status_bar: u32,
    background: u32,
    foreground: u32,
    muted: u32,
) -> Option<ChromeColors> {
    // How much of the colour the bar and the toolbar take.
    let (bar, wash) = match tint {
        Tint::None => return None,
        Tint::Soft => (0.3, 0.08),
        Tint::Strong => (1.0, 0.16),
    };
    let mut bar = mix(status_bar, color, bar);
    // In a dark theme the bar is a deep fill under the theme's light text,
    // never a band brighter than that text: the colour keeps its hue and
    // loses lightness until the text reads on it at AAA's 7:1 (at AA's 4.5:1
    // production's red stays a loud mid red across the window).
    if luminance(foreground) > luminance(background)
        && contrast(foreground, bar) < DARK_BAR_CONTRAST
    {
        bar = darkened_under(foreground, bar, DARK_BAR_CONTRAST);
    }
    // Lighter until the theme's text reads on it: the toolbar's buttons
    // keep their own colours.
    let wash = (0..=(wash * 100.0) as u32)
        .rev()
        .map(|percent| mix(background, color, f64::from(percent) / 100.0))
        .find(|wash| contrast(foreground, *wash) >= 4.5 && contrast(muted, *wash) >= 4.5)
        .unwrap_or(background);
    Some(ChromeColors {
        frame: color,
        bar,
        bar_text: text_on(bar, foreground),
        wash,
    })
}

/// How well the light text of a dark theme reads on a tinted bar.
const DARK_BAR_CONTRAST: f64 = 7.0;

/// `color` with its HSL lightness lowered step by step, hue and saturation
/// kept, until `text` reads on it at `ratio` (black, at the end, does for
/// any light text of a dark theme).
fn darkened_under(text: u32, color: u32, ratio: f64) -> u32 {
    let hsla: Hsla = rgb(color).into();
    (0..=100)
        .rev()
        .map(|percent| {
            rgb_of(Hsla {
                l: hsla.l * percent as f32 / 100.0,
                ..hsla
            })
        })
        .find(|fill| contrast(text, *fill) >= ratio)
        .unwrap_or(0x000000)
}

/// `0xRRGGBB` of a theme colour (its alpha left out).
pub(crate) fn rgb_of(color: Hsla) -> u32 {
    let color = color.to_rgb();
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;
    (channel(color.r) << 16) | (channel(color.g) << 8) | channel(color.b)
}

/// What a connection's environment paints on the chrome around it, in the
/// current theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnvironmentChrome {
    pub frame: Hsla,
    pub bar: Hsla,
    pub bar_text: Hsla,
    pub wash: Hsla,
}

/// The chrome of `config`'s environment; `None` for an environment that
/// tints nothing (local, by default) or has no colour for this theme.
pub fn environment_chrome(config: &ConnectionConfig, cx: &App) -> Option<EnvironmentChrome> {
    environment_chrome_of(&config.environment, cx)
}

/// The chrome of the environment `id`, as [`environment_chrome`].
pub fn environment_chrome_of(id: &str, cx: &App) -> Option<EnvironmentChrome> {
    let state = cx.try_global::<crate::state::AppState>()?;
    let def = environment::environment(&state.settings.environments, id);
    let theme = cx.theme();
    let colors = chrome_colors(
        def.tint,
        def.color(theme.is_dark())?,
        rgb_of(theme.status_bar),
        rgb_of(theme.background),
        rgb_of(theme.foreground),
        rgb_of(theme.muted_foreground),
    )?;
    Some(EnvironmentChrome {
        frame: rgb(colors.frame).into(),
        bar: rgb(colors.bar).into(),
        bar_text: rgb(colors.bar_text).into(),
        wash: rgb(colors.wash).into(),
    })
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

    /// `#RRGGBB` or `#RRGGBBAA` (alpha left out) as `0xRRGGBB`.
    fn hex(color: &str) -> u32 {
        let digits = color.trim_start_matches('#');
        assert!(
            digits.len() == 6 || digits.len() == 8,
            "not a hex colour: {color}"
        );
        u32::from_str_radix(&digits[..6], 16).unwrap()
    }

    fn contrast(a: &str, b: &str) -> f64 {
        super::contrast(hex(a), hex(b))
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
            // A destructive button's label on the button itself (the
            // kit's default is the red on a 20 % red tint: too faint in dark).
            ("button.danger.foreground", "button.danger.background"),
            ("button.danger.foreground", "button.danger.hover.background"),
            (
                "button.danger.foreground",
                "button.danger.active.background",
            ),
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
            // The side list of tabs and the headers of stacked results (J10, J11).
            ("muted.foreground", "list.active.background"),
            ("muted.foreground", "tab_bar.background"),
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
            // The kit's highlight of matched characters (completion list,
            // palette): the text accent, on the popup.
            ("base.blue", "popover.background"),
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

    /// A selected row stands out from the rows around it in the dark
    /// theme (where it carries no light-on-dark wash): at least 1.45:1
    /// against a plain and a striped row. (The light theme's Lime wash is
    /// a brand rule and carries the Olive marker.)
    #[test]
    fn a_selected_row_stands_out_in_the_dark_theme() {
        let themes = themes();
        let dark = &themes[1].colors;
        for row in ["table.background", "table.even.background"] {
            let ratio = contrast(&dark["table.active.background"], &dark[row]);
            assert!(ratio >= 1.45, "selected row on {row} is {ratio:.2}:1");
        }
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

    /// The chrome colours of `color` and `tint` over `theme`.
    fn chrome_in(theme: &ThemeEntry, tint: Tint, color: u32) -> Option<ChromeColors> {
        let c = |key: &str| hex(&theme.colors[key]);
        chrome_colors(
            tint,
            color,
            c("status_bar.background"),
            c("background"),
            c("foreground"),
            c("muted.foreground"),
        )
    }

    /// The tinted chrome of the built-in environments reads in both themes:
    /// text on the status bar and the tab titles, the theme's text and
    /// muted text on the toolbar (4.5:1); the frame stands out from the
    /// surfaces around it (3:1). Local tints nothing.
    #[test]
    fn the_built_in_environments_chrome_meets_wcag_aa() {
        let builtins = environment::builtin_environments();
        let mut failures = Vec::new();
        for theme in themes() {
            let dark = theme.mode == "dark";
            for def in &builtins {
                let Some(color) = def.color(dark) else {
                    assert_eq!(def.tint, Tint::None, "{} has no colour", def.id);
                    continue;
                };
                let chrome = chrome_in(&theme, def.tint, color).expect("a tinted environment");
                let mut check = |what: &str, ratio: f64, floor: f64| {
                    if ratio < floor {
                        failures.push(format!("{} {}: {what} {ratio:.2}:1", theme.name, def.id));
                    }
                };
                check(
                    "text on the bar",
                    super::contrast(chrome.bar_text, chrome.bar),
                    4.5,
                );
                for key in ["foreground", "muted.foreground"] {
                    let text = hex(&theme.colors[key]);
                    check(key, super::contrast(text, chrome.wash), 4.5);
                }
                for surface in ["background", "status_bar.background"] {
                    let surface = hex(&theme.colors[surface]);
                    check("the frame", super::contrast(chrome.frame, surface), 3.0);
                }
            }
            let local = environment::environment(&builtins, environment::LOCAL);
            assert_eq!(chrome_in(&theme, local.tint, 0xFF0000), None);
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));

        // The bars they give, measured: production in dark is a deep red
        // under the light text, not the bright tag red across the window.
        let table = [
            // 10.74:1, 5.42:1, 7.81:1, 7.22:1.
            (LIGHT, environment::STAGING, 0xD6C9AA, 0x16191D),
            (LIGHT, environment::PRODUCTION, 0xC62F3D, 0xFFFFFF),
            (DARK, environment::STAGING, 0x4F4523, 0xE6E9ED),
            (DARK, environment::PRODUCTION, 0x970D19, 0xE6E9ED),
        ];
        let themes = themes();
        let mut wrong = Vec::new();
        for (name, id, bar, text) in table {
            let theme = themes.iter().find(|theme| theme.name == name).unwrap();
            let def = environment::environment(&builtins, id);
            let color = def.color(theme.mode == "dark").unwrap();
            let chrome = chrome_in(theme, def.tint, color).unwrap();
            if (chrome.bar, chrome.bar_text) != (bar, text) {
                wrong.push(format!(
                    "{name} {id}: bar #{:06X}, text #{:06X} ({:.2}:1)",
                    chrome.bar,
                    chrome.bar_text,
                    super::contrast(chrome.bar, chrome.bar_text)
                ));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// Whatever colour the user picks, text on the tinted chrome reaches
    /// 4.5:1 in both themes and both tints: the colour is blended, then the
    /// text is picked (or the toolbar's wash lightened) by contrast.
    #[test]
    fn any_environment_colour_keeps_text_readable() {
        // Every corner and edge of the cube, then a spread of the rest.
        let mut colors: Vec<u32> = (0..64u32)
            .map(|i| {
                let level = |bits: u32| [0x00, 0x55, 0xAA, 0xFF][bits as usize];
                (level(i & 3) << 16) | (level((i >> 2) & 3) << 8) | level(i >> 4)
            })
            .collect();
        let mut seed: u32 = 0x2545_F491;
        for _ in 0..4000 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            colors.push(seed >> 8);
        }
        for theme in themes() {
            let (foreground, muted) = (
                hex(&theme.colors["foreground"]),
                hex(&theme.colors["muted.foreground"]),
            );
            let dark = theme.mode == "dark";
            let status_bar = hex(&theme.colors["status_bar.background"]);
            for tint in [Tint::Soft, Tint::Strong] {
                for &color in &colors {
                    let chrome = chrome_in(&theme, tint, color).unwrap();
                    let ratio = super::contrast(chrome.bar_text, chrome.bar);
                    assert!(
                        ratio >= 4.5,
                        "{} {tint:?} #{color:06X}: bar {ratio:.2}",
                        theme.name
                    );
                    if dark {
                        // A deep fill under the theme's own light text,
                        // never brighter than that text...
                        assert_eq!(chrome.bar_text, foreground, "#{color:06X}");
                        assert!(luminance(chrome.bar) < luminance(foreground));
                        assert!(ratio >= DARK_BAR_CONTRAST, "#{color:06X}: {ratio:.2}");
                        // ...of the colour's hue: only its lightness went
                        // down (greys and the darkest fills, whose 8-bit
                        // channels cannot hold a hue, left out).
                        let blended = mix(
                            status_bar,
                            color,
                            if tint == Tint::Strong { 1.0 } else { 0.3 },
                        );
                        let (before, after): (Hsla, Hsla) =
                            (rgb(blended).into(), rgb(chrome.bar).into());
                        if before.s > 0.2 && after.l > 0.1 {
                            let turn = ((before.h - after.h).abs() * 360.0) as f64;
                            let turn = turn.min(360.0 - turn);
                            assert!(turn <= 6.0, "#{color:06X} {tint:?}: hue moved {turn:.1}");
                        }
                    }
                    for text in [foreground, muted] {
                        let ratio = super::contrast(text, chrome.wash);
                        assert!(
                            ratio >= 4.5,
                            "{} {tint:?} #{color:06X}: toolbar {ratio:.2}",
                            theme.name
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn mixing_and_picking_text_work_on_known_values() {
        assert_eq!(mix(0xFFFFFF, 0x000000, 0.0), 0xFFFFFF);
        assert_eq!(mix(0xFFFFFF, 0x000000, 1.0), 0x000000);
        assert_eq!(mix(0x000000, 0xFF8000, 0.5), 0x804000);
        assert_eq!(text_on(0xFFFFFF, 0x16191D), 0x16191D);
        // Grey text on a mid red: white or black, whichever reads.
        assert_eq!(text_on(0xC62F3D, 0x5B6470), 0xFFFFFF);
        assert_eq!(text_on(0xF26D78, 0xE6E9ED), 0x000000);
    }

    #[test]
    fn tag_keys_are_unique() {
        let mut keys: Vec<_> = TAG_COLORS.iter().map(|(key, _, _)| *key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), TAG_COLORS.len());
    }
}
