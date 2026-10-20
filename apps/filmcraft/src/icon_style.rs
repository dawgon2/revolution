//! Settings ▸ Appearance ▸ App icon (fork, owner 2026-10-20, FORK-PLAN §1a.108): the macOS-style
//! rounded tile by default (what the packages install), or the plain square picture.
//!
//! The window icon follows at once everywhere. On Linux the desktop takes the launcher, taskbar
//! and app-switcher icon from the icon theme by app id, so the chosen style is also written to the
//! user's hicolor theme (`$XDG_DATA_HOME/icons/hicolor/<n>x<n>/apps/<app id>.png`), which the
//! desktop looks up before the system's packaged icons. Both styles are written the same way, so
//! switching back works for a source-tree or tarball install whose only icons live there. A
//! Flatpak can't write outside its sandbox: there only the window icon changes. Every failure is
//! logged, never fatal.

use std::sync::Arc;

/// The sizes written to the user's icon theme (the desktop picks the nearest).
const SIZES: [u32; 6] = [32, 48, 64, 128, 256, 512];

/// The window icon in `style`.
pub fn window_icon(style: &str) -> egui::IconData {
    match filmcraft_ui_egui::app_logo::icon_rgba(style, 256) {
        Some((width, height, rgba)) => egui::IconData { rgba, width, height },
        None => egui::IconData::default(),
    }
}

/// Apply a changed App icon setting: the window icon now, and the desktop's icon on Linux.
pub fn apply(ctx: &egui::Context, style: &str) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(Arc::new(window_icon(style)))));
    #[cfg(target_os = "linux")]
    if std::env::var_os("FLATPAK_ID").is_none() {
        let written = user_data_dir().ok_or_else(|| std::io::Error::other("no home folder")).and_then(|d| write_theme_icons(&d, style));
        if let Err(e) = written {
            log::warn!("app icon: the desktop's icon was not changed: {e}");
        }
    }
}

/// Write `style` into the hicolor icon theme of the data folder `data` under the app id.
#[cfg(target_os = "linux")]
fn write_theme_icons(data: &std::path::Path, style: &str) -> std::io::Result<()> {
    let hicolor = data.join("icons").join("hicolor");
    for size in SIZES {
        let Some(png) = filmcraft_ui_egui::app_logo::icon_png(style, size) else { continue };
        let dir = hicolor.join(format!("{size}x{size}")).join("apps");
        std::fs::create_dir_all(&dir)?;
        let dest = dir.join(format!("{}.png", crate::APP_ID));
        let tmp = dest.with_extension("png.tmp");
        std::fs::write(&tmp, png)?;
        std::fs::rename(&tmp, &dest)?;
    }
    // icon caches (GTK, KDE's) notice a changed theme folder by its modification time
    let now = std::fs::FileTimes::new().set_modified(std::time::SystemTime::now());
    if let Ok(f) = std::fs::File::open(&hicolor) {
        let _ = f.set_times(now);
    }
    Ok(())
}

/// `$XDG_DATA_HOME`, else `$HOME/.local/share`; only absolute paths (an untrusted environment
/// must never make us write into the working directory).
#[cfg(target_os = "linux")]
fn user_data_dir() -> Option<std::path::PathBuf> {
    let abs = |v: std::ffi::OsString| Some(std::path::PathBuf::from(v)).filter(|p| p.is_absolute());
    std::env::var_os("XDG_DATA_HOME").and_then(abs).or_else(|| std::env::var_os("HOME").and_then(abs).map(|h| h.join(".local").join("share")))
}

#[cfg(test)]
mod tests {
    #[test]
    fn both_window_icons_are_square_rgba() {
        for style in ["rounded", "square"] {
            let i = super::window_icon(style);
            assert_eq!((i.width, i.height, i.rgba.len()), (256, 256, 256 * 256 * 4), "{style}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn theme_icons_go_under_the_data_folder() {
        let dir = std::env::temp_dir().join(format!("revolution-icon-test-{}", std::process::id()));
        super::write_theme_icons(&dir, "square").unwrap();
        let f = dir.join(format!("icons/hicolor/256x256/apps/{}.png", crate::APP_ID));
        assert!(f.is_file(), "{}", f.display());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
