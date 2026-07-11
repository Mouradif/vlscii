# vlscii

**True-color ANSI terminal video player**

`vlscii` plays video *inside your terminal*. It hands decoding off to `ffmpeg`/`ffprobe`, reads back raw `rgb24` frames, and maps each frame to colored terminal cells as an ASCII glyph ramp, Unicode braille sub-pixels, or half-block sub-pixels, painting them on the alternate screen with 24-bit color and no flicker.

---

## Requirements

- **`ffmpeg`** and **`ffprobe`** on your `PATH`
- A terminal with **24-bit truecolor** support (most modern terminals)

## Install

### Package managers (coming soon)

### From source
```sh
git clone https://github.com/Mouradif/vlscii
cd vlscii
cargo build --release
```
Then move `./target/release/vlscii` to a directory in your PATH (like `~/.local/bin` on many UNIX systems)

## Usage

```sh
vlscii <VIDEO> [OPTIONS]
```

Play a file:

```sh
vlscii movie.mp4
```

Higher-detail braille rendering with dithering:

```sh
vlscii movie.mp4 --render braille --dither --threshold 128
```

Two-color half-block mode at a fixed width:

```sh
vlscii movie.mp4 --render half-block --cols 120
```

Press **Ctrl+C** to quit at any time, the terminal is restored cleanly.

### Options

| Flag | Description | Default |
|------|-------------|---------|
| `<VIDEO>` | Path to the video file (MP4, AVI, MKV, …). | *required* |
| `--render <MODE>` | `ascii`, `braille`, or `half-block`. | `ascii` |
| `--palette <STR>` | Custom glyph ramp, dark→bright, space-separated (ascii mode). | built-in 93-level ramp |
| `-q, --quality <0-3>` | Color quantization: `0` = max quality, `3` = max speed (fewer color levels → fewer escapes). | `0` |
| `-c, --cols <N>` | Fixed grid width in cells. `0` auto-fits to the terminal. | `0` |
| `-t, --threshold <0-255>` | Braille dot luma cutoff: lower = more dots lit. Also the black/white split when `--dither` is on. | `50` |
| `--dither` | Floyd–Steinberg dither braille dots (pairs well with `--threshold 128`). | off |
| `--subtitles <FILE>` | External `.srt` file. Overrides embedded tracks. | - |
| `--subtitles-track <N>` | Embedded subtitle track to use (`0` = first). Ignored if `--subtitles` is given. | first text track if it exists |
| `--splash <SECS>` | Show the info splash for N seconds before playback | `0` |
| `-V, --version` | Print the version (and the git commit it was built from). | - |
| `-h, --help` | Full help. | - |

## Subtitles

`vlscii` renders **plain text** subtitles centered in the bottom band, timed to playback. Styling markup is stripped, lines wider than the grid are truncated.

**External file**: pass any `.srt`:

```sh
vlscii movie.mp4 --subtitles movie.en.srt
```

**Embedded tracks**: when no `--subtitles` is given, `vlscii` automatically uses the first embedded *text* subtitle track if the container has one (e.g. MKV). To pick a specific track:

```sh
vlscii movie.mkv --subtitles-track 1
```

Text codecs (SubRip/ASS/SSA/`mov_text`/WebVTT…) are transcoded to SRT via `ffmpeg`. **Bitmap subtitles** (PGS, VobSub) can't be turned into text and are rejected.

## Render modes at a glance

| Mode | Detail per cell | Color | Best for |
|------|-----------------|-------|----------|
| `ascii` | 1 glyph, brightness-mapped | 1 truecolor/cell | Classic look, widest compatibility |
| `braille` | 2×4 = 8 dots | 1 truecolor/cell | Fine shapes, line art, high detail |
| `half-block` | 1×2 = 2 sub-pixels | 2 truecolors/cell | Color-rich, doubled vertical resolution |

## How it works

```
ffprobe ──► metadata (width, height, fps)
ffmpeg  ──► resize to the target grid, emit raw rgb24 frames on stdout
              │
              ▼
          Mapper  (ascii / braille / half-block)
              │  rgb frame → colored ANSI cells
              ▼
       TerminalRenderer  (alt screen, FPS pacing, centering, subtitles)
```

No native media bindings: all decoding is delegated to the `ffmpeg` tools, and frames are read straight off a pipe.

## Roadmap

- [ ] Controls for pause/play with Space
- [ ] Seek +/- 1s with arrow left/right
- [ ] Seek +/- 5s with modifier+arrow (likely shift)
- [ ] Add loop mode
- [ ] Got any idea? Open an issue 😁

## Development

```sh
cargo build      # debug build
cargo test       # unit tests (mappers, SRT parsing, subtitle timing)
cargo clippy     # lints
```

The source is organized one concern per module: `decoder` (ffmpeg/ffprobe), `mapper` + `ascii`/`braille`/`half_block` (frame → cells), `subtitles` (parsing + timed lookup), `term` (alt-screen + signal-safe teardown), and `renderer` (the playback loop).
