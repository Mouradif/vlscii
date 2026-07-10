//! Mapper — dispatch over the available render modes, plus the shared ANSI
//! truecolor helpers every mapper uses.
//!
//! Each render mode lives in its own module (`ascii`, `braille`, `half_block`)
//! and turns one `rgb24` frame into a colored terminal string. This module owns
//! the enum that dispatches between them and the small primitives they share:
//! escape-sequence emission, Rec.601 luma, and cheap `u8` formatting.

use crate::ascii::AsciiMapper;
use crate::braille::{BRAILLE_SUB_H, BRAILLE_SUB_W, BrailleMapper};
use crate::half_block::{HALF_SUB_H, HALF_SUB_W, HalfBlockMapper};

/// SGR reset — clears fg/bg color and attributes.
pub(crate) const RESET: &str = "\x1b[0m";

/// Active render mode, selected on the CLI.
pub enum Mapper {
    Ascii(Box<AsciiMapper>),
    Braille(BrailleMapper),
    HalfBlock(HalfBlockMapper),
}

impl Mapper {
    /// Sub-pixel multipliers (x, y) the decoder must scale to, relative to the
    /// terminal cell grid. ASCII is 1×1; braille is 2×4; half-block is 1×2.
    pub fn subpixel_scale(&self) -> (usize, usize) {
        match self {
            Mapper::Ascii(_) => (1, 1),
            Mapper::Braille(_) => (BRAILLE_SUB_W, BRAILLE_SUB_H),
            Mapper::HalfBlock(_) => (HALF_SUB_W, HALF_SUB_H),
        }
    }

    /// Render one frame. `cell_w`/`cell_h` are the terminal cell dimensions;
    /// `rgb` is sized at `cell * subpixel_scale`.
    pub fn convert(&self, rgb: &[u8], cell_w: usize, cell_h: usize) -> String {
        match self {
            Mapper::Ascii(m) => m.convert(rgb, cell_w, cell_h),
            Mapper::Braille(m) => m.convert(rgb, cell_w, cell_h),
            Mapper::HalfBlock(m) => m.convert(rgb, cell_w, cell_h),
        }
    }
}

/// Append `\x1b[38;2;R;G;Bm` without allocating an intermediate String.
#[inline]
pub(crate) fn push_color_escape(out: &mut String, r: u8, g: u8, b: u8, scratch: &mut [u8; 3]) {
    out.push_str("\x1b[38;2;");
    push_u8(out, r, scratch);
    out.push(';');
    push_u8(out, g, scratch);
    out.push(';');
    push_u8(out, b, scratch);
    out.push('m');
}

/// Append `\x1b[38;2;Rf;Gf;Bf;48;2;Rb;Gb;Bbm` — foreground + background in one
/// escape, for half-block rendering (top color = fg, bottom color = bg).
#[inline]
#[allow(clippy::too_many_arguments)]
pub(crate) fn push_fg_bg_escape(
    out: &mut String,
    fr: u8,
    fg: u8,
    fb: u8,
    br: u8,
    bg: u8,
    bb: u8,
    scratch: &mut [u8; 3],
) {
    out.push_str("\x1b[38;2;");
    push_u8(out, fr, scratch);
    out.push(';');
    push_u8(out, fg, scratch);
    out.push(';');
    push_u8(out, fb, scratch);
    out.push_str(";48;2;");
    push_u8(out, br, scratch);
    out.push(';');
    push_u8(out, bg, scratch);
    out.push(';');
    push_u8(out, bb, scratch);
    out.push('m');
}

/// Perceptual luma (0..=255), Rec.601 weights.
#[inline]
pub(crate) fn luma(r: u8, g: u8, b: u8) -> u8 {
    ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000) as u8
}

/// Append a u8 in decimal (0..=255) cheaply.
#[inline]
pub(crate) fn push_u8(out: &mut String, v: u8, scratch: &mut [u8; 3]) {
    if v >= 100 {
        scratch[0] = b'0' + v / 100;
        scratch[1] = b'0' + (v / 10) % 10;
        scratch[2] = b'0' + v % 10;
        out.push(scratch[0] as char);
        out.push(scratch[1] as char);
        out.push(scratch[2] as char);
    } else if v >= 10 {
        out.push((b'0' + v / 10) as char);
        out.push((b'0' + v % 10) as char);
    } else {
        out.push((b'0' + v) as char);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subpixel_scale_matches_mode() {
        assert_eq!(
            Mapper::Ascii(AsciiMapper::new(None, 0)).subpixel_scale(),
            (1, 1)
        );
        assert_eq!(
            Mapper::Braille(BrailleMapper::new(128, false, 0)).subpixel_scale(),
            (2, 4)
        );
        assert_eq!(
            Mapper::HalfBlock(HalfBlockMapper::new(0)).subpixel_scale(),
            (1, 2)
        );
    }
}
