//! Pictures: PNG decoding to premultiplied RGBA and box-filter scaling.
use crate::canvas::Image;

/// PNG to premultiplied RGBA.
pub fn decode_png(data: &[u8]) -> Option<Image> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(data));
    dec.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as i32, info.height as i32);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    let channels = info.color_type.samples();
    for px in buf[..info.buffer_size()].chunks(channels) {
        let (r, g, b, a) = match channels {
            4 => (px[0], px[1], px[2], px[3]),
            2 => (px[0], px[0], px[0], px[1]),
            3 => (px[0], px[1], px[2], 255),
            _ => (px[0], px[0], px[0], 255),
        };
        let pm = |c: u8| ((c as u32 * a as u32 + 127) / 255) as u8;
        out.extend_from_slice(&[pm(r), pm(g), pm(b), a]);
    }
    Some(Image { w, h, data: out })
}

/// Box-filter scale of a premultiplied image to fit (w, h), keeping aspect.
pub fn scale_to_fit(img: &Image, w: i32, h: i32) -> Image {
    if img.w <= w && img.h <= h {
        return Image {
            w: img.w,
            h: img.h,
            data: img.data.clone(),
        };
    }
    let s = (w as f32 / img.w as f32).min(h as f32 / img.h as f32);
    let nw = ((img.w as f32 * s).round() as i32).max(1);
    let nh = ((img.h as f32 * s).round() as i32).max(1);
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for y in 0..nh {
        let sy0 = y * img.h / nh;
        let sy1 = ((y + 1) * img.h / nh).max(sy0 + 1);
        for x in 0..nw {
            let sx0 = x * img.w / nw;
            let sx1 = ((x + 1) * img.w / nw).max(sx0 + 1);
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for sy in sy0..sy1 {
                let row = (sy * img.w) as usize * 4;
                for sx in sx0..sx1 {
                    let i = row + sx as usize * 4;
                    for (a, v) in acc.iter_mut().zip(&img.data[i..i + 4]) {
                        *a += *v as u32;
                    }
                    n += 1;
                }
            }
            let o = ((y * nw + x) * 4) as usize;
            for (dst, a) in out[o..o + 4].iter_mut().zip(acc) {
                *dst = (a / n.max(1)) as u8;
            }
        }
    }
    Image {
        w: nw,
        h: nh,
        data: out,
    }
}
