//! HalfBlockMapper — 1×2 sub-pixel rendering via the upper-half block (▀).
//!
//! Each cell stacks two source pixels vertically and renders U+2580 (▀):
//!   - the glyph's solid top half takes the FOREGROUND color  = top pixel
//!   - the empty bottom half shows the BACKGROUND color       = bottom pixel
//!
//! This doubles vertical resolution while keeping TWO full truecolor values per
//! cell — the color-rich counterpart to braille's detail-rich/one-color trade.

use crate::mapper::{RESET, push_fg_bg_escape};

const UPPER_HALF_BLOCK: char = '\u{2580}'; // ▀
/// Sub-pixel block size per cell: 1 wide, 2 tall.
pub const HALF_SUB_W: usize = 1;
pub const HALF_SUB_H: usize = 2;

pub struct HalfBlockMapper {
    quantize_bits: u32,
}

impl HalfBlockMapper {
    pub fn new(quantize_bits: u32) -> HalfBlockMapper {
        HalfBlockMapper { quantize_bits }
    }

    /// Convert one `rgb24` frame sized at `cell_w × (cell_h*2)` into a half-block
    /// string of `cell_w × cell_h` characters.
    pub fn convert(&self, rgb: &[u8], cell_w: usize, cell_h: usize) -> String {
        let sub_w = cell_w * HALF_SUB_W;
        let sub_h = cell_h * HALF_SUB_H;
        debug_assert_eq!(rgb.len(), sub_w * sub_h * 3);

        let qb = self.quantize_bits;
        let mask: u8 = if qb > 0 { (0xFFu8) << qb } else { 0xFF };
        let q = |v: u8| if qb > 0 { v & mask } else { v };

        let mut out = String::with_capacity(cell_w * cell_h * 3 + cell_w * cell_h + 16);

        let mut esc = [0u8; 3];
        let px = |x: usize, y: usize| (y * sub_w + x) * 3;

        for cy in 0..cell_h {
            if cy > 0 {
                out.push('\n');
            }
            // Reset at the START of every row, then re-emit colors. This keeps
            // the active background from leaking past the row's end — otherwise
            // the surrounding centering padding (plain spaces) would render in
            // the last cell's background color. RLE restarts each row, so
            // `prev` begins as None.
            out.push_str(RESET);
            let mut prev: Option<(u8, u8, u8, u8, u8, u8)> = None;

            let top_y = cy * HALF_SUB_H;
            let bot_y = top_y + 1;
            for cx in 0..cell_w {
                let t = px(cx, top_y);
                let b = px(cx, bot_y);
                let fr = q(rgb[t]);
                let fg = q(rgb[t + 1]);
                let fb = q(rgb[t + 2]);
                let br = q(rgb[b]);
                let bg = q(rgb[b + 1]);
                let bb = q(rgb[b + 2]);

                let cur = (fr, fg, fb, br, bg, bb);
                if prev != Some(cur) {
                    push_fg_bg_escape(&mut out, fr, fg, fb, br, bg, bb, &mut esc);
                    prev = Some(cur);
                }
                out.push(UPPER_HALF_BLOCK);
            }
            // Clear the background again so the right-side padding is clean too.
            out.push_str(RESET);
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_block_uses_fg_and_bg() {
        let m = HalfBlockMapper::new(0);
        // 1 cell = top pixel (red) over bottom pixel (blue).
        let frame = vec![200, 0, 0, /* top */ 0, 0, 200 /* bottom */];
        let s = m.convert(&frame, 1, 1);
        assert!(s.contains('\u{2580}')); // ▀
        // fg = top (red), bg = bottom (blue).
        assert!(s.contains("\x1b[38;2;200;0;0;48;2;0;0;200m"));
    }

    #[test]
    fn half_block_rle_reuses_pair() {
        let m = HalfBlockMapper::new(0);
        // Two identical cells side by side -> one combined escape.
        let frame = vec![
            10, 20, 30, // cell0 top
            10, 20, 30, // cell1 top
            40, 50, 60, // cell0 bottom
            40, 50, 60, // cell1 bottom
        ];
        let s = m.convert(&frame, 2, 1);
        let count = s.matches("\x1b[38;2;10;20;30;48;2;40;50;60m").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn half_block_resets_background_each_row() {
        // Regression: the active background must be cleared before every
        // newline, or centering padding renders in the last cell's bg color.
        let m = HalfBlockMapper::new(0);
        // 1×2 grid: two rows, distinct colors.
        let frame = vec![
            10, 0, 0, // row0 top
            20, 0, 0, // row0 bottom
            30, 0, 0, // row1 top
            40, 0, 0, // row1 bottom
        ];
        let s = m.convert(&frame, 1, 2);
        // Each newline must be immediately preceded by a RESET.
        for (i, _) in s.match_indices('\n') {
            assert!(
                s[..i].ends_with(RESET),
                "row did not end with RESET before newline"
            );
        }
        // And the whole frame must end reset too (clean right/bottom padding).
        assert!(s.ends_with(RESET));
    }
}
