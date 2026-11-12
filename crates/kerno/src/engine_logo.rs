//! The logo of a database engine, shown wherever an engine is named.
//!
//! The artwork in `assets/engines/` is the vendors' own, unmodified (sources
//! and trademark notices in `THIRD_PARTY_NOTICES.md`). It identifies the
//! engine a connection talks to and is never part of Kerno's own branding.

use std::borrow::Cow;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    AnyElement, App, AssetSource, IntoElement as _, Pixels, SharedString, Styled as _, img, svg,
};
use kerno_core::Engine;

const POSTGRESQL: &str = "engines/postgresql.svg";
const MYSQL: &str = "engines/mysql.svg";
const MARIADB: &str = "engines/mariadb.svg";

/// Kerno's assets: the engine logos, then everything the UI kit bundles.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        let own: Option<&'static [u8]> = match path {
            POSTGRESQL => Some(include_bytes!("../assets/engines/postgresql.svg")),
            MYSQL => Some(include_bytes!("../assets/engines/mysql.svg")),
            MARIADB => Some(include_bytes!("../assets/engines/mariadb.svg")),
            _ => None,
        };
        match own {
            Some(bytes) => Ok(Some(Cow::Borrowed(bytes))),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        gpui_kit::assets::Assets.list(path)
    }
}

/// The logo of `engine` in a square of `size`, proportions kept.
pub fn engine_logo(engine: Engine, size: Pixels, cx: &App) -> AnyElement {
    match engine {
        // The elephant in its official colours; its white outline carries
        // it on dark surfaces too.
        Engine::Postgres => img(POSTGRESQL).size(size).into_any_element(),
        // Single-colour marks, drawn in one flat colour as their vendors'
        // own black and reversed variants are: dark on light, light on dark.
        Engine::MySql => mark(MYSQL, size, cx),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_engine_has_its_logo_and_the_kit_icons_still_load() {
        for path in [POSTGRESQL, MYSQL, MARIADB] {
            let bytes = Assets.load(path).unwrap().expect(path);
            assert!(bytes.windows(4).any(|window| window == b"<svg"), "{path}");
        }
        // The UI kit's own icons are still served.
        let kit_icons = Assets.list("icons").unwrap();
        let icon = kit_icons.first().expect("the UI kit bundles icons");
        assert!(Assets.load(icon).unwrap().is_some(), "{icon}");
    }
}
