//! TerminalRenderer — orchestrates probe -> sizing -> decode -> render.
//!
//! The renderer probes the video, computes an aspect-correct grid that fits the
//! terminal (or honours a fixed `--cols`), prints an info screen, then runs the
//! zero-flicker playback loop with FPS pacing and centering padding.

use crate::ascii::AsciiMapper;
use crate::braille::BrailleMapper;
use crate::decoder::{FrameReader, VideoProbe};
use crate::half_block::HalfBlockMapper;
use crate::mapper::Mapper;
use crate::subtitles::Subtitles;
use crate::term::TermSession;
use std::cell::{Cell, RefCell};
use std::io::{self, Write};
use std::time::{Duration, Instant};

/// Which renderer to use for a session.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    Ascii,
    Braille,
    HalfBlock,
}

impl RenderMode {
    fn label(self) -> &'static str {
        match self {
            RenderMode::Ascii => "ASCII (glyph ramp)",
            RenderMode::Braille => "BRAILLE (2×4 dots)",
            RenderMode::HalfBlock => "HALF-BLOCK (1×2, 2 colors)",
        }
    }
}

/// Video orientation, derived from the source dimensions. Drives the
/// aspect-preserving sizing branch and the info splash.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Orientation {
    Portrait,
    Landscape,
}

impl Orientation {
    /// Classify by dimensions: taller than wide is portrait.
    fn of(width: u32, height: u32) -> Orientation {
        if height > width {
            Orientation::Portrait
        } else {
            Orientation::Landscape
        }
    }

    fn label(self) -> &'static str {
        match self {
            Orientation::Portrait => "PORTRAIT",
            Orientation::Landscape => "LANDSCAPE",
        }
    }
}

// ── ANSI control sequences ──────────────────────────────────────────────
// Session setup/teardown (alt screen, cursor, wrap, bg) lives in `term`.
const CURSOR_HOME: &str = "\x1b[H";
const CLEAR_SCREEN: &str = "\x1b[2J";

/// Terminal character aspect ratio correction (cells are taller than wide).
const CHAR_RATIO: f64 = 0.45;

pub struct TerminalRenderer {
    path: String,
    mapper: Mapper,
    /// Terminal-cell grid dimensions.
    cols: u32,
    rows: u32,
    /// Sub-pixel grid ffmpeg decodes to (cols*sx, rows*sy).
    sub_cols: u32,
    sub_rows: u32,
    fps: f64,
    pad_y: usize,
    pad_x: String,
    /// Usable terminal size, for absolute-positioning the subtitle band.
    term_cols: usize,
    term_lines: usize,
    /// Active subtitles, if any. `RefCell` because the playback loop runs on
    /// `&self` but cue lookup advances an internal cursor.
    subtitles: Option<RefCell<Subtitles>>,
    /// Number of subtitle rows drawn on the previous frame, so we can clear the
    /// band when a cue ends (fewer/zero lines) instead of leaving it on screen.
    prev_sub_rows: Cell<usize>,
    /// Keeps the alternate screen + signal recovery alive for the session;
    /// dropping it restores the terminal.
    _session: TermSession,
}

impl TerminalRenderer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        path: &str,
        palette: Option<Vec<char>>,
        quantize_bits: u32,
        cols_arg: u32,
        mode: RenderMode,
        braille_threshold: u8,
        dither: bool,
        subtitles: Option<Subtitles>,
        splash: u32,
    ) -> io::Result<TerminalRenderer> {
        // ── Video metadata ──────────────────────────────────────────────
        let probe = VideoProbe::open(path)?;
        let vid_w = probe.width;
        let vid_h = probe.height;
        let src_fps = probe.fps;

        // ── Terminal dimensions ─────────────────────────────────────────
        let (t_cols, t_lines_raw) = terminal_size().unwrap_or((220, 50));
        let t_cols = t_cols as i64;
        let t_lines = t_lines_raw as i64 - 2;

        // ── Orientation & aspect-preserving sizing ──────────────────────
        let orientation = Orientation::of(vid_w, vid_h);
        let aspect = vid_h as f64 / vid_w as f64;

        let (cols, rows): (i64, i64) = if cols_arg > 0 {
            let cols = cols_arg as i64;
            let rows = ((cols as f64 * aspect * CHAR_RATIO) as i64).max(1);
            (cols, rows)
        } else {
            // Windows terminals often struggle above 160 cols.
            let safe_cols = t_cols.min(160);
            if orientation == Orientation::Landscape {
                let mut cols = safe_cols;
                let mut rows = ((cols as f64 * aspect * CHAR_RATIO) as i64).max(1);
                if rows > t_lines {
                    rows = t_lines;
                    cols = ((rows as f64 / (aspect * CHAR_RATIO)) as i64).max(1);
                }
                (cols, rows)
            } else {
                let mut rows = t_lines;
                let mut cols = ((rows as f64 / (aspect * CHAR_RATIO)) as i64).max(1);
                if cols > safe_cols {
                    cols = safe_cols;
                    rows = ((cols as f64 * aspect * CHAR_RATIO) as i64).max(1);
                }
                (cols, rows)
            }
        };

        let cols = cols.max(1) as u32;
        let rows = rows.max(1) as u32;

        // ── Center padding ──────────────────────────────────────────────
        let pad_y = ((t_lines - rows as i64) / 2).max(0) as usize;
        let pad_x = " ".repeat(((t_cols - cols as i64) / 2).max(0) as usize);

        let mapper = match mode {
            RenderMode::Ascii => Mapper::Ascii(Box::new(AsciiMapper::new(palette, quantize_bits))),
            RenderMode::Braille => {
                Mapper::Braille(BrailleMapper::new(braille_threshold, dither, quantize_bits))
            }
            RenderMode::HalfBlock => Mapper::HalfBlock(HalfBlockMapper::new(quantize_bits)),
        };
        let (sx, sy) = mapper.subpixel_scale();
        let sub_cols = cols * sx as u32;
        let sub_rows = rows * sy as u32;

        // ── Info screen ─────────────────────────────────────────────────
        let levels = 2u32.pow(8 - quantize_bits);
        let detail = match &mapper {
            Mapper::Ascii(m) => format!("{} glyph levels", m.levels()),
            Mapper::Braille(_) => {
                let dith = if dither { ", dithered" } else { "" };
                format!("{sub_cols}x{sub_rows} dots, threshold {braille_threshold}{dith}")
            }
            Mapper::HalfBlock(_) => {
                format!("{sub_cols}x{sub_rows} sub-pixels, 2 colors/cell")
            }
        };
        // Enter the alternate screen now (after probing, so probe errors land
        // on the normal screen). The info splash + playback live on this page,
        // and any exit path — clean, error, or Ctrl+C — restores the terminal.
        let session = TermSession::enter()?;

        // Optional info splash: only shown when `--splash <secs>` is > 0, then
        // cleared so no stale lines survive into the first video frame.
        if splash > 0 {
            let subs_line = match &subtitles {
                Some(s) => format!("{} cue(s)", s.len()),
                None => "none".to_string(),
            };
            print!("{CLEAR_SCREEN}{CURSOR_HOME}");
            println!(
                "\x1b[1m[ASCII Player — True Color]\x1b[0m\n\
                 \u{20} Mode        : {}\n\
                 \u{20} Orientation : {}\n\
                 \u{20} Video       : {}x{}\n\
                 \u{20} Grid        : {}x{} cells\n\
                 \u{20} Detail      : {}\n\
                 \u{20} FPS         : {:.1}\n\
                 \u{20} Quantization: {} levels/channel\n\
                 \u{20} Subtitles   : {}\n\
                 \u{20} Exit        : Ctrl+C\n",
                mode.label(),
                orientation.label(),
                vid_w,
                vid_h,
                cols,
                rows,
                detail,
                src_fps,
                levels,
                subs_line,
            );
            io::stdout().flush().ok();
            std::thread::sleep(Duration::from_secs_f64(splash as f64));
            // Wipe the splash so the first frame starts on a clean page.
            print!("{CLEAR_SCREEN}{CURSOR_HOME}");
            io::stdout().flush().ok();
        }

        Ok(TerminalRenderer {
            path: path.to_string(),
            mapper,
            cols,
            rows,
            sub_cols,
            sub_rows,
            fps: src_fps,
            pad_y,
            pad_x,
            term_cols: t_cols.max(1) as usize,
            term_lines: t_lines.max(1) as usize,
            subtitles: subtitles.map(RefCell::new),
            prev_sub_rows: Cell::new(0),
            _session: session,
        })
    }

    /// Main playback loop.
    pub fn play(&self) -> io::Result<()> {
        // ffmpeg decodes at the sub-pixel resolution; the mapper folds it back
        // down to the cell grid (1×1 for ASCII, 2×4 for braille).
        let mut reader = FrameReader::open(&self.path, self.sub_cols, self.sub_rows)?;
        let frame_t = 1.0 / self.fps;

        let stdout = io::stdout();
        let mut out = io::BufWriter::new(stdout.lock());

        // Alt screen + cursor/wrap/bg were already set up by TermSession::enter
        // in `new`, and teardown is handled there too (incl. Ctrl+C).

        let w = self.cols as usize;
        let h = self.rows as usize;
        let top_pad = "\n".repeat(self.pad_y);

        // Subtitle band: the bottom padding region, below the video. We reserve
        // up to the last 2 lines of that band for (up to two) centered cue
        // lines, clearing each to the line end so stale text doesn't linger.
        let mut frame_idx: u64 = 0;

        loop {
            let t0 = Instant::now();

            let frame = match reader.next_frame()? {
                Some(f) => f,
                None => break,
            };

            let mut rendered_frame = self.mapper.convert(frame, w, h);

            // Apply centering padding.
            if !self.pad_x.is_empty() {
                rendered_frame = format!(
                    "{}{}",
                    self.pad_x,
                    rendered_frame.replace('\n', &format!("\n{}", self.pad_x))
                );
            }
            if self.pad_y > 0 {
                rendered_frame = format!("{top_pad}{rendered_frame}");
            }

            // Append subtitle overlay positioned by absolute cursor moves, so it
            // lands in the bottom band regardless of the frame's contents.
            if self.subtitles.is_some() {
                let t = frame_idx as f64 / self.fps;
                self.append_subtitles(&mut rendered_frame, t);
            }

            out.write_all(CURSOR_HOME.as_bytes())?;
            out.write_all(rendered_frame.as_bytes())?;
            out.flush()?;

            frame_idx += 1;

            let elapsed = t0.elapsed().as_secs_f64();
            let wait = frame_t - elapsed;
            if wait > 0.0 {
                std::thread::sleep(Duration::from_secs_f64(wait));
            }
        }

        Ok(())
    }

    /// Append ANSI sequences that draw the active subtitle cue (if any) into the
    /// bottom band, using absolute cursor positioning so placement is
    /// independent of the frame text already written.
    ///
    /// Up to `MAX_SUB_LINES` cue lines are shown, each centered and truncated to
    /// the terminal width, anchored near the bottom of the screen. Every band
    /// row is cleared to end-of-line so a previous, longer cue can't linger.
    fn append_subtitles(&self, frame: &mut String, t: f64) {
        /// How many cue lines we're willing to draw (keeps the band shallow).
        const MAX_SUB_LINES: usize = 2;
        const RESET: &str = "\x1b[0m";

        let subs = match &self.subtitles {
            Some(s) => s,
            None => return,
        };

        // Collect the lines we'll draw, truncated to the terminal width. We
        // borrow the cue lines briefly, then drop the borrow before writing.
        let display: Vec<String> = {
            let mut subs = subs.borrow_mut();
            let active = subs.active_at(t);
            active
                .iter()
                .take(MAX_SUB_LINES)
                .map(|l| truncate_to_width(l, self.term_cols))
                .collect()
        };

        // The band sits at the bottom: place the last cue line on the final
        // usable row, stacking earlier lines just above it. The video occupies
        // rows pad_y+1 ..= pad_y+rows; we never draw above pad_y+rows+1.
        let band_top = self.pad_y + self.rows as usize + 1;
        // 1-based bottom-most row we may use.
        let last_row = self.term_lines;
        // How many rows we can actually fit in the band.
        let band_rows = last_row.saturating_sub(band_top) + 1;
        let n = display.len().min(band_rows);
        // First row of the (n-line) block, bottom-anchored at `last_row`.
        let first_row = last_row + 1 - n.max(1);

        // Clear the rows the previous frame's cue occupied but this frame's
        // doesn't — otherwise a cue that ended, or a shorter one, would leave
        // stale text lingering during silence. Both blocks are bottom-anchored
        // at `last_row`, so the previously-drawn rows are the bottom `prev_n`;
        // the ones not covered by this frame's `n` rows sit just above them.
        let prev_n = self.prev_sub_rows.get();
        if prev_n > n {
            let clear_from = last_row + 1 - prev_n;
            let clear_to = last_row - n; // last row not covered this frame
            for row in clear_from..=clear_to {
                frame.push_str(&format!("\x1b[{row};1H\x1b[2K"));
            }
        }

        for (i, line) in display.iter().take(n).enumerate() {
            let row = first_row + i;
            let width = line.chars().count();
            let col = 1 + self.term_cols.saturating_sub(width) / 2;
            // Move to (row, 1), clear the whole line, then move to the centered
            // column and print. Clearing at col 1 wipes any stale, wider cue.
            frame.push_str(&format!(
                "\x1b[{row};1H\x1b[2K\x1b[{row};{col}H{line}{RESET}"
            ));
        }

        // Remember how many rows we actually drew, for next frame's cleanup.
        self.prev_sub_rows.set(n);
    }
}

/// Truncate a string to at most `max` display columns (counting `char`s, a good
/// enough proxy here), so an over-long subtitle line can't wrap or overflow.
fn truncate_to_width(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars()
            .take(max.saturating_sub(1))
            .chain(std::iter::once('…'))
            .collect()
    }
}

/// Best-effort terminal size as (cols, lines).
///
/// Tries `stty size` (the controlling tty), then the COLUMNS/LINES env vars.
/// Returns None so the caller can fall back to a sensible default (220, 50).
fn terminal_size() -> Option<(u32, u32)> {
    if let Some(sz) = stty_size() {
        return Some(sz);
    }
    let cols = std::env::var("COLUMNS").ok()?.parse().ok()?;
    let lines = std::env::var("LINES").ok()?.parse().ok()?;
    Some((cols, lines))
}

fn stty_size() -> Option<(u32, u32)> {
    use std::process::Command;
    // `stty size` prints "<lines> <cols>"; read it from the controlling tty.
    let out = Command::new("stty")
        .arg("size")
        .stdin(std::process::Stdio::inherit())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.split_whitespace();
    let lines: u32 = parts.next()?.parse().ok()?;
    let cols: u32 = parts.next()?.parse().ok()?;
    if cols == 0 || lines == 0 {
        return None;
    }
    Some((cols, lines))
}
