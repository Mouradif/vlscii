//! AsciiMapper — converts an rgb24 frame into a colored ASCII string.
//!
//!   1. Per-pixel gray intensity -> ASCII glyph via an intensity LUT.
//!   2. Per-pixel RGB -> ANSI 24-bit truecolor escape, with optional bit-depth
//!      quantization and per-row run-length encoding (escape emitted only when
//!      the color changes).
//!
//! Gray is derived from RGB with the standard Rec.601 luma weights
//! (0.299 R + 0.587 G + 0.114 B).

use crate::mapper::{RESET, luma, push_color_escape};

/// Default 93-level intensity ramp (dark -> bright).
const DEFAULT_PALETTE: &str =
    " `.-':_,^=;><+!rc*/z?sLTv)J7(|Fi{C}fI31tlu[neoZ5Yxjya]2ESwqkP6h9d4VpOGbUAKXHm8RD#$Bg0MNWQ%&@";

pub struct AsciiMapper {
    lut: Vec<char>,
    /// 256-entry table mapping a gray byte directly to a palette glyph,
    /// precomputed from the `gray / (256 / n)` + clamp bucketing.
    gray_to_glyph: [char; 256],
    quantize_bits: u32,
}

impl AsciiMapper {
    pub fn new(palette: Option<Vec<char>>, quantize_bits: u32) -> AsciiMapper {
        let lut: Vec<char> = match palette {
            Some(p) if !p.is_empty() => p,
            _ => DEFAULT_PALETTE.chars().collect(),
        };
        let n = lut.len();
        // index = clamp(gray / max(1, 256 / n), 0, n-1)
        let divisor = (256 / n).max(1);
        let mut gray_to_glyph = [' '; 256];
        for (g, slot) in gray_to_glyph.iter_mut().enumerate() {
            let idx = (g / divisor).min(n - 1);
            *slot = lut[idx];
        }

        AsciiMapper {
            lut,
            gray_to_glyph,
            quantize_bits,
        }
    }

    /// Convert one `rgb24` frame (`width*height*3` bytes) into a colored ASCII
    /// string with rows joined by `\n` and wrapped in RESET on both ends.
    pub fn convert(&self, rgb: &[u8], width: usize, height: usize) -> String {
        debug_assert_eq!(rgb.len(), width * height * 3);

        let qb = self.quantize_bits;
        // Mask for clearing the low `qb` bits (qb=0 -> no-op mask 0xFF).
        let mask: u8 = if qb > 0 { (0xFFu8) << qb } else { 0xFF };

        // Pre-size generously: glyphs + occasional ~19-byte escapes + newlines.
        let mut out = String::with_capacity(width * height * 2 + height + 16);
        out.push_str(RESET);

        // -1 sentinel: the first pixel always emits a color code.
        let mut prev_r: i32 = -1;
        let mut prev_g: i32 = -1;
        let mut prev_b: i32 = -1;

        let mut esc = [0u8; 3]; // scratch for itoa-style writes

        for y in 0..height {
            if y > 0 {
                out.push('\n');
            }
            let row = &rgb[y * width * 3..(y + 1) * width * 3];
            for x in 0..width {
                let base = x * 3;
                let mut r = row[base];
                let mut g = row[base + 1];
                let mut b = row[base + 2];
                if qb > 0 {
                    r &= mask;
                    g &= mask;
                    b &= mask;
                }

                // gray = luma(r,g,b) -> glyph
                let glyph = self.gray_to_glyph[luma(r, g, b) as usize];

                let (ri, gi, bi) = (r as i32, g as i32, b as i32);
                if ri != prev_r || gi != prev_g || bi != prev_b {
                    push_color_escape(&mut out, r, g, b, &mut esc);
                    prev_r = ri;
                    prev_g = gi;
                    prev_b = bi;
                }
                out.push(glyph);
            }
        }

        out.push_str(RESET);
        out
    }

    /// Number of glyphs in the active palette (for the info screen).
    pub fn levels(&self) -> usize {
        self.lut.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_pixel_is_first_palette_glyph() {
        let m = AsciiMapper::new(None, 0);
        // A single black pixel -> darkest glyph (space) + leading escape.
        let s = m.convert(&[0, 0, 0], 1, 1);
        assert!(s.contains("\x1b[38;2;0;0;0m"));
        assert!(s.starts_with(RESET));
        assert!(s.ends_with(RESET));
    }

    #[test]
    fn white_pixel_is_brightest_glyph() {
        let m = AsciiMapper::new(None, 0);
        let s = m.convert(&[255, 255, 255], 1, 1);
        assert!(s.contains('@')); // last glyph in default ramp
        assert!(s.contains("\x1b[38;2;255;255;255m"));
    }

    #[test]
    fn rle_reuses_color_across_run() {
        let m = AsciiMapper::new(None, 0);
        // Two identical pixels in a row -> only one color escape.
        let s = m.convert(&[10, 20, 30, 10, 20, 30], 2, 1);
        let occurrences = s.matches("\x1b[38;2;10;20;30m").count();
        assert_eq!(occurrences, 1);
    }

    #[test]
    fn quantize_masks_low_bits() {
        let m = AsciiMapper::new(None, 2);
        // 0b1111_1111 -> 0b1111_1100 = 252 under qb=2.
        let s = m.convert(&[255, 255, 255], 1, 1);
        assert!(s.contains("\x1b[38;2;252;252;252m"));
    }
}
