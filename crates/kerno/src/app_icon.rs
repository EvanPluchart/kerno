//! Kerno's icon when the bare binary runs, outside `Kerno.app` (CLAUDE.md
//! §3, owner's test of 2026-11-13, item 4): the Dock and the app switcher on
//! macOS, the window on Linux. The files are those of `assets/icon/`, the
//! ones the bundle scripts install.

/// The icon file `scripts/bundle-macos.sh` copies into the bundle.
#[cfg(any(target_os = "macos", test))]
const ICNS: &[u8] = include_bytes!("../../../assets/icon/kerno.icns");

/// The window icon on Linux (X11; Wayland reads the desktop entry's).
#[cfg(any(target_os = "linux", test))]
const PNG_64: &[u8] = include_bytes!("../../../assets/icon/kerno-64.png");

/// The app's icon as AppKit draws it, or `None` when the file does not
/// decode.
#[cfg(target_os = "macos")]
fn dock_icon_image() -> Option<objc2::rc::Retained<objc2_app_kit::NSImage>> {
    use objc2::AllocAnyThread as _;
    use objc2_app_kit::NSImage;
    use objc2_foundation::NSData;
    NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(ICNS))
}

/// Shows Kerno's icon in the Dock and the app switcher. Inside `Kerno.app`
/// AppKit already shows the bundle's icon; setting the same image again is
/// harmless, so this runs every time. Off the main thread it does nothing.
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
        assert!(ICNS.starts_with(b"icns"), "an .icns file");
        #[cfg(target_os = "macos")]
        {
            let image = dock_icon_image().expect("AppKit decodes the .icns file");
            let size = image.size();
            assert!(size.width > 0.0 && size.height > 0.0, "{size:?}");
        }
    }
}
