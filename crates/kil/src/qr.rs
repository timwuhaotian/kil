//! Turn an Agent on Edge pairing URL into a GPUI-renderable QR image.

use std::sync::Arc;

use gpui::{RenderImage, SharedString};
use image::{Frame, Rgba, RgbaImage};

/// Pixels per QR module; high enough for the iPhone camera at a glance.
const MODULE_SCALE: u32 = 8;
/// White modules around the code, as the QR spec expects.
const QUIET_MODULES: u32 = 4;

/// Renders a `kil://` pairing URL as a crisp RGBA bitmap.
pub fn qr_render_image(url: &str) -> Result<Arc<RenderImage>, SharedString> {
    let qr = qrcode::QrCode::new(url.as_bytes())
        .map_err(|err| SharedString::from(format!("QR generation failed: {err}")))?;
    let modules = qr.to_colors();
    let size =
        u32::try_from(qr.width()).map_err(|_| SharedString::from("QR is too large to render"))?;
    let dim = (size + QUIET_MODULES * 2) * MODULE_SCALE;
    let mut image = RgbaImage::from_pixel(dim, dim, Rgba([255, 255, 255, 255]));
    for y in 0..size {
        for x in 0..size {
            // A black module has RGB 0; white is 255.
            if modules[(y * size + x) as usize] != qrcode::Color::Dark {
                continue;
            }
            for dy in 0..MODULE_SCALE {
                for dx in 0..MODULE_SCALE {
                    image.put_pixel(
                        (x + QUIET_MODULES) * MODULE_SCALE + dx,
                        (y + QUIET_MODULES) * MODULE_SCALE + dy,
                        Rgba([0, 0, 0, 255]),
                    );
                }
            }
        }
    }
    Ok(Arc::new(RenderImage::new(vec![Frame::new(image)])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pairing_url_renders_a_square_rgba_image() {
        let image = qr_render_image("kil://192.168.0.1:1234?code=ABCDEF&fp=sha256:abcd&v=1")
            .expect("a valid pairing URL renders");
        let size = image.size(0);
        assert_eq!(size.width.0, size.height.0);
        assert!(size.width.0 > 0);
        assert!(image.as_bytes(0).is_some(), "the QR has pixel data");
    }
}
