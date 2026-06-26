use std::error::Error;

use image::{imageops, GrayImage, Luma};

// number of evenly-spaced gray levels the panel can show (4-bit).
const LEVELS: f32 = 16.0;

// floyd-steinberg dithering to `LEVELS` evenly-spaced gray levels. this is the
// 1-bit dither from waveshare-epaper/server/src/render.rs generalised from 2
// levels to 16 so it uses the LilyGo panel's full grayscale range.
fn dither_levels(img: &GrayImage) -> GrayImage {
    let (w, h) = img.dimensions();
    let mut px: Vec<f32> = img.pixels().map(|p| p.0[0] as f32).collect();
    let step = 255.0 / (LEVELS - 1.0);

    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) as usize;
            let old = px[idx];
            let new = (old / step).round().clamp(0.0, LEVELS - 1.0) * step;
            px[idx] = new;
            let err = old - new;

            if x + 1 < w {
                px[idx + 1] += err * 7.0 / 16.0;
            }
            if y + 1 < h {
                if x > 0 {
                    px[idx + w as usize - 1] += err * 3.0 / 16.0;
                }
                px[idx + w as usize] += err * 5.0 / 16.0;
                if x + 1 < w {
                    px[idx + w as usize + 1] += err * 1.0 / 16.0;
                }
            }
        }
    }

    let mut out = GrayImage::new(w, h);
    for (pixel, value) in out.pixels_mut().zip(px) {
        *pixel = Luma([value.clamp(0.0, 255.0) as u8]);
    }
    out
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: {} <input-image> <output.bmp> [WxH]", args[0]);
        std::process::exit(2);
    }

    let (w, h) = match args.get(3) {
        Some(dim) => {
            let (ws, hs) = dim
                .split_once('x')
                .ok_or("dimensions must look like 540x960")?;
            (ws.parse()?, hs.parse()?)
        }
        None => (540u32, 960u32),
    };

    let gray = image::open(&args[1])?
        .resize_to_fill(w, h, imageops::FilterType::Lanczos3)
        .to_luma8();
    let dithered = dither_levels(&gray);

    // save as a 24-bit grayscale BMP (R=G=B). the device reads it as Gray4 via
    // tinybmp, and the evenly-spaced levels map cleanly with luma >> 4.
    let mut rgb = image::RgbImage::new(w, h);
    for (out, src) in rgb.pixels_mut().zip(dithered.pixels()) {
        let v = src.0[0];
        *out = image::Rgb([v, v, v]);
    }
    rgb.save_with_format(&args[2], image::ImageFormat::Bmp)?;

    println!(
        "wrote {} — {}x{}, {} gray levels (Floyd-Steinberg)",
        args[2], w, h, LEVELS as u32
    );
    Ok(())
}
