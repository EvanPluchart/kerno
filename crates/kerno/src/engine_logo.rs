//! The logo of a database engine, shown wherever an engine is named.
//!
//! The artwork in `assets/engines/` is the vendors' own, unmodified (sources
//! and trademark notices in `THIRD_PARTY_NOTICES.md`). It identifies the
//! engine a connection talks to and is never part of Kerno's own branding.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    AnyElement, App, AssetSource, DevicePixels, Global, ImageCacheError, ImageSource, Img,
    IntoElement as _, Pixels, RenderImage, SharedString, Size, Styled as _, SvgSize, img, px, size,
    svg,
};
use kerno_core::Engine;

const POSTGRESQL: &str = "engines/postgresql.svg";
const MYSQL: &str = "engines/mysql.svg";
const MARIADB: &str = "engines/mariadb.svg";

// The glyphs of the schema tree and of the query toolbar, from the same
// Lucide set as the UI kit's.
gpui_kit::assets::icon_assets!(
    TreeGlyphs,
    [
        Bookmark,
        Boxes,
        Briefcase,
        Building2,
        CalendarClock,
        Clock,
        Cloud,
        Columns3,
        Database,
        Eye,
        FlaskConical,
        Folder,
        House,
        KeyRound,
        Layers,
        Link2,
        ListOrdered,
        Puzzle,
        RefreshCw,
        Server,
        Shapes,
        ShieldCheck,
        SquareFunction,
        Star,
        Table,
        Workflow,
        Zap,
    ]
);

/// The glyphs a folder can be drawn with, by the key its look stores; the
/// first is the plain folder's (no key stored).
pub const FOLDER_GLYPHS: [(&str, IconName); 8] = [
    ("folder", IconName::Folder),
    ("briefcase", IconName::Briefcase),
    ("building", IconName::Building2),
    ("house", IconName::House),
    ("flask", IconName::FlaskConical),
    ("server", IconName::Server),
    ("cloud", IconName::Cloud),
    ("star", IconName::Star),
];

/// The glyph of a folder's look; the folder glyph for none or an unknown
/// key.
pub fn folder_glyph(key: Option<&str>) -> IconName {
    FOLDER_GLYPHS
        .iter()
        .find(|(name, _)| Some(*name) == key)
        .map_or(IconName::Folder, |(_, glyph)| *glyph)
}

/// Kerno's assets: the engine logos, Kerno's brand files and the schema
/// tree's glyphs, then everything the UI kit bundles.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        let own: Option<&'static [u8]> = match path {
            POSTGRESQL => Some(include_bytes!("../assets/engines/postgresql.svg")),
            MYSQL => Some(include_bytes!("../assets/engines/mysql.svg")),
            MARIADB => Some(include_bytes!("../assets/engines/mariadb.svg")),
            // Kerno's own mark and lockups, under `brand/`.
            _ => crate::brand::FILES
                .iter()
                .find(|(file, _)| *file == path)
                .map(|(_, bytes)| *bytes),
        };
        if let Some(bytes) = own {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        match TreeGlyphs.load(path)? {
            Some(glyph) => Ok(Some(glyph)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut found: Vec<SharedString> = crate::brand::FILES
            .iter()
            .filter(|(file, _)| file.starts_with(path))
            .map(|(file, _)| SharedString::from(*file))
            .collect();
        found.extend(gpui_kit::assets::Assets.list(path)?);
        Ok(found)
    }
}

/// The logo of `engine` in a square of `size`, proportions kept.
pub fn engine_logo(engine: Engine, size: Pixels, cx: &App) -> AnyElement {
    match engine {
        // The elephant in its official colours; its white outline carries
        // it on dark surfaces too.
        Engine::Postgres => svg_image(POSTGRESQL, size, size).into_any_element(),
        // The official dolphin and wordmark, in their colours (the owner's
        // choice, see DECISIONS D-019).
        Engine::MySql => svg_image(MYSQL, size, size).into_any_element(),
        // A single-colour mark, drawn in one flat colour as the vendor's
        // own black and reversed variants are: dark on light, light on dark.
        Engine::MariaDb => mark(MARIADB, size, cx),
    }
}

fn mark(path: &'static str, size: Pixels, cx: &App) -> AnyElement {
    svg()
        .path(path)
        .size(size)
        .text_color(cx.theme().foreground)
        .into_any_element()
}

/// The SVG asset `path` in its own colours, fitted in `width` × `height`
/// with its proportions kept, and rasterised at exactly the pixels it
/// covers. `img(path)` would rasterise the file at twice its own size and
/// keep that bitmap and its texture for the life of the app: 1,152 × 1,188
/// pixels (5.5 MB) for a PostgreSQL logo drawn at 16 px.
pub fn svg_image(path: &'static str, width: Pixels, height: Pixels) -> Img {
    img(ImageSource::Custom(Arc::new(move |window, cx| {
        let scale = window.scale_factor();
        let device =
            |length: Pixels| DevicePixels(((length / px(1.)) * scale).round().max(1.) as i32);
        let bounds = size(device(width), device(height));
        Some(rasterised(path, bounds, cx).map_err(|error| ImageCacheError::Other(Arc::new(error))))
    })))
    .w(width)
    .h(height)
}

/// Bitmaps of SVG assets by path and size in device pixels: each is drawn
/// once per size and display density.
#[derive(Default)]
struct Rasters(HashMap<(&'static str, i32, i32), Arc<RenderImage>>);

impl Global for Rasters {}

fn rasterised(
    path: &'static str,
    bounds: Size<DevicePixels>,
    cx: &mut App,
) -> anyhow::Result<Arc<RenderImage>> {
    let key = (path, bounds.width.0, bounds.height.0);
    if let Some(image) = cx.default_global::<Rasters>().0.get(&key) {
        return Ok(image.clone());
    }
    let image = rasterise(path, bounds, cx)?;
    cx.global_mut::<Rasters>().0.insert(key, image.clone());
    Ok(image)
}

/// `path` at the largest size that fits in `bounds`, proportions kept.
fn rasterise(path: &str, bounds: Size<DevicePixels>, cx: &App) -> anyhow::Result<Arc<RenderImage>> {
    let bytes = Assets
        .load(path)?
        .with_context(|| format!("no asset {path}"))?;
    let renderer = cx.svg_renderer();
    let svg = renderer.parse_svg(&bytes)?;
    // Fitted to the width; a file taller than the box, to its height.
    let image = renderer.render_parsed(&svg, SvgSize::Size(bounds))?;
    let drawn = image.size(0);
    if drawn.height <= bounds.height {
        return Ok(image);
    }
    let width = i64::from(bounds.width.0) * i64::from(bounds.height.0) / i64::from(drawn.height.0);
    let width = DevicePixels(i32::try_from(width)?.max(1));
    Ok(renderer.render_parsed(&svg, SvgSize::Size(size(width, bounds.height)))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_engine_has_its_logo_and_the_kit_icons_still_load() {
        for path in [POSTGRESQL, MYSQL, MARIADB] {
            let bytes = Assets.load(path).unwrap().expect(path);
            assert!(bytes.windows(4).any(|window| window == b"<svg"), "{path}");
        }
        // So is every glyph of the schema tree, and of folders.
        for glyph in [
            IconName::Database,
            IconName::Table,
            IconName::SquareFunction,
            IconName::Zap,
        ]
        .into_iter()
        .chain(FOLDER_GLYPHS.map(|(_, glyph)| glyph))
        {
            let path = glyph.path();
            assert!(TreeGlyphs.load(&path).unwrap().is_some(), "{path}");
        }
        // The UI kit's own icons are still served.
        let kit_icons = Assets.list("icons").unwrap();
        let icon = kit_icons.first().expect("the UI kit bundles icons");
        assert!(Assets.load(icon).unwrap().is_some(), "{icon}");
    }

    #[gpui_kit::test]
    fn logos_are_rasterised_at_the_size_they_are_drawn(cx: &mut gpui_kit::TestAppContext) {
        let brand = crate::brand::FILES.map(|(path, _)| path);
        let bounds = size(DevicePixels(32), DevicePixels(32));
        cx.update(|cx| {
            for path in [POSTGRESQL, MYSQL, MARIADB].into_iter().chain(brand) {
                let image = rasterised(path, bounds, cx).unwrap();
                let drawn = image.size(0);
                // Fitted: inside the box, touching it on one side.
                assert!(
                    drawn.width <= bounds.width && drawn.height <= bounds.height,
                    "{path}"
                );
                assert!(
                    drawn.width.0 >= 31 || drawn.height.0 >= 31,
                    "{path}: {drawn:?}"
                );
                // Drawn once per size.
                assert!(Arc::ptr_eq(&image, &rasterised(path, bounds, cx).unwrap()));
            }
        });
    }
}
