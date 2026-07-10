//! vlscii
//! ======
//! Modular, True Color (24-bit ANSI), zero-flicker terminal video player.
//!
//! Decoding is delegated to `ffprobe` (metadata) and `ffmpeg` (decode + resize),
//! reading raw `rgb24` frames from ffmpeg's stdout — no native media bindings.
//!
//!   - VideoProbe      : video metadata via ffprobe.
//!   - FrameReader     : raw rgb24 frames at the target grid size via ffmpeg.
//!   - Mapper          : rgb frame -> colored cells (ascii / braille / half-block).
//!   - TerminalRenderer: main loop, FPS pacing, orientation detection, render.
//!
//! Requires `ffmpeg` and `ffprobe` on PATH.

mod ascii;
mod braille;
mod decoder;
mod half_block;
mod mapper;
mod renderer;
mod subtitles;
mod term;

use crate::renderer::RenderMode;
use crate::subtitles::Subtitles;
use clap::{Parser, ValueEnum};
use std::process::exit;

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RenderModeArg {
    /// Glyph ramp: one ASCII char per cell, per-pixel color (default).
    Ascii,
    /// Unicode braille: 2×4 dots per cell, per-cell color (higher detail).
    Braille,
    /// Half-block (▀): 1×2 sub-pixels per cell, two full colors per cell.
    HalfBlock,
}

impl From<RenderModeArg> for RenderMode {
    fn from(m: RenderModeArg) -> RenderMode {
        match m {
            RenderModeArg::Ascii => RenderMode::Ascii,
            RenderModeArg::Braille => RenderMode::Braille,
            RenderModeArg::HalfBlock => RenderMode::HalfBlock,
        }
    }
}

/// Version string reported by `--version`: the crate version plus the short git
/// commit the binary was built from (baked in by `build.rs`).
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("VLSCII_GIT_HASH"), ")");

/// True Color ANSI ASCII video player — zero flicker.
#[derive(Parser, Debug)]
#[command(name = "vlscii", about, long_about = None, version = VERSION)]
struct Args {
    /// Path to video file (MP4, AVI, MKV ...)
    video: String,

    /// Render mode: ascii glyph ramp or braille sub-pixel dots
    #[arg(long, value_enum, default_value_t = RenderModeArg::Ascii)]
    render: RenderModeArg,

    /// Custom character palette, space-separated (ascii mode only)
    #[arg(long)]
    palette: Option<String>,

    /// Color quality: 0=max quality, 3=max speed
    #[arg(short, long, default_value_t = 0, value_parser = clap::value_parser!(u8).range(0..=3))]
    quality: u8,

    /// Fixed grid width. If 0, auto-fits to terminal
    #[arg(short, long, default_value_t = 0)]
    cols: u32,

    /// Braille dot luma threshold 0-255: lower = more dots lit. Acts as the
    /// black/white split point when --dither is on (braille mode only)
    #[arg(short, long, default_value_t = 50, value_parser = clap::value_parser!(u8))]
    threshold: u8,

    /// Floyd–Steinberg dither braille dots so gradients stipple instead of
    /// going flat (braille mode only; pairs well with --threshold 128)
    #[arg(long, default_value_t = false)]
    dither: bool,

    /// Path to an external subtitle file (.srt). Overrides embedded tracks.
    #[arg(long)]
    subtitles: Option<String>,

    /// Index of the embedded subtitle track to use (0 = first). Ignored when
    /// --subtitles is given. If omitted, the first text track is used.
    #[arg(long)]
    subtitles_track: Option<u32>,

    /// Show the info splash for <n> seconds before playback. 0 disables it.
    #[arg(long, default_value_t = 0, value_name = "SECS")]
    splash: u32,
}

fn main() {
    let args = Args::parse();

    let palette: Option<Vec<char>> = args.palette.as_ref().map(|p| {
        // Whitespace-separated palette tokens, flattened to glyphs.
        p.split_whitespace().flat_map(|tok| tok.chars()).collect()
    });

    // Load subtitles before entering the alt screen so errors land on the
    // normal terminal. Precedence:
    //   --subtitles <file>        : external file (hard error if unreadable).
    //   --subtitles-track <n>     : that embedded track (hard error if missing).
    //   neither                   : auto-use the first embedded text track if
    //                               one exists; silently skip if none.
    let subtitles: Option<Subtitles> = match (&args.subtitles, args.subtitles_track) {
        (Some(path), _) => Some(match Subtitles::from_file(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("\n[Error] {e}");
                exit(1);
            }
        }),
        (None, Some(track)) => Some(match Subtitles::from_embedded(&args.video, Some(track)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("\n[Error] {e}");
                exit(1);
            }
        }),
        // No explicit request: try the first embedded text track, but never
        // fail the run if there isn't one (most files have no subtitles).
        (None, None) => Subtitles::from_embedded(&args.video, None).ok(),
    };

    let renderer = match renderer::TerminalRenderer::new(
        &args.video,
        palette,
        args.quality as u32,
        args.cols,
        args.render.into(),
        args.threshold,
        args.dither,
        subtitles,
        args.splash,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("\n[Error] {e}");
            exit(1);
        }
    };

    if let Err(e) = renderer.play() {
        eprintln!("\n[Error] {e}");
        exit(1);
    }
}
