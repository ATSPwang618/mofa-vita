use std::io::{Read,Write};use std::sync::Once;use image::{DynamicImage,Rgba,RgbaImage,imageops::{self,FilterType}};
unsafe extern "C" {
    fn knife_bc7_init();
    fn knife_bc7_encode(rgba: *const u8, width: u32, height: u32, output: *mut u8);
}
static INIT: Once = Once::new();

fn encode_bc7(image: &RgbaImage) -> Vec<u8> {
    INIT.call_once(|| unsafe { knife_bc7_init() });
    let mut out = vec![0u8; image.width().div_ceil(4) as usize * image.height().div_ceil(4) as usize * 16];
    // SAFETY: RgbaImage owns width*height*4 bytes; out has precisely the padded BC7 block size.
    // The encoder's shared tables were initialized once, and all per-block scratch state is local.
    unsafe { knife_bc7_encode(image.as_ptr(), image.width(), image.height(), out.as_mut_ptr()) };
    out
}

fn resize_alpha(image: &RgbaImage, width: u32, height: u32) -> RgbaImage {
    if image.pixels().all(|p| p[3] == 255) {
        return imageops::resize(image, width, height, FilterType::Lanczos3);
    }
    let premultiplied = image::ImageBuffer::from_fn(image.width(), image.height(), |x, y| {
        let p = image.get_pixel(x, y);
        let a = f32::from(p[3]) / 255.0;
        // image's f32 resampler clamps every channel to [0, 1].
        Rgba([f32::from(p[0]) / 255.0 * a, f32::from(p[1]) / 255.0 * a, f32::from(p[2]) / 255.0 * a, a])
    });
    let scaled = imageops::resize(&premultiplied, width, height, FilterType::Lanczos3);
    RgbaImage::from_fn(width, height, |x, y| {
        let p = scaled.get_pixel(x, y);
        let a = p[3].clamp(0.0, 1.0);
        if a <= 1.0 / 510.0 { return Rgba([0, 0, 0, 0]); }
        Rgba([(p[0] / a * 255.0).round().clamp(0.0, 255.0) as u8,
            (p[1] / a * 255.0).round().clamp(0.0, 255.0) as u8,
            (p[2] / a * 255.0).round().clamp(0.0, 255.0) as u8, (a * 255.0).round() as u8])
    })
}

fn resize(image: &RgbaImage, scale: f32, strips: u32) -> RgbaImage {

    let width = ((image.width() as f32 * scale).round() as u32).max(1);
    let rows = image.height() / strips;
    let height = if strips > 1 {
        // Whole BC7 rows per language keep blocks and filtering from mixing adjacent translations.
        (((rows as f32 * scale / 4.0).round() as u32).max(1) * 4).min(rows)
    } else { ((rows as f32 * scale).round() as u32).max(1) };
    let mut out = RgbaImage::new(width, height * strips);
    for strip in 0..strips {
        let src = imageops::crop_imm(image, 0, rows * strip, image.width(), rows).to_image();
        let scaled = resize_alpha(&src, width, height);
        imageops::replace(&mut out, &scaled, 0, i64::from(height * strip));
    }
    out
}

fn main(){let mut input=std::io::stdin().lock();let mut output=std::io::stdout().lock();loop{let mut h=[0u8;20];if input.read_exact(&mut h).is_err(){break;}let u=|i:usize|u32::from_le_bytes(h[i..i+4].try_into().unwrap());let (w,ht,scale,strips,jpeg)=(u(0),u(4),f32::from_bits(u(8)),u(12),u(16));let mut data=vec![0;if jpeg==2{w as usize}else{w as usize*ht as usize*4}];input.read_exact(&mut data).unwrap();let im=if jpeg==2{image::load_from_memory(&data).unwrap().to_rgba8()}else{RgbaImage::from_raw(w,ht,data).unwrap()};let im=resize(&im,scale,strips);let (w,h)=(im.width(),im.height());let bytes=if jpeg!=0{let mut v=Vec::new();image::codecs::jpeg::JpegEncoder::new_with_quality(&mut v,92).encode_image(&DynamicImage::ImageRgba8(im)).unwrap();v}else{encode_bc7(&im)};for x in [w,h,bytes.len() as u32]{output.write_all(&x.to_le_bytes()).unwrap();}output.write_all(&bytes).unwrap();output.flush().unwrap();}}