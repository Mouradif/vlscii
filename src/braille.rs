//! BrailleMapper — 2×4 sub-pixel rendering via Unicode braille (U+2800..28FF).
//!
//! Each terminal cell becomes a 2×4 grid of dots (8× the spatial resolution of
//! the glyph-ramp mode for the same cell count). A dot is "on" when its source
//! sub-pixel's luma clears a threshold; the cell is colored by the average color
//! of its lit dots (or all dots, if none are lit), reusing the same ANSI
//! truecolor + per-row RLE machinery as the other mappers.
//!
//! Braille dot -> bit layout (the historical, non-obvious ordering):
//!
//!     col0 col1
//!   ┌──────────
//!   │  d0   d3      bits: 0x01  0x08
//!   │  d1   d4            0x02  0x10
//!   │  d2   d5            0x04  0x20
//!   │  d6   d7            0x40  0x80
//!
//! So sub-pixel at (dx, dy) within the 2×4 block maps to:
//!   dy < 3 : bit = 1 << (dy + 3*dx)
//!   dy == 3: bit = 0x40 << dx

use crate::mapper::{RESET, luma, push_color_escape};

const BRAILLE_BASE: u32 = 0x2800;
/// Width/height of the sub-pixel block ffmpeg must produce per terminal cell.
pub const BRAILLE_SUB_W: usize = 2;
pub const BRAILLE_SUB_H: usize = 4;

pub struct BrailleMapper {
    /// Luma threshold (0..=255); sub-pixels at/above this turn their dot on.
    threshold: u8,
    /// When true, use Floyd–Steinberg error diffusion over the whole frame's
    /// luma plane to decide dots, instead of a hard per-dot threshold. This
    /// turns smooth gradients into stippled patterns instead of flat regions.
    dither: bool,
    quantize_bits: u32,
}

impl BrailleMapper {
    pub fn new(threshold: u8, dither: bool, quantize_bits: u32) -> BrailleMapper {
        BrailleMapper {
            threshold,
            dither,
            quantize_bits,
        }
    }

    /// Convert one `rgb24` frame sized at the *sub-pixel* resolution
    /// (`cell_w*2` wide, `cell_h*4` tall) into a braille string of
    /// `cell_w × cell_h` characters.
    pub fn convert(&self, rgb: &[u8], cell_w: usize, cell_h: usize) -> String {
        let sub_w = cell_w * BRAILLE_SUB_W;
        let sub_h = cell_h * BRAILLE_SUB_H;
        debug_assert_eq!(rgb.len(), sub_w * sub_h * 3);

        let qb = self.quantize_bits;
        let mask: u8 = if qb > 0 { (0xFFu8) << qb } else { 0xFF };

        // Decide every dot up front. With dithering this is a frame-wide pass
        // (error diffusion crosses cell boundaries); otherwise it's a plain
        // per-pixel threshold. `on[y*sub_w + x]` = dot lit at that sub-pixel.
        let on = if self.dither {
            dither_plane(rgb, sub_w, sub_h, self.threshold)
        } else {
            threshold_plane(rgb, sub_w, sub_h, self.threshold)
        };

        // Braille chars are 3 bytes in UTF-8; budget for chars + escapes + nl.
        let mut out = String::with_capacity(cell_w * cell_h * 3 + cell_w * cell_h + 16);
        out.push_str(RESET);

        let mut prev_r: i32 = -1;
        let mut prev_g: i32 = -1;
        let mut prev_b: i32 = -1;
        let mut esc = [0u8; 3];

        // Index a sub-pixel: (x,y) -> byte offset into rgb.
        let px = |x: usize, y: usize| (y * sub_w + x) * 3;

        for cy in 0..cell_h {
            if cy > 0 {
                out.push('\n');
            }
            for cx in 0..cell_w {
                let ox = cx * BRAILLE_SUB_W; // origin of this cell's block
                let oy = cy * BRAILLE_SUB_H;

                let mut bits: u32 = 0;
                // Accumulate color of lit dots, and of all dots as a fallback.
                let (mut lr, mut lg, mut lb, mut lit) = (0u32, 0u32, 0u32, 0u32);
                let (mut ar, mut ag, mut ab) = (0u32, 0u32, 0u32);

                for dx in 0..BRAILLE_SUB_W {
                    for dy in 0..BRAILLE_SUB_H {
                        let sx = ox + dx;
                        let sy = oy + dy;
                        let off = px(sx, sy);
                        let r = rgb[off];
                        let g = rgb[off + 1];
                        let b = rgb[off + 2];
                        ar += r as u32;
                        ag += g as u32;
                        ab += b as u32;
                        if on[sy * sub_w + sx] {
                            bits |= dot_bit(dx, dy);
                            lr += r as u32;
                            lg += g as u32;
                            lb += b as u32;
                            lit += 1;
                        }
                    }
                }

                // Color = average of lit dots; if none lit, average of all 8.
                let (mut r, mut g, mut b) = if lit > 0 {
                    ((lr / lit) as u8, (lg / lit) as u8, (lb / lit) as u8)
                } else {
                    let n = (BRAILLE_SUB_W * BRAILLE_SUB_H) as u32;
                    ((ar / n) as u8, (ag / n) as u8, (ab / n) as u8)
                };
                if qb > 0 {
                    r &= mask;
                    g &= mask;
                    b &= mask;
                }

                let (ri, gi, bi) = (r as i32, g as i32, b as i32);
                if ri != prev_r || gi != prev_g || bi != prev_b {
                    push_color_escape(&mut out, r, g, b, &mut esc);
                    prev_r = ri;
                    prev_g = gi;
                    prev_b = bi;
                }

                // SAFETY-free: BRAILLE_BASE + bits (bits ≤ 0xFF) is always a
                // valid braille scalar in U+2800..=U+28FF.
                out.push(char::from_u32(BRAILLE_BASE + bits).unwrap());
            }
        }

        out.push_str(RESET);
        out
    }
}

/// Map a sub-pixel position within a 2×4 block to its braille dot bit.
#[inline]
fn dot_bit(dx: usize, dy: usize) -> u32 {
    if dy < 3 {
        1u32 << (dy + 3 * dx)
    } else {
        0x40u32 << dx
    }
}

/// Hard-threshold every sub-pixel: dot lit iff its luma ≥ `threshold`.
fn threshold_plane(rgb: &[u8], w: usize, h: usize, threshold: u8) -> Vec<bool> {
    let mut on = vec![false; w * h];
    for (i, slot) in on.iter_mut().enumerate() {
        let off = i * 3;
        *slot = luma(rgb[off], rgb[off + 1], rgb[off + 2]) >= threshold;
    }
    on
}

/// Floyd–Steinberg dither the luma plane into a 1-bit on/off mask.
///
/// Each pixel is rounded to black or white; the quantization error is pushed to
/// neighbours with the classic 7/16, 3/16, 5/16, 1/16 weights. `threshold` sets
/// where the black/white split lands (default 128 ≈ midpoint; lower = brighter
/// overall). Running over the whole frame (not per cell) is what makes gradients
/// resolve into smooth stipple instead of blocky bands.
fn dither_plane(rgb: &[u8], w: usize, h: usize, threshold: u8) -> Vec<bool> {
    // Work in i32 so diffused error can push values past 0..255.
    let mut buf: Vec<i32> = Vec::with_capacity(w * h);
    for i in 0..w * h {
        let off = i * 3;
        buf.push(luma(rgb[off], rgb[off + 1], rgb[off + 2]) as i32);
    }

    let mut on = vec![false; w * h];
    let t = threshold as i32;
    for y in 0..h {
        for x in 0..w {
            let idx = y * w + x;
            let old = buf[idx];
            let lit = old >= t;
            on[idx] = lit;
            // Quantize to the extreme we picked, error = what we discarded.
            let new = if lit { 255 } else { 0 };
            let err = old - new;

            // Distribute error to not-yet-visited neighbours.
            if x + 1 < w {
                buf[idx + 1] += err * 7 / 16;
            }
            if y + 1 < h {
                if x > 0 {
                    buf[idx + w - 1] += err * 3 / 16;
                }
                buf[idx + w] += err * 5 / 16;
                if x + 1 < w {
                    buf[idx + w + 1] += err / 16;
                }
            }
        }
    }
    on
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 2×4 (sub-pixel) rgb24 block for one braille cell from a closure
    /// returning each sub-pixel's (r,g,b).
    fn cell_block(f: impl Fn(usize, usize) -> (u8, u8, u8)) -> Vec<u8> {
        let mut v = Vec::with_capacity(BRAILLE_SUB_W * BRAILLE_SUB_H * 3);
        for y in 0..BRAILLE_SUB_H {
            for x in 0..BRAILLE_SUB_W {
                let (r, g, b) = f(x, y);
                v.extend_from_slice(&[r, g, b]);
            }
        }
        v
    }

    #[test]
    fn all_dark_is_blank_braille() {
        let m = BrailleMapper::new(128, false, 0);
        let blk = cell_block(|_, _| (0, 0, 0));
        let s = m.convert(&blk, 1, 1);
        // U+2800 is the blank braille cell.
        assert!(s.contains('\u{2800}'));
    }

    #[test]
    fn all_bright_lights_every_dot() {
        let m = BrailleMapper::new(128, false, 0);
        let blk = cell_block(|_, _| (255, 255, 255));
        let s = m.convert(&blk, 1, 1);
        // All 8 dots set -> 0x2800 + 0xFF = U+28FF.
        assert!(s.contains('\u{28ff}'));
        assert!(s.contains("\x1b[38;2;255;255;255m"));
    }

    #[test]
    fn single_top_left_dot() {
        let m = BrailleMapper::new(128, false, 0);
        // Only sub-pixel (0,0) bright -> dot d0 -> bit 0x01 -> U+2801.
        let blk = cell_block(|x, y| if x == 0 && y == 0 { (255, 255, 255) } else { (0, 0, 0) });
        let s = m.convert(&blk, 1, 1);
        assert!(s.contains('\u{2801}'));
    }

    #[test]
    fn bottom_right_dot_uses_high_bit() {
        let m = BrailleMapper::new(128, false, 0);
        // Sub-pixel (1,3) -> bottom-right dot d7 -> bit 0x80 -> U+2880.
        let blk = cell_block(|x, y| if x == 1 && y == 3 { (255, 255, 255) } else { (0, 0, 0) });
        let s = m.convert(&blk, 1, 1);
        assert!(s.contains('\u{2880}'));
    }

    #[test]
    fn color_averages_lit_dots_only() {
        // Threshold below red's luma (200*0.299 ≈ 59) so the dot lights.
        let m = BrailleMapper::new(40, false, 0);
        // One lit red dot, rest black -> cell color is that red, not a dimmed
        // average that black would drag down.
        let blk = cell_block(|x, y| if x == 0 && y == 0 { (200, 0, 0) } else { (0, 0, 0) });
        let s = m.convert(&blk, 1, 1);
        assert!(s.contains("\x1b[38;2;200;0;0m"));
    }

    #[test]
    fn dither_extremes_are_stable() {
        // Pure black/white must not stipple: every dot off / on respectively.
        let m = BrailleMapper::new(128, true, 0);
        let black = cell_block(|_, _| (0, 0, 0));
        let white = cell_block(|_, _| (255, 255, 255));
        assert!(m.convert(&black, 1, 1).contains('\u{2800}'));
        assert!(m.convert(&white, 1, 1).contains('\u{28ff}'));
    }

    #[test]
    fn dither_midtone_is_partially_lit() {
        // A flat mid-gray field should diffuse into a mix of lit/unlit dots
        // rather than collapsing to all-on or all-off.
        let m = BrailleMapper::new(128, true, 0);
        // 4×4 cells of mid-gray (128) -> 8×16 sub-pixel plane.
        let w = 4 * BRAILLE_SUB_W;
        let h = 4 * BRAILLE_SUB_H;
        let mut frame = Vec::with_capacity(w * h * 3);
        for _ in 0..w * h {
            frame.extend_from_slice(&[128, 128, 128]);
        }
        let s = m.convert(&frame, 4, 4);
        let blank = s.matches('\u{2800}').count();
        let full = s.matches('\u{28ff}').count();
        // Not entirely blank and not entirely full -> dithering did something.
        assert!(blank < 16 && full < 16, "blank={blank} full={full}");
    }
}
