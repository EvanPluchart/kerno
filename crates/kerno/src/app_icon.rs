//! Kerno's icon when the bare binary runs, outside `Kerno.app` (CLAUDE.md
//! §3, owner's test of 2026-11-13, item 4): the Dock and the app switcher on
//! macOS, the window on Linux. The files are those of `assets/icon/`, the
//! ones the bundle scripts install.

/// The icon for the Dock and the app switcher: the 256 px drawing, shown
/// at 128 points (see [`DOCK_POINTS`]).
#[cfg(any(target_os = "macos", test))]
const DOCK_PNG: &[u8] = include_bytes!("../../../assets/icon/kerno-256.png");

/// The size the icon is given, in points: the Dock's largest tile, 256 px
/// on a Retina screen. AppKit keeps the icon rendered at twice the image's
/// size in points, whatever the file holds: the 1,024 px `.icns` (512
/// points) cost 8 MB of rendered pixels, and about 10 MB more of idle
/// memory than the 512 px PNG; this, 0.5 MB of rendered pixels.
#[cfg(target_os = "macos")]
const DOCK_POINTS: f64 = 128.0;

/// The window icon on Linux (X11; Wayland reads the desktop entry's).
#[cfg(any(target_os = "linux", test))]
const PNG_64: &[u8] = include_bytes!("../../../assets/icon/kerno-64.png");

/// The app's icon as AppKit draws it, or `None` when the file does not
/// decode.
#[cfg(target_os = "macos")]
fn dock_icon_image() -> Option<objc2::rc::Retained<objc2_app_kit::NSImage>> {
    use objc2::AllocAnyThread as _;
    use objc2_app_kit::NSImage;
    use objc2_foundation::{NSData, NSSize};
    let image = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(DOCK_PNG))?;
    image.setSize(NSSize::new(DOCK_POINTS, DOCK_POINTS));
    Some(image)
}

/// Shows Kerno's icon in the Dock and the app switcher when the bare binary
/// runs. Inside an app bundle the system already shows the bundle's icon:
/// nothing is done there. Off the main thread it does nothing.
#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "objc2-app-kit marks `setApplicationIconImage` unsafe, and no safe call in the \
              locked crates sets the application icon (docs/DECISIONS.md D-042)"
)]
pub fn set_dock_icon() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    if std::env::current_exe().is_ok_and(|exe| in_app_bundle(&exe)) {
        return;
    }
    let Some(image) = dock_icon_image() else {
        tracing::warn!("the app icon did not decode; the Dock keeps the default one");
        return;
    };
    let app = NSApplication::sharedApplication(main_thread);
    // SAFETY: the method's one documented condition is that the image "might
    // not allow `None`"; a valid, retained `NSImage` is passed, never `None`.
    // AppKit is only touched on the main thread (`MainThreadMarker` proves
    // it), and the image outlives the call (AppKit retains what it keeps).
    unsafe { app.setApplicationIconImage(Some(&image)) };
}

/// Whether `exe` is the binary of an app bundle: `<name>.app/Contents/MacOS/<exe>`.
#[cfg(any(target_os = "macos", test))]
fn in_app_bundle(exe: &std::path::Path) -> bool {
    let mut up = exe.ancestors().skip(1).map(std::path::Path::file_name);
    up.next() == Some(Some("MacOS".as_ref()))
        && up.next() == Some(Some("Contents".as_ref()))
        && up
            .next()
            .flatten()
            .and_then(|bundle| std::path::Path::new(bundle).extension())
            == Some("app".as_ref())
}

/// The window icon on Linux, decoded from the embedded PNG.
#[cfg(any(target_os = "linux", test))]
pub fn window_icon() -> Option<std::sync::Arc<image::RgbaImage>> {
    image::load_from_memory_with_format(PNG_64, image::ImageFormat::Png)
        .ok()
        .map(|icon| std::sync::Arc::new(icon.into_rgba8()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_icons_decode() {
        let icon = window_icon().expect("the Linux window icon decodes");
        assert_eq!(icon.dimensions(), (64, 64));
        assert!(DOCK_PNG.starts_with(b"\x89PNG"), "a PNG file");
        #[cfg(target_os = "macos")]
        {
            let image = dock_icon_image().expect("AppKit decodes the icon");
            let size = image.size();
            assert_eq!((size.width, size.height), (DOCK_POINTS, DOCK_POINTS));
        }
    }

    #[test]
    fn only_the_binary_of_an_app_bundle_is_in_one() {
        use std::path::Path;
        assert!(in_app_bundle(Path::new(
            "/Applications/Kerno.app/Contents/MacOS/kerno"
        )));
        assert!(in_app_bundle(Path::new(
            "/tmp/Renamed.app/Contents/MacOS/kerno"
        )));
        assert!(!in_app_bundle(Path::new("/repo/target/release/kerno")));
        assert!(!in_app_bundle(Path::new("/tmp/Contents/MacOS/kerno")));
        assert!(!in_app_bundle(Path::new("kerno")));
    }
}
