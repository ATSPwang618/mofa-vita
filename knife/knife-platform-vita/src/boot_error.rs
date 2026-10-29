//! Asset-independent startup diagnostics, rasterized once at 480x270.
use crate::asset_check::AssetError;

pub const WIDTH: usize = 480;
pub const HEIGHT: usize = 270;

#[cfg(not(feature = "assets-540p"))]
const PACK_DESCRIPTION: &str = "THIS ENGINE USES THE 480 X 270 ASSET PACK.";
#[cfg(feature = "assets-540p")]
const PACK_DESCRIPTION: &str = "THIS ENGINE USES THE 960 X 540 ASSET PACK.";
#[cfg(not(feature = "assets-540p"))]
const ASSETS_ROOT: &str = "ux0:data/knife-woh/assets";
#[cfg(feature = "assets-540p")]
const ASSETS_ROOT: &str = "ux0:data/knife-woh-540p/assets";

pub fn raster(error: &AssetError) -> Vec<u8> {
    let mut pixels = [14, 18, 25, 255].repeat(WIDTH * HEIGHT);
    text(&mut pixels, 16, 22, 2, "ASSETS REQUIRED", [246, 200, 105, 255]);
    text(&mut pixels, 16, 64, 2, error.description(), [246, 246, 246, 255]);
    text(&mut pixels, 16, 92, 1, error.path, [246, 200, 105, 255]);
    for (y, line) in [
        (128, PACK_DESCRIPTION),
        (144, "COPY ITS COMPLETE ASSETS FOLDER TO:"),
        (160, ASSETS_ROOT),
        (184, "DO NOT MIX PACKS OR TITLE VERSIONS."),
        (230, "PRESS CROSS OR CIRCLE TO EXIT"),
    ] { text(&mut pixels, 16, y, 1, line, [220, 225, 233, 255]); }
    pixels
}

fn text(pixels: &mut [u8], x: usize, y: usize, scale: usize, line: &str, color: [u8; 4]) {
    for (character, ch) in line.bytes().enumerate() {
        let x = x + character * 6 * scale;
        if x + 5 * scale > WIDTH { break; }
        for (row, bits) in glyph(ch.to_ascii_uppercase()).iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) == 0 { continue; }
                for dy in 0..scale {
                    let yy = y + row * scale + dy;
                    if yy >= HEIGHT { continue; }
                    for dx in 0..scale {
                        let offset = (yy * WIDTH + x + col * scale + dx) * 4;
                        pixels[offset..offset + 4].copy_from_slice(&color);
                    }
                }
            }
        }
    }
}

// Built-in ASCII 5x7 diagnostic glyphs; no game fonts or image assets needed.
fn glyph(ch: u8) -> [u8; 7] {
    match ch {
        b'A' => [14,17,17,31,17,17,17], b'B' => [30,17,17,30,17,17,30],
        b'C' => [14,17,16,16,16,17,14], b'D' => [30,17,17,17,17,17,30],
        b'E' => [31,16,16,30,16,16,31], b'F' => [31,16,16,30,16,16,16],
        b'G' => [14,17,16,23,17,17,15], b'H' => [17,17,17,31,17,17,17],
        b'I' => [14,4,4,4,4,4,14], b'J' => [7,2,2,2,18,18,12],
        b'K' => [17,18,20,24,20,18,17], b'L' => [16,16,16,16,16,16,31],
        b'M' => [17,27,21,21,17,17,17], b'N' => [17,25,21,19,17,17,17],
        b'O' => [14,17,17,17,17,17,14], b'P' => [30,17,17,30,16,16,16],
        b'Q' => [14,17,17,17,21,18,13], b'R' => [30,17,17,30,20,18,17],
        b'S' => [15,16,16,14,1,1,30], b'T' => [31,4,4,4,4,4,4],
        b'U' => [17,17,17,17,17,17,14], b'V' => [17,17,17,17,17,10,4],
        b'W' => [17,17,17,21,21,21,10], b'X' => [17,17,10,4,10,17,17],
        b'Y' => [17,17,10,4,4,4,4], b'Z' => [31,1,2,4,8,16,31],
        b'0' => [14,17,19,21,25,17,14], b'1' => [4,12,4,4,4,4,14],
        b'2' => [14,17,1,2,4,8,31], b'3' => [30,1,1,14,1,1,30],
        b'4' => [2,6,10,18,31,2,2], b'5' => [31,16,16,30,1,1,30],
        b'6' => [14,16,16,30,17,17,14], b'7' => [31,1,2,4,8,8,8],
        b'8' => [14,17,17,14,17,17,14], b'9' => [14,17,17,15,1,1,14],
        b'/' => [1,2,2,4,8,8,16], b':' => [0,4,4,0,4,4,0],
        b'.' => [0,0,0,0,0,12,12], b'-' => [0,0,0,31,0,0,0],
        b'_' => [0,0,0,0,0,0,31], b' ' => [0; 7],
        _ => [14,17,1,2,4,0,4],
    }
}

#[cfg(any(test, target_os = "vita"))]
#[derive(Default)]
struct Dismiss { released: bool }
#[cfg(any(test, target_os = "vita"))]
impl Dismiss {
    fn update(&mut self, buttons: u32) -> bool {
        let held = buttons & 0x6000 != 0; // Cross or Circle, either region mapping.
        if !held { self.released = true; }
        self.released && held
    }
}

#[cfg(target_os = "vita")]
pub fn show(error: &AssetError, dev_frame_limit: Option<u64>) -> Result<(), String> {
    use crate::gxm::Gxm;
    #[repr(C)]
    #[derive(Default)]
    struct CtrlData { timestamp: u64, buttons: u32, axes: [u8; 4], reserved: [u8; 16] }
    const _: () = assert!(std::mem::size_of::<CtrlData>() == 32);
    unsafe extern "C" {
        fn sceCtrlPeekBufferPositive(port: i32, data: *mut CtrlData, count: i32) -> i32;
    }
    let mut gxm = Gxm::new()?;
    let pixels = raster(error);
    let texture = gxm.create_texture(&pixels, WIDTH as u32, HEIGHT as u32, (WIDTH * 4) as u32)?;
    drop(pixels); // No CPU raster copy retained while the error screen is open.
    let mut dismiss = Dismiss::default();
    let mut frame = 0;
    loop {
        gxm.begin([14. / 255., 18. / 255., 25. / 255., 1.])?;
        gxm.draw(Some(&texture), [0., 0., 960., 540.], [0., 0., 1., 1.], [1.; 4], false)?;
        gxm.present()?;
        frame += 1;
        let mut controller = CtrlData::default();
        // SAFETY: native one-record controller ABI, live mutable output, no callbacks.
        let status = unsafe { sceCtrlPeekBufferPositive(0, &mut controller, 1) };
        if status < 0 { return Err(format!("startup controller read: {status:#x}")); }
        if status == 1 && dismiss.update(controller.buttons)
            || dev_frame_limit.is_some_and(|limit| frame >= limit) { break; }
        std::thread::sleep(std::time::Duration::from_millis(33));
    }
    gxm.close()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset_check::Failure;

    #[test]
    fn error_canvas_is_bounded_opaque_and_has_readable_glyphs() {
        let pixels = raster(&AssetError { path: "allui.hed", failure: Failure::Content });
        assert_eq!(pixels.len(), 480 * 270 * 4);
        assert!(pixels.chunks_exact(4).all(|p| p[3] == 255));
        // Top-left pixel of title glyph '2' is blank; next pixel is lit (5x7).
        let sample = |x: usize, y: usize| &pixels[(y * WIDTH + x) * 4..][..4];
        assert_eq!(sample(16, 22), [14, 18, 25, 255]);
        assert_eq!(sample(18, 22), [246, 200, 105, 255]);
        assert_ne!(pixels, raster(&AssetError { path: "allui.hed", failure: Failure::Missing }));
        assert_ne!(pixels, raster(&AssetError { path: "parts.hed", failure: Failure::Content }));
    }

    #[test]
    fn held_launch_button_cannot_dismiss_error_until_released_and_pressed_again() {
        let mut input = Dismiss::default();
        assert!(!input.update(0x4000));
        assert!(!input.update(0x4000));
        assert!(!input.update(0));
        assert!(!input.update(0x10));
        assert!(input.update(0x2000));
    }
}
