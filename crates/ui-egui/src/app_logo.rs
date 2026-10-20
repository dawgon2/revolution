//! Revolution's own logo inside the app (fork, owner 2026-10-20, FORK-PLAN §1a.108): the
//! macOS-style app icon wherever DaVinci Resolve shows its mark (the page bar's name, the Project
//! Manager's title, its New Project tile and the tiles of projects without a poster, About).
//!
//! The picture is the 128 px hicolor icon (`assets/app-icon`, public domain, see its
//! `.attribution`), decoded once per egui context. A picture that can't be decoded draws nothing:
//! the logo is decoration, never a reason to fail.

use egui::{Color32, Context, Painter, Rect, TextureHandle, TextureId, pos2};

/// The macOS-style icon: the tile is 824 / 1024 of the picture, centred in a transparent margin.
const PNG: &[u8] = include_bytes!("../../../assets/app-icon/hicolor/128x128/apps/io.github.dawgon2.Revolution.png");

/// How much of the picture's side the tile itself covers (Apple's grid).
pub const TILE_FRACTION: f32 = 824.0 / 1024.0;

/// The logo's texture, decoded on first use and kept in the context's memory.
pub fn texture(ctx: &Context) -> Option<TextureId> {
    let id = egui::Id::new("revolution-app-logo");
    if let Some(t) = ctx.data(|d| d.get_temp::<Option<TextureHandle>>(id)) {
        return t.as_ref().map(TextureHandle::id);
    }
    let tex = decode().map(|img| ctx.load_texture("revolution-app-logo", img, egui::TextureOptions::LINEAR));
    let out = tex.as_ref().map(TextureHandle::id);
    ctx.data_mut(|d| d.insert_temp(id, tex));
    out
}

fn decode() -> Option<egui::ColorImage> {
    let img = image::load_from_memory_with_format(PNG, image::ImageFormat::Png).ok()?.to_rgba8();
    let size = [usize::try_from(img.width()).ok()?, usize::try_from(img.height()).ok()?];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()))
}

/// Paint the logo so its tile fills `rect` (the transparent margin and shadow spill over it),
/// tinted by `tint` (`Color32::WHITE` for the real colours; a grey fades it).
pub fn paint(ctx: &Context, painter: &Painter, rect: Rect, tint: Color32) {
    let Some(tex) = texture(ctx) else { return };
    let side = rect.width().min(rect.height()) / TILE_FRACTION;
    let r = Rect::from_center_size(rect.center(), egui::vec2(side, side));
    painter.image(tex, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint);
}

/// The app icon in Settings ▸ Appearance ▸ App icon's two styles (§1a.108), for the window icon
/// and the desktop's icon theme: `rounded` is the macOS-style tile (the packaged icon), `square`
/// the plain square picture.
const ROUNDED_512: &[u8] = include_bytes!("../../../assets/app-icon/filmcraft-macos-512.png");
const SQUARE_SOURCE: &[u8] = include_bytes!("../../../assets/app-icon/revolution-photo.png");

/// The app icon of `style` (`rounded` or `square`; anything else is `rounded`) as RGBA8 at
/// `size` × `size` (16 … 1024). `None` when it can't be made.
pub fn icon_rgba(style: &str, size: u32) -> Option<(u32, u32, Vec<u8>)> {
    let size = size.clamp(16, 1024);
    let src = if style == "square" { SQUARE_SOURCE } else { ROUNDED_512 };
    let img = image::load_from_memory_with_format(src, image::ImageFormat::Png).ok()?;
    let img = img.resize_exact(size, size, image::imageops::FilterType::Lanczos3).to_rgba8();
    Some((img.width(), img.height(), img.into_raw()))
}

/// [`icon_rgba`] as a PNG file's bytes.
pub fn icon_png(style: &str, size: u32) -> Option<Vec<u8>> {
    let (w, h, px) = icon_rgba(style, size)?;
    let img = image::RgbaImage::from_raw(w, h, px)?;
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
    Some(png)
}

#[cfg(test)]
mod tests {
    #[test]
    fn both_icon_styles_render() {
        for style in ["rounded", "square", "hostile"] {
            let (w, h, px) = super::icon_rgba(style, 48).expect(style);
            assert_eq!((w, h, px.len()), (48, 48, 48 * 48 * 4));
            assert!(super::icon_png(style, 32).is_some_and(|p| p.starts_with(b"\x89PNG")));
        }
        // the rounded tile has transparent corners, the square one doesn't
        let corner = |style: &str| super::icon_rgba(style, 64).map(|(_, _, px)| px[3]);
        assert_eq!(corner("rounded"), Some(0));
        assert_eq!(corner("square"), Some(255));
    }

    #[test]
    fn the_logo_decodes() {
        let img = super::decode().expect("the embedded logo decodes");
        assert_eq!(img.size, [128, 128]);
    }
}
