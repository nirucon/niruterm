//! The terminal font: regular, bold, italic and bold italic of one monospace
//! family, a fallback font per missing character (found once through
//! fontconfig), and glyph masks cached per (character, style). Box drawing,
//! block elements and powerline arrows are drawn as exact pixels instead, so
//! lines join cell to cell without gaps at any size.
use crate::canvas::{Canvas, Rect};
use ab_glyph::{point, Font, FontArc, PxScale, ScaleFont};
use std::collections::HashMap;

pub struct Mask {
    pub w: i32,
    pub h: i32,
    pub x: i32,
    pub y: i32,
    pub coverage: Vec<u8>,
}

pub struct Fonts {
    faces: [FontArc; 4],
    fallback: HashMap<char, Option<FontArc>>,
    loaded: HashMap<String, FontArc>,
    cache: HashMap<(char, u8), Option<Mask>>,
    pub px: f32,
    pub cell_w: i32,
    pub cell_h: i32,
    pub ascent: i32,
    family: String,
}

const DEFAULT_FAMILY: &str = "JetBrainsMono Nerd Font,JetBrains Mono,DejaVu Sans Mono";

fn fc_match(pattern: &str) -> Option<String> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", pattern])
        .output()
        .ok()?;
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !p.is_empty()).then_some(p)
}

fn open(path: &str) -> Option<FontArc> {
    FontArc::try_from_vec(std::fs::read(path).ok()?).ok()
}

/// Pixels per point from Xft.dpi when set, else 96 dpi.
pub fn dpi_scale(xft_dpi: Option<f32>) -> f32 {
    xft_dpi.unwrap_or(96.0) / 72.0
}

impl Fonts {
    pub fn new(
        family: Option<&str>,
        points: f32,
        dpi: f32,
        line_height: f32,
    ) -> Result<Self, String> {
        let family = family.unwrap_or(DEFAULT_FAMILY).to_string();
        let mut loaded: HashMap<String, FontArc> = HashMap::new();
        let mut face = |style: &str| -> Result<FontArc, String> {
            let pattern = format!("{family}:style={style}");
            let path = fc_match(&pattern)
                .or_else(|| fc_match("monospace"))
                .or_else(|| Some("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf".into()))
                .unwrap();
            if let Some(f) = loaded.get(&path) {
                return Ok(f.clone());
            }
            let f = open(&path).ok_or_else(|| format!("cannot open font {path}"))?;
            loaded.insert(path, f.clone());
            Ok(f)
        };
        let faces = [
            face("Regular")?,
            face("Bold")?,
            face("Italic")?,
            face("Bold Italic")?,
        ];
        let mut f = Fonts {
            faces,
            fallback: HashMap::new(),
            loaded,
            cache: HashMap::new(),
            px: 0.0,
            cell_w: 1,
            cell_h: 1,
            ascent: 0,
            family,
        };
        f.set_size(points * dpi, line_height);
        Ok(f)
    }

    pub fn set_size(&mut self, px: f32, line_height: f32) {
        self.px = px.clamp(6.0, 120.0);
        let s = self.faces[0].as_scaled(PxScale::from(self.px));
        self.cell_w = s.h_advance(self.faces[0].glyph_id('M')).round().max(1.0) as i32;
        let h = (s.ascent() - s.descent() + s.line_gap()) * line_height;
        self.cell_h = h.round().max(1.0) as i32;
        self.ascent =
            (s.ascent() + (h - (s.ascent() - s.descent() + s.line_gap())) / 2.0).round() as i32;
        self.cache.clear();
    }

    fn face_for(&mut self, c: char, style: u8) -> Option<FontArc> {
        let f = &self.faces[style as usize & 3];
        if f.glyph_id(c).0 != 0 {
            return Some(f.clone());
        }
        if self.faces[0].glyph_id(c).0 != 0 {
            return Some(self.faces[0].clone());
        }
        if let Some(hit) = self.fallback.get(&c) {
            return hit.clone();
        }
        // fontconfig picks a font that has this character.
        let found = fc_match(&format!(
            "{}:charset={:x}",
            self.family.split(',').next().unwrap_or("monospace"),
            c as u32
        ))
        .and_then(|path| {
            if let Some(f) = self.loaded.get(&path) {
                return Some(f.clone());
            }
            let f = open(&path)?;
            self.loaded.insert(path, f.clone());
            Some(f)
        })
        .filter(|f| f.glyph_id(c).0 != 0);
        self.fallback.insert(c, found.clone());
        found
    }

    /// Draw one character in a cell (or two for a wide one).
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        cv: &mut Canvas,
        x: i32,
        y: i32,
        c: char,
        style: u8,
        color: u32,
        cells: i32,
    ) {
        if c == ' ' || c == '\0' {
            return;
        }
        if draw_special(cv, x, y, self.cell_w * cells, self.cell_h, c, color) {
            return;
        }
        let key = (c, style);
        if !self.cache.contains_key(&key) {
            let m = self.rasterise(c, style, cells);
            self.cache.insert(key, m);
        }
        if let Some(Some(m)) = self.cache.get(&key) {
            cv.mask(x + m.x, y + self.ascent + m.y, m.w, m.h, &m.coverage, color);
        }
    }

    fn rasterise(&mut self, c: char, style: u8, cells: i32) -> Option<Mask> {
        let face = self.face_for(c, style)?;
        let mut px = self.px;
        // A wide glyph from a proportional fallback is fitted to its cells.
        let s = face.as_scaled(PxScale::from(px));
        let adv = s.h_advance(face.glyph_id(c));
        let room = (self.cell_w * cells) as f32;
        if adv > room * 1.05 {
            px *= room / adv;
        }
        let glyph = face
            .glyph_id(c)
            .with_scale_and_position(PxScale::from(px), point(0.0, 0.0));
        let outline = face.outline_glyph(glyph)?;
        let b = outline.px_bounds();
        let (w, h) = (b.width().ceil() as i32, b.height().ceil() as i32);
        if w <= 0 || h <= 0 {
            return None;
        }
        let mut coverage = vec![0u8; (w * h) as usize];
        outline.draw(|gx, gy, v| {
            let i = (gy as i32 * w + gx as i32) as usize;
            if i < coverage.len() {
                coverage[i] = (v * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        });
        // Synthetic bold for a family without a bold face is not needed:
        // fontconfig returns the regular face, which stays readable.
        Some(Mask {
            w,
            h,
            x: b.min.x.floor() as i32,
            y: b.min.y.floor() as i32,
            coverage,
        })
    }
}

/// Box drawing (U+2500..257F), blocks (U+2580..259F) and powerline arrows
/// (U+E0B0..E0B3) as exact pixels. Returns false for anything else.
pub fn draw_special(cv: &mut Canvas, x: i32, y: i32, w: i32, h: i32, c: char, col: u32) -> bool {
    let code = c as u32;
    let light = (w / 9).max(1);
    let heavy = (light * 2).max(2);
    let cx = x + (w - light) / 2;
    let cy = y + (h - light) / 2;
    // Lines from the centre: (left, right, up, down) with 0 none, 1 light, 2 heavy, 3 double.
    let lines: Option<[u8; 4]> = match code {
        0x2500 => Some([1, 1, 0, 0]),
        0x2501 => Some([2, 2, 0, 0]),
        0x2502 => Some([0, 0, 1, 1]),
        0x2503 => Some([0, 0, 2, 2]),
        0x250c => Some([0, 1, 0, 1]),
        0x250f => Some([0, 2, 0, 2]),
        0x2510 => Some([1, 0, 0, 1]),
        0x2513 => Some([2, 0, 0, 2]),
        0x2514 => Some([0, 1, 1, 0]),
        0x2517 => Some([0, 2, 2, 0]),
        0x2518 => Some([1, 0, 1, 0]),
        0x251b => Some([2, 0, 2, 0]),
        0x251c => Some([0, 1, 1, 1]),
        0x2523 => Some([0, 2, 2, 2]),
        0x2524 => Some([1, 0, 1, 1]),
        0x252b => Some([2, 0, 2, 2]),
        0x252c => Some([1, 1, 0, 1]),
        0x2533 => Some([2, 2, 0, 2]),
        0x2534 => Some([1, 1, 1, 0]),
        0x253b => Some([2, 2, 2, 0]),
        0x253c => Some([1, 1, 1, 1]),
        0x254b => Some([2, 2, 2, 2]),
        0x2550 => Some([3, 3, 0, 0]),
        0x2551 => Some([0, 0, 3, 3]),
        0x2554 => Some([0, 3, 0, 3]),
        0x2557 => Some([3, 0, 0, 3]),
        0x255a => Some([0, 3, 3, 0]),
        0x255d => Some([3, 0, 3, 0]),
        0x2560 => Some([0, 3, 3, 3]),
        0x2563 => Some([3, 0, 3, 3]),
        0x2566 => Some([3, 3, 0, 3]),
        0x2569 => Some([3, 3, 3, 0]),
        0x256c => Some([3, 3, 3, 3]),
        // Rounded corners are drawn square; they read the same at text size.
        0x256d => Some([0, 1, 0, 1]),
        0x256e => Some([1, 0, 0, 1]),
        0x256f => Some([1, 0, 1, 0]),
        0x2570 => Some([0, 1, 1, 0]),
        0x2574 => Some([1, 0, 0, 0]),
        0x2575 => Some([0, 0, 1, 0]),
        0x2576 => Some([0, 1, 0, 0]),
        0x2577 => Some([0, 0, 0, 1]),
        _ => None,
    };
    if let Some([l, r, u, d]) = lines {
        let thick = |k: u8| if k == 2 { heavy } else { light };
        let hy = |k: u8| y + (h - thick(k)) / 2;
        let vx = |k: u8| x + (w - thick(k)) / 2;
        let gap = light * 2;
        // Horizontal halves.
        for (k, x0, x1) in [(l, x, cx + light), (r, cx, x + w)] {
            match k {
                0 => {}
                3 => {
                    cv.fill(Rect::new(x0, cy - gap / 2 - light / 2, x1 - x0, light), col);
                    cv.fill(Rect::new(x0, cy + gap / 2 + light / 2, x1 - x0, light), col);
                }
                _ => cv.fill(Rect::new(x0, hy(k), x1 - x0, thick(k)), col),
            }
        }
        for (k, y0, y1) in [(u, y, cy + light), (d, cy, y + h)] {
            match k {
                0 => {}
                3 => {
                    cv.fill(Rect::new(cx - gap / 2 - light / 2, y0, light, y1 - y0), col);
                    cv.fill(Rect::new(cx + gap / 2 + light / 2, y0, light, y1 - y0), col);
                }
                _ => cv.fill(Rect::new(vx(k), y0, thick(k), y1 - y0), col),
            }
        }
        return true;
    }
    match code {
        // Upper half, lower eighths, full, left eighths, right half, shades.
        0x2580 => cv.fill(Rect::new(x, y, w, h / 2), col),
        0x2581..=0x2588 => {
            let n = (code - 0x2580) as i32;
            let hh = h * n / 8;
            cv.fill(Rect::new(x, y + h - hh, w, hh), col);
        }
        0x2589..=0x258f => {
            let n = (0x2590 - code) as i32;
            cv.fill(Rect::new(x, y, w * n / 8, h), col);
        }
        0x2590 => cv.fill(Rect::new(x + w / 2, y, w - w / 2, h), col),
        0x2591..=0x2593 => {
            let a = [64u32, 128, 192][(code - 0x2591) as usize];
            cv.tint(Rect::new(x, y, w, h), col, a);
        }
        0x2594 => cv.fill(Rect::new(x, y, w, (h / 8).max(1)), col),
        0x2595 => cv.fill(Rect::new(x + w - (w / 8).max(1), y, (w / 8).max(1), h), col),
        0x2596..=0x259f => {
            // Quadrants: bits for upper-left, upper-right, lower-left, lower-right.
            let q = [
                0b0010, 0b0001, 0b1000, 0b1011, 0b1001, 0b1110, 0b1101, 0b0100, 0b0110, 0b0111,
            ][(code - 0x2596) as usize];
            let (hw, hh) = (w / 2, h / 2);
            if q & 0b1000 != 0 {
                cv.fill(Rect::new(x, y, hw, hh), col);
            }
            if q & 0b0100 != 0 {
                cv.fill(Rect::new(x + hw, y, w - hw, hh), col);
            }
            if q & 0b0010 != 0 {
                cv.fill(Rect::new(x, y + hh, hw, h - hh), col);
            }
            if q & 0b0001 != 0 {
                cv.fill(Rect::new(x + hw, y + hh, w - hw, h - hh), col);
            }
        }
        0xe0b0 | 0xe0b2 => {
            // Solid powerline arrows, right and left.
            for row in 0..h {
                let half = h as f32 / 2.0;
                let d = (row as f32 + 0.5 - half).abs();
                let len = ((1.0 - d / half) * w as f32).round() as i32;
                if code == 0xe0b0 {
                    cv.fill(Rect::new(x, y + row, len, 1), col);
                } else {
                    cv.fill(Rect::new(x + w - len, y + row, len, 1), col);
                }
            }
        }
        0xe0b1 | 0xe0b3 => {
            for row in 0..h {
                let half = h as f32 / 2.0;
                let d = (row as f32 + 0.5 - half).abs();
                let pos = ((1.0 - d / half) * (w - light) as f32).round() as i32;
                let px = if code == 0xe0b1 {
                    x + pos
                } else {
                    x + w - light - pos
                };
                cv.fill(Rect::new(px, y + row, light, 1), col);
            }
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_lines_reach_the_cell_edges_so_they_join() {
        let mut cv = Canvas::new(10, 20);
        assert!(draw_special(&mut cv, 0, 0, 10, 20, '─', 0xffffff));
        let row = (20 - 1) / 2;
        assert_eq!(cv.px[(row * 10) as usize], 0xffffff);
        assert_eq!(cv.px[(row * 10 + 9) as usize], 0xffffff);
        let mut cv = Canvas::new(10, 20);
        assert!(draw_special(&mut cv, 0, 0, 10, 20, '│', 0xffffff));
        assert_eq!(cv.px[4].max(cv.px[5]), 0xffffff);
        assert!(!draw_special(&mut cv, 0, 0, 10, 20, 'a', 0xffffff));
    }

    #[test]
    fn full_block_fills_the_cell() {
        let mut cv = Canvas::new(8, 16);
        draw_special(&mut cv, 0, 0, 8, 16, '█', 0x123456);
        assert!(cv.px.iter().all(|p| *p == 0x123456));
    }
}
