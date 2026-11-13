//! The folder dialog (CLAUDE.md §3, owner's test of 2026-11-13, item 3): a
//! folder's name and look, made with New folder and changed with Edit
//! folder…. The look is a colour of the connection palette, a built-in
//! glyph, or a picture the user picks (PNG, JPEG or SVG), copied into the
//! folder icons directory at 32 px and drawn at 16 px.
//!
//! The picture is untrusted input: it is read off the UI thread, bounded in
//! bytes and in pixels, and the stored file's name is Kerno's own (a hash of
//! what is written), never taken from the picked path.

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::io::{Cursor, Read as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Task, TestSupportExt as _, Window, div, img, px,
};
use image::{ImageFormat, ImageReader, Limits, RgbaImage};
use kerno_core::config::{FolderLook, folder_name, parent_folder};
use resvg::{tiny_skia, usvg};

use crate::bridge;
use crate::connections::manager::Connections;
use crate::engine_logo::FOLDER_GLYPHS;
use crate::state::AppState;
use crate::theme;

/// The largest picture file taken.
const MAX_PICTURE_BYTES: u64 = 2 * 1024 * 1024;
/// The largest picture decoded, in pixels per side.
const MAX_PICTURE_SIDE: u32 = 4096;
/// The side of the stored copy: twice the 16 px it is drawn at.
const STORED_SIDE: u32 = 32;

/// What the dialog makes or changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A new folder in this one; the top level when `None`.
    New(Option<String>),
    /// The folder at this path.
    Edit(String),
}

pub struct FolderDialog {
    connections: Entity<Connections>,
    target: Target,
    name: Entity<InputState>,
    look: FolderLook,
    /// What is wrong with the name, from the last OK.
    problem: Option<String>,
    /// Why the picture picked last was not taken.
    picture_problem: Option<String>,
    /// The picture being read and copied.
    reading: Option<Task<()>>,
    picking: Option<Task<()>>,
}

/// Opens the dialog on `target`; it closes once the folder is saved.
pub fn open(
    connections: Entity<Connections>,
    target: Target,
    window: &mut Window,
    cx: &mut App,
) -> Entity<FolderDialog> {
    let title = match target {
        Target::New(_) => "New folder",
        Target::Edit(_) => "Edit folder",
    };
    let view = cx.new(|cx| FolderDialog::new(connections, target, window, cx));
    let shown = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let (view, on_enter) = (shown.clone(), shown.clone());
        dialog
            .title(title)
            .w(px(440.))
            .on_ok(move |_, _, cx| on_enter.update(cx, |this, cx| this.submit(cx)))
            .content(move |content, _, _| content.child(view.clone()))
    });
    let name = view.read(cx).name.clone();
    window.defer(cx, move |window, cx| {
        name.read(cx).focus_handle(cx).focus(window, cx)
    });
    view
}

impl FolderDialog {
    fn new(
        connections: Entity<Connections>,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (current, look) = match &target {
            Target::New(_) => (String::new(), FolderLook::default()),
            Target::Edit(path) => (
                kerno_core::config::last_folder_name(path).to_string(),
                AppState::get(cx)
                    .settings
                    .folder_looks
                    .get(path)
                    .cloned()
                    .unwrap_or_default(),
            ),
        };
        let name = cx.new(|cx| InputState::new(window, cx).default_value(current));
        Self {
            connections,
            target,
            name,
            look,
            problem: None,
            picture_problem: None,
            reading: None,
            picking: None,
        }
    }

    /// The look as it stands in the dialog.
    #[cfg(test)]
    pub fn look(&self) -> &FolderLook {
        &self.look
    }

    /// Whether a picture is being read.
    #[cfg(test)]
    pub fn reading(&self) -> bool {
        self.reading.is_some()
    }

    /// Makes or renames the folder and saves its look; says what is wrong
    /// and returns `false` to keep the dialog open.
    fn submit(&mut self, cx: &mut Context<Self>) -> bool {
        if self.reading.is_some() {
            return false;
        }
        let name = self.name.read(cx).value().to_string();
        let (target, look) = (self.target.clone(), self.look.clone());
        let saved = self.connections.update(cx, |c, cx| {
            let path = match &target {
                Target::New(parent) => c.create_folder(parent.as_deref(), &name, cx)?,
                Target::Edit(path) => {
                    c.rename_folder(path, &name, cx)?;
                    let name = folder_name(&name)?;
                    match parent_folder(path) {
                        Some(parent) => format!("{parent}/{name}"),
                        None => name,
                    }
                }
            };
            c.set_folder_look(&path, look, cx);
            Ok::<_, String>(())
        });
        self.problem = saved.err();
        cx.notify();
        self.problem.is_none()
    }

    fn choose_picture(&mut self, cx: &mut Context<Self>) {
        let picking = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        self.picking = Some(cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picking.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |this, cx| this.load_picture(path, cx));
            }
        }));
    }

    /// Takes the picture at `path`: copied, downscaled, off the UI thread.
    /// One that cannot be read says why and changes nothing.
    pub fn load_picture(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let dir = AppState::get(cx).paths.folder_icons();
        let storing = bridge::spawn_blocking(cx, move || store_folder_icon(&path, &dir));
        self.picture_problem = None;
        self.reading = Some(cx.spawn(async move |this, cx| {
            let stored = storing.await;
            let _ = this.update(cx, |this, cx| {
                this.reading = None;
                match stored {
                    Ok(file) => this.look.image = Some(file),
                    Err(problem) => this.picture_problem = Some(problem),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn row(label: &'static str, content: impl IntoElement, cx: &App) -> impl IntoElement {
        h_flex()
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .w(px(64.))
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(content)
    }
}

impl Render for FolderDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut swatches = Vec::new();
        for key in std::iter::once(None).chain(theme::TAG_COLORS.map(|(key, _, _)| Some(key))) {
            let id = SharedString::from(format!("folder-color-{}", key.unwrap_or("none")));
            swatches.push(
                theme::swatch(id, key, self.look.color.as_deref() == key, cx)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.look.color = key.map(str::to_string);
                        cx.notify();
                    }))
                    .test_support(),
            );
        }
        let glyphs = FOLDER_GLYPHS
            .iter()
            .enumerate()
            .map(|(index, (key, glyph))| {
                // The first is the plain folder: no key stored.
                let key = (index > 0).then_some(*key);
                let selected = self.look.image.is_none() && self.look.glyph.as_deref() == key;
                theme::chip(
                    SharedString::from(format!("folder-glyph-{}", key.unwrap_or("folder"))),
                    selected,
                )
                .small()
                .icon(*glyph)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.look.glyph = key.map(str::to_string);
                    this.look.image = None;
                    cx.notify();
                }))
            });
        let picture = self
            .look
            .image
            .as_ref()
            .map(|file| AppState::get(cx).paths.folder_icons().join(file));
        let reading = self.reading.is_some();
        let theme = cx.theme();
        let problem = |id: &'static str, text: &Option<String>| {
            text.clone().map(|text| {
                div()
                    .id(id)
                    .test_support()
                    .text_xs()
                    .text_color(theme.danger)
                    .child(SharedString::from(text))
            })
        };
        let picture_row = h_flex()
            .gap_2()
            .child(
                Button::new("folder-picture-choose")
                    .outline()
                    .small()
                    .label("Choose picture…")
                    .disabled(reading)
                    .on_click(cx.listener(|this, _, _, cx| this.choose_picture(cx))),
            )
            .when_some(picture, |this, picture| {
                this.child(
                    div()
                        .id("folder-picture")
                        .test_support()
                        .child(img(picture).size(px(16.))),
                )
                .child(
                    Button::new("folder-picture-remove")
                        .ghost()
                        .small()
                        .label("Remove")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.look.image = None;
                            cx.notify();
                        })),
                )
            })
            .when(reading, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Reading the picture…"),
                )
            });
        v_flex()
            .id("folder-dialog")
            .test_support()
            .gap_3()
            .pt_1()
            .child(Self::row(
                "Name",
                div().flex_1().child(Input::new(&self.name).id("name-input")),
                cx,
            ))
            .children(problem("name-error", &self.problem))
            .child(Self::row("Colour", h_flex().gap_1().children(swatches), cx))
            .child(Self::row("Icon", h_flex().gap_1().flex_wrap().children(glyphs), cx))
            .child(Self::row("Picture", picture_row, cx))
            .children(problem("folder-picture-error", &self.picture_problem))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("A picture (PNG, JPEG or SVG, up to 2 MB) is drawn instead of the icon, at 16 px. A colour tints the icon, or shows as a dot beside a picture."),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("name-cancel")
                            .ghost()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("name-ok")
                            .primary()
                            .label(match self.target {
                                Target::New(_) => "Create",
                                Target::Edit(_) => "Save",
                            })
                            .disabled(reading)
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.submit(cx) {
                                    window.close_dialog(cx);
                                }
                            })),
                    ),
            )
    }
}

/// Whether `name` is one Kerno gives a folder picture: 16 hex digits and
/// `.png`. Nothing else is drawn from, or removed in, the directory.
pub fn is_icon_name(name: &str) -> bool {
    name.strip_suffix(".png")
        .is_some_and(|hash| hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Copies the picture at `source` into `dir`, at most 32 px a side, as a
/// PNG named after a hash of its content (the same picture twice is one
/// file); returns that name, or what is wrong in words for the user.
pub fn store_folder_icon(source: &Path, dir: &Path) -> Result<String, String> {
    let shown = source.file_name().map_or_else(
        || source.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let svg = match source
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("svg") => true,
        Some("png" | "jpg" | "jpeg") => false,
        _ => return Err(format!("“{shown}” is not a PNG, JPEG or SVG file.")),
    };
    let unreadable = |error: std::io::Error| format!("“{shown}” could not be read: {error}.");
    // Read at most one byte past the limit, whatever the file claims.
    let mut bytes = Vec::new();
    std::fs::File::open(source)
        .and_then(|file| file.take(MAX_PICTURE_BYTES + 1).read_to_end(&mut bytes))
        .map_err(unreadable)?;
    if bytes.len() as u64 > MAX_PICTURE_BYTES {
        return Err(format!(
            "“{shown}” is larger than 2 MB: choose a smaller picture."
        ));
    }
    // A decoder that panics on a crafted file is a file that cannot be read.
    let picture = std::panic::catch_unwind(|| {
        if svg {
            rasterise_svg(&bytes)
        } else {
            decode_bitmap(&bytes)
        }
    })
    .ok()
    .flatten()
    .ok_or_else(|| format!("“{shown}” could not be read as a picture. Nothing was changed."))?;
    let mut png = Vec::new();
    picture
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .map_err(|error| format!("The picture could not be converted: {error}."))?;
    let mut hasher = DefaultHasher::new();
    png.hash(&mut hasher);
    let name = format!("{:016x}.png", hasher.finish());
    let not_saved = |error: std::io::Error| format!("The picture could not be saved: {error}.");
    std::fs::create_dir_all(dir).map_err(not_saved)?;
    std::fs::write(dir.join(&name), &png).map_err(not_saved)?;
    Ok(name)
}

/// Removes from `dir` the pictures Kerno named that no folder uses and
/// that were written before `before`.
pub fn remove_unused_icons(
    dir: &Path,
    used: &HashSet<String>,
    before: SystemTime,
) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        entries => entries?,
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_icon_name(name) && !used.contains(name) && entry.metadata()?.modified()? <= before {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// The limits every decoded picture is held to.
fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_PICTURE_SIDE);
    limits.max_image_height = Some(MAX_PICTURE_SIDE);
    limits.max_alloc = Some(128 * 1024 * 1024);
    limits
}

/// A PNG or JPEG file, decoded within [`limits`] and downscaled.
fn decode_bitmap(bytes: &[u8]) -> Option<RgbaImage> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    if !matches!(reader.format(), Some(ImageFormat::Png | ImageFormat::Jpeg)) {
        return None;
    }
    reader.limits(limits());
    let picture = reader.decode().ok()?;
    Some(picture.thumbnail(STORED_SIDE, STORED_SIDE).into_rgba8())
}

/// An SVG file drawn at [`STORED_SIDE`], proportions kept. It never reads
/// another file (an `<image href="/dev/zero">` would); pictures embedded in
/// it are drawn only when within the size limit.
fn rasterise_svg(bytes: &[u8]) -> Option<RgbaImage> {
    let mut options = usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(embedded_picture),
            resolve_string: Box::new(|_, _| None),
        },
        ..usvg::Options::default()
    };
    options.fontdb_mut().load_system_fonts();
    let tree = usvg::Tree::from_data(bytes, &options).ok()?;
    let size = tree.size();
    let scale = STORED_SIDE as f32 / size.width().max(size.height());
    let side = |length: f32| ((length * scale).round() as u32).clamp(1, STORED_SIDE);
    let (width, height) = (side(size.width()), side(size.height()));
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let pixels = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect();
    RgbaImage::from_raw(width, height, pixels)
}

/// A PNG or JPEG embedded in an SVG (a `data:` URL), when its declared
/// size is within the limit.
fn embedded_picture(_: &str, data: Arc<Vec<u8>>, _: &usvg::Options) -> Option<usvg::ImageKind> {
    let reader = ImageReader::new(Cursor::new(data.as_slice()))
        .with_guessed_format()
        .ok()?;
    let format = reader.format()?;
    let (width, height) = reader.into_dimensions().ok()?;
    if width > MAX_PICTURE_SIDE || height > MAX_PICTURE_SIDE {
        return None;
    }
    match format {
        ImageFormat::Png => Some(usvg::ImageKind::PNG(data)),
        ImageFormat::Jpeg => Some(usvg::ImageKind::JPEG(data)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(dir: &Path, name: &str) -> RgbaImage {
        assert!(is_icon_name(name), "{name}");
        image::open(dir.join(name)).unwrap().into_rgba8()
    }

    #[test]
    fn pictures_are_copied_small_under_a_name_of_kernos() {
        let source = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let icons = dir.path().join("folder-icons");

        // A large PNG is stored at 32 px.
        let png = source.path().join("Logo of the project.PNG");
        RgbaImage::from_pixel(1000, 500, image::Rgba([200, 240, 60, 255]))
            .save_with_format(&png, ImageFormat::Png)
            .unwrap();
        let name = store_folder_icon(&png, &icons).unwrap();
        assert_eq!(stored(&icons, &name).dimensions(), (32, 16));
        // The same picture again is the same file.
        assert_eq!(store_folder_icon(&png, &icons).unwrap(), name);

        // A JPEG, and an SVG drawn at 32 px without reading other files.
        let jpeg = source.path().join("photo.jpeg");
        image::RgbImage::from_pixel(64, 64, image::Rgb([10, 20, 30]))
            .save_with_format(&jpeg, ImageFormat::Jpeg)
            .unwrap();
        assert_eq!(
            stored(&icons, &store_folder_icon(&jpeg, &icons).unwrap()).dimensions(),
            (32, 32)
        );
        let svg = source.path().join("mark.svg");
        std::fs::write(
            &svg,
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100">
                   <rect width="200" height="100" fill="red"/>
                   <image href="{}" width="10" height="10"/></svg>"#,
                png.display()
            ),
        )
        .unwrap();
        let drawn = stored(&icons, &store_folder_icon(&svg, &icons).unwrap());
        assert_eq!(drawn.dimensions(), (32, 16));
        assert_eq!(drawn.get_pixel(16, 8).0, [255, 0, 0, 255]);
        // The `<image>` naming a file of this computer was not drawn.
        assert_eq!(drawn.get_pixel(1, 1).0, [255, 0, 0, 255]);
    }

    #[test]
    fn a_file_that_is_not_a_picture_changes_nothing() {
        let source = tempfile::tempdir().unwrap();
        let icons = source.path().join("folder-icons");
        let write = |name: &str, bytes: &[u8]| {
            let path = source.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        for (path, problem) in [
            (
                write("notes.txt", b"hello"),
                "is not a PNG, JPEG or SVG file",
            ),
            (
                write("fake.png", b"not a picture"),
                "could not be read as a picture",
            ),
            (
                write("broken.svg", b"<svg"),
                "could not be read as a picture",
            ),
            // A GIF named .png is refused too.
            (
                write("moving.png", b"GIF89a\x01\x00\x01\x00"),
                "could not be read as a picture",
            ),
            (
                write("huge.jpg", &vec![0; MAX_PICTURE_BYTES as usize + 1]),
                "larger than 2 MB",
            ),
            (source.path().join("gone.png"), "could not be read:"),
        ] {
            let error = store_folder_icon(&path, &icons).unwrap_err();
            assert!(error.contains(problem), "{}: {error}", path.display());
        }
        // A picture too large in pixels is not decoded.
        let wide = source.path().join("wide.png");
        RgbaImage::new(MAX_PICTURE_SIDE + 1, 1)
            .save_with_format(&wide, ImageFormat::Png)
            .unwrap();
        assert!(store_folder_icon(&wide, &icons).is_err());
        assert!(!icons.exists(), "nothing was written");
    }

    #[test]
    fn only_kernos_unused_and_older_pictures_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| dir.path().join(name);
        for name in ["00000000000000aa.png", "00000000000000bb.png", "logo.png"] {
            std::fs::write(path(name), b"x").unwrap();
        }
        let before = SystemTime::now();
        std::fs::write(path("00000000000000cc.png"), b"x").unwrap();
        let used = HashSet::from(["00000000000000aa.png".to_string()]);
        remove_unused_icons(dir.path(), &used, before).unwrap();
        assert!(path("00000000000000aa.png").exists(), "in use");
        assert!(!path("00000000000000bb.png").exists(), "unused");
        assert!(path("logo.png").exists(), "not a name of Kerno's");
        // Written after the call was made: a dialog may be about to use it.
        let newer = path("00000000000000cc.png");
        let written = std::fs::metadata(&newer).unwrap().modified().unwrap();
        assert_eq!(newer.exists(), written > before);
        // No directory yet: nothing to do.
        remove_unused_icons(&path("missing"), &used, before).unwrap();
    }
}
