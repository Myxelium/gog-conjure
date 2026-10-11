//! Downscaled separable blur of the window backdrop (CSS `blur(16px)` at full size).

use egui::{Color32, ColorImage, Pos2, Rect};

/// Full-resolution blur radius, in egui points.
pub const BLUR_PX: f32 = 16.0;

pub fn half_size(width: f32, height: f32) -> (usize, usize) {
    let w = (width * 0.5).round().max(1.0) as usize;
    let h = (height * 0.5).round().max(1.0) as usize;
    (w, h)
}

/// Sigma in the downscaled buffer so the blur reads as [`BLUR_PX`] once scaled back up.
pub fn downscaled_sigma(screen_width: f32, buffer_width: usize) -> f32 {
    if buffer_width == 0 || screen_width < 1.0 {
        return 0.0;
    }
    BLUR_PX * (buffer_width as f32) / screen_width
}

pub fn raster_gradient(
    w: usize,
    h: usize,
    nw: Color32,
    ne: Color32,
    se: Color32,
    sw: Color32,
) -> Vec<u8> {
    let mut out = vec![0u8; w.saturating_mul(h).saturating_mul(4)];
    if w == 0 || h == 0 {
        return out;
    }
    for y in 0..h {
        let ty = if h == 1 {
            0.0
        } else {
            y as f32 / (h - 1) as f32
        };
        for x in 0..w {
            let tx = if w == 1 {
                0.0
            } else {
                x as f32 / (w - 1) as f32
            };
            let top = lerp3(rgb(nw), rgb(ne), tx);
            let bot = lerp3(rgb(sw), rgb(se), tx);
            let c = lerp3(top, bot, ty);
            let i = (y * w + x) * 4;
            out[i] = c[0];
            out[i + 1] = c[1];
            out[i + 2] = c[2];
            out[i + 3] = 255;
        }
    }
    out
}

/// Cover-crop `img` into a `w`×`h` buffer, then darken by `black_alpha` (0–255).
pub fn raster_cover(img: &ColorImage, w: usize, h: usize, uv: Rect, black_alpha: u8) -> Vec<u8> {
    let mut out = vec![0u8; w.saturating_mul(h).saturating_mul(4)];
    if w == 0 || h == 0 || img.size[0] == 0 || img.size[1] == 0 {
        return out;
    }
    let keep = 255u16.saturating_sub(black_alpha as u16);
    for y in 0..h {
        let sy = (y as f32 + 0.5) / h as f32;
        let v = uv.min.y + sy * uv.height();
        for x in 0..w {
            let sx = (x as f32 + 0.5) / w as f32;
            let u = uv.min.x + sx * uv.width();
            let px = sample_bilinear(img, u, v);
            let i = (y * w + x) * 4;
            out[i] = (px[0] as u16 * keep / 255) as u8;
            out[i + 1] = (px[1] as u16 * keep / 255) as u8;
            out[i + 2] = (px[2] as u16 * keep / 255) as u8;
            out[i + 3] = 255;
        }
    }
    out
}

pub fn to_color_image(w: usize, h: usize, rgba: &[u8]) -> ColorImage {
    let mut pixels = Vec::with_capacity(w.saturating_mul(h));
    for px in rgba.chunks_exact(4) {
        pixels.push(Color32::from_rgba_unmultiplied(px[0], px[1], px[2], px[3]));
    }
    ColorImage {
        size: [w, h],
        pixels,
    }
}

/// Three-pass box blur approximating a Gaussian of `sigma` pixels in this buffer.
pub fn gaussian_blur(buf: &mut [u8], w: usize, h: usize, sigma: f32) {
    if w == 0 || h == 0 || sigma < 0.5 || buf.len() < w * h * 4 {
        return;
    }
    let mut tmp = vec![0u8; w * h * 4];
    for radius in box_radii(sigma) {
        box_blur_h(buf, &mut tmp, w, h, radius);
        box_blur_v(&tmp, buf, w, h, radius);
    }
}

fn rgb(c: Color32) -> [u8; 3] {
    [c.r(), c.g(), c.b()]
}

fn lerp3(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        lerp_u8(a[0], b[0], t),
        lerp_u8(a[1], b[1], t),
        lerp_u8(a[2], b[2], t),
    ]
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round() as u8
}

fn sample_bilinear(img: &ColorImage, u: f32, v: f32) -> [u8; 4] {
    let w = img.size[0];
    let h = img.size[1];
    let x = u.clamp(0.0, 1.0) * (w - 1) as f32;
    let y = v.clamp(0.0, 1.0) * (h - 1) as f32;
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let c00 = px_at(img, x0, y0);
    let c10 = px_at(img, x1, y0);
    let c01 = px_at(img, x0, y1);
    let c11 = px_at(img, x1, y1);
    let top = lerp4(c00, c10, tx);
    let bot = lerp4(c01, c11, tx);
    lerp4(top, bot, ty)
}

fn px_at(img: &ColorImage, x: usize, y: usize) -> [u8; 4] {
    let c = img.pixels[y * img.size[0] + x];
    [c.r(), c.g(), c.b(), c.a()]
}

fn lerp4(a: [u8; 4], b: [u8; 4], t: f32) -> [u8; 4] {
    [
        lerp_u8(a[0], b[0], t),
        lerp_u8(a[1], b[1], t),
        lerp_u8(a[2], b[2], t),
        lerp_u8(a[3], b[3], t),
    ]
}

/// Odd kernel widths from Ivan Kutskir's three-box Gaussian approximation.
fn box_radii(sigma: f32) -> [usize; 3] {
    let n = 3.0_f32;
    let w_ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut wl = w_ideal.floor();
    if (wl as i32) % 2 == 0 {
        wl -= 1.0;
    }
    if wl < 1.0 {
        wl = 1.0;
    }
    let wu = wl + 2.0;
    let m_ideal = (12.0 * sigma * sigma - n * wl * wl - 4.0 * n * wl - 3.0 * n) / (-4.0 * wl - 4.0);
    let m = m_ideal.round().clamp(0.0, n) as usize;
    let mut radii = [0usize; 3];
    for (i, radius) in radii.iter_mut().enumerate() {
        let width = if i < m { wl } else { wu };
        *radius = ((width as usize).saturating_sub(1)) / 2;
    }
    radii
}

fn box_blur_h(src: &[u8], dst: &mut [u8], w: usize, h: usize, radius: usize) {
    let r = radius as i32;
    let diam = (radius * 2 + 1) as u32;
    for y in 0..h {
        let mut sum = [0u32; 4];
        for k in -r..=r {
            add_px(&mut sum, src, (y * w + clamp_idx(k, w)) * 4);
        }
        for x in 0..w {
            write_avg(dst, (y * w + x) * 4, sum, diam);
            let remove = clamp_idx(x as i32 - r, w);
            let add = clamp_idx(x as i32 + r + 1, w);
            sub_px(&mut sum, src, (y * w + remove) * 4);
            add_px(&mut sum, src, (y * w + add) * 4);
        }
    }
}

fn box_blur_v(src: &[u8], dst: &mut [u8], w: usize, h: usize, radius: usize) {
    let r = radius as i32;
    let diam = (radius * 2 + 1) as u32;
    for x in 0..w {
        let mut sum = [0u32; 4];
        for k in -r..=r {
            add_px(&mut sum, src, (clamp_idx(k, h) * w + x) * 4);
        }
        for y in 0..h {
            write_avg(dst, (y * w + x) * 4, sum, diam);
            let remove = clamp_idx(y as i32 - r, h);
            let add = clamp_idx(y as i32 + r + 1, h);
            sub_px(&mut sum, src, (remove * w + x) * 4);
            add_px(&mut sum, src, (add * w + x) * 4);
        }
    }
}

fn clamp_idx(i: i32, len: usize) -> usize {
    if i <= 0 {
        0
    } else if i as usize >= len {
        len - 1
    } else {
        i as usize
    }
}

fn add_px(sum: &mut [u32; 4], src: &[u8], i: usize) {
    for c in 0..4 {
        sum[c] += src[i + c] as u32;
    }
}

fn sub_px(sum: &mut [u32; 4], src: &[u8], i: usize) {
    for c in 0..4 {
        sum[c] = sum[c].saturating_sub(src[i + c] as u32);
    }
}

fn write_avg(dst: &mut [u8], i: usize, sum: [u32; 4], diam: u32) {
    for c in 0..4 {
        dst[i + c] = (sum[c] / diam) as u8;
    }
}

pub fn screen_uv(screen: Rect, card: Rect) -> Rect {
    let w = screen.width().max(1.0);
    let h = screen.height().max(1.0);
    Rect::from_min_max(
        Pos2::new(
            (card.min.x - screen.min.x) / w,
            (card.min.y - screen.min.y) / h,
        ),
        Pos2::new(
            (card.max.x - screen.min.x) / w,
            (card.max.y - screen.min.y) / h,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_color_survives_blur() {
        let w = 24;
        let h = 16;
        let mut buf = vec![0u8; w * h * 4];
        for px in buf.chunks_exact_mut(4) {
            px.copy_from_slice(&[40, 80, 120, 255]);
        }
        gaussian_blur(&mut buf, w, h, 5.0);
        for px in buf.chunks_exact(4) {
            assert!((px[0] as i16 - 40).abs() <= 1);
            assert!((px[1] as i16 - 80).abs() <= 1);
            assert!((px[2] as i16 - 120).abs() <= 1);
            assert_eq!(px[3], 255);
        }
    }

    #[test]
    fn blur_spreads_a_spike_about_ten_pixels() {
        let w = 41;
        let h = 1;
        let mut buf = vec![0u8; w * h * 4];
        let mid = 20;
        buf[mid * 4] = 255;
        buf[mid * 4 + 3] = 255;
        gaussian_blur(&mut buf, w, h, 5.0);
        let at = |x: usize| buf[x * 4];
        assert!(at(mid) < 255, "center should diffuse");
        assert!(at(mid) >= at(mid + 1) && at(mid) >= at(mid - 1));
        assert!(at(mid + 2) > 0, "nearby pixels pick up energy");
        assert!(at(mid + 4) > at(mid + 12), "falloff with distance");
        assert_eq!(at(0), 0, "far pixels stay black");
        assert_eq!(at(mid - 4), at(mid + 4), "blur stays centered");
    }

    #[test]
    fn half_size_is_about_half_the_window() {
        assert_eq!(half_size(1180.0, 760.0), (590, 380));
        let (w, _) = half_size(1180.0, 760.0);
        let sigma = downscaled_sigma(1180.0, w);
        assert!((sigma - 8.0).abs() < 0.05);
    }

    #[test]
    fn screen_uv_maps_a_card_onto_the_window() {
        let screen = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(200.0, 100.0));
        let card = Rect::from_min_max(Pos2::new(50.0, 25.0), Pos2::new(150.0, 75.0));
        let uv = screen_uv(screen, card);
        assert!((uv.min.x - 0.25).abs() < 0.001);
        assert!((uv.min.y - 0.25).abs() < 0.001);
        assert!((uv.max.x - 0.75).abs() < 0.001);
        assert!((uv.max.y - 0.75).abs() < 0.001);
    }
}
