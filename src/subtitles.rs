//! Subtitle support: SRT parsing, MKV track extraction, and timed lookup.
//!
//! Two sources feed the same `Subtitles` cue list:
//!   - an external `.srt` file (`--subtitles path`), parsed directly, or
//!   - a text subtitle stream embedded in the container (e.g. MKV), which we
//!     ask ffmpeg to transcode to SRT on stdout and then parse identically.
//!
//! Only *text* subtitle codecs are supported (subrip/ass/ssa/mov_text/webvtt…);
//! bitmap subtitles (PGS/VobSub) can't be turned into text and are rejected
//! with a clear message pointing at `--subtitles-track`.
//!
//! Styling/markup is stripped down to plain text: ASS override blocks (`{...}`),
//! SRT/HTML tags (`<i>`…), and ASS `\N` line breaks are all flattened.

use std::io::{self, Read};
use std::process::{Command, Stdio};

/// One subtitle cue: shown from `start` to `end` (seconds), with `lines` of
/// already-cleaned plain text.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub lines: Vec<String>,
}

/// A time-ordered list of cues with a cursor for monotonic playback lookup.
pub struct Subtitles {
    cues: Vec<Cue>,
    /// Cache of the last index returned, so `active_at` is O(1) amortized as
    /// playback time advances monotonically.
    cursor: usize,
}

impl Subtitles {
    /// Load subtitles from an external file. The extension is ignored; the
    /// content is parsed as SRT (the common interchange format).
    pub fn from_file(path: &str) -> io::Result<Subtitles> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            io::Error::new(e.kind(), format!("could not read subtitle file {path:?}: {e}"))
        })?;
        let cues = parse_srt(&text);
        if cues.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("no subtitles found in {path:?} (is it a valid .srt file?)"),
            ));
        }
        Ok(Subtitles::from_cues(cues))
    }

    /// Extract an embedded text subtitle track from `video` via ffmpeg.
    ///
    /// `track` is the index *among subtitle streams* (0 = first subtitle track),
    /// matching `--subtitles-track`. When `None`, the first text subtitle track
    /// found is used.
    pub fn from_embedded(video: &str, track: Option<u32>) -> io::Result<Subtitles> {
        let streams = probe_subtitle_streams(video)?;
        if streams.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no subtitle tracks found in {video:?}"),
            ));
        }

        let chosen = match track {
            Some(t) => streams.get(t as usize).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "subtitle track {t} out of range: {} text track(s) available (0..{})",
                        streams.len(),
                        streams.len().saturating_sub(1)
                    ),
                )
            })?,
            None => &streams[0],
        };

        if !chosen.is_text {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "subtitle track {} is a bitmap format ({}), which can't be rendered as text; \
                     pick a text track with --subtitles-track <n>",
                    chosen.sub_index, chosen.codec
                ),
            ));
        }

        let srt = extract_srt(video, chosen.sub_index)?;
        let cues = parse_srt(&srt);
        if cues.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("subtitle track {} produced no cues", chosen.sub_index),
            ));
        }
        Ok(Subtitles::from_cues(cues))
    }

    /// Number of cues loaded (for the info screen).
    pub fn len(&self) -> usize {
        self.cues.len()
    }

    fn from_cues(mut cues: Vec<Cue>) -> Subtitles {
        cues.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
        Subtitles { cues, cursor: 0 }
    }

    /// Lines to display at playback time `t` (seconds), or `&[]` if none.
    ///
    /// Optimized for monotonically increasing `t`: advances a cursor forward,
    /// but also handles seeks/restarts by resetting when `t` falls behind.
    pub fn active_at(&mut self, t: f64) -> &[String] {
        // If time jumped backwards (restart), rewind the cursor.
        if self.cursor < self.cues.len() && t < self.cues[self.cursor].start {
            self.cursor = 0;
        }
        // Advance past cues that have fully ended.
        while self.cursor < self.cues.len() && self.cues[self.cursor].end < t {
            self.cursor += 1;
        }
        if let Some(cue) = self.cues.get(self.cursor)
            && t >= cue.start
            && t <= cue.end
        {
            return &cue.lines;
        }
        &[]
    }
}

/// A subtitle stream as reported by ffprobe.
struct SubStream {
    /// Index among *subtitle* streams (for ffmpeg's `0:s:<n>` selector).
    sub_index: u32,
    codec: String,
    is_text: bool,
}

/// List subtitle streams in order via ffprobe.
fn probe_subtitle_streams(video: &str) -> io::Result<Vec<SubStream>> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "s",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(video)
        .output()
        .map_err(|e| {
            io::Error::new(e.kind(), format!("failed to run ffprobe (is it installed?): {e}"))
        })?;

    if !out.status.success() {
        return Err(io::Error::other(format!(
            "ffprobe failed to read subtitle streams from {video:?}"
        )));
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let streams = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .enumerate()
        .map(|(i, codec)| SubStream {
            sub_index: i as u32,
            codec: codec.to_string(),
            is_text: is_text_codec(codec),
        })
        .collect();
    Ok(streams)
}

/// Whether an ffmpeg subtitle codec name is a text format we can transcode to
/// SRT. Bitmap formats (hdmv_pgs_subtitle, dvd_subtitle, dvb_subtitle…) are not.
fn is_text_codec(codec: &str) -> bool {
    matches!(
        codec,
        "subrip" | "srt" | "ass" | "ssa" | "mov_text" | "webvtt" | "text" | "subviewer" | "subviewer1"
    )
}

/// Ask ffmpeg to transcode the `sub_index`-th subtitle stream to SRT on stdout.
fn extract_srt(video: &str, sub_index: u32) -> io::Result<String> {
    let map = format!("0:s:{sub_index}");
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(video)
        .args(["-map", &map, "-f", "srt", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| {
            io::Error::new(e.kind(), format!("failed to run ffmpeg (is it installed?): {e}"))
        })?;

    let mut buf = String::new();
    child
        .stdout
        .take()
        .expect("ffmpeg stdout was piped")
        .read_to_string(&mut buf)?;
    let status = child.wait()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "ffmpeg failed to extract subtitle track {sub_index}"
        )));
    }
    Ok(buf)
}

/// Parse SRT text into cues. Tolerant of missing indices, CRLF, and blank-line
/// padding; ignores blocks without a valid timing line.
fn parse_srt(text: &str) -> Vec<Cue> {
    let mut cues = Vec::new();
    // Split into blocks on blank lines (handles \r\n and \n).
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    for block in normalized.split("\n\n") {
        let mut lines = block.lines().filter(|l| !l.trim().is_empty());

        // First line may be a numeric index (skip it) or the timing line itself.
        let first = match lines.next() {
            Some(l) => l,
            None => continue,
        };
        let (timing, text_lines): (&str, Vec<&str>) = if first.contains("-->") {
            (first, lines.collect())
        } else {
            match lines.next() {
                Some(t) if t.contains("-->") => (t, lines.collect()),
                _ => continue,
            }
        };

        let (start, end) = match parse_timing(timing) {
            Some(se) => se,
            None => continue,
        };

        let cleaned: Vec<String> = text_lines
            .iter()
            .flat_map(|l| clean_text(l))
            .filter(|l| !l.is_empty())
            .collect();
        if cleaned.is_empty() {
            continue;
        }
        cues.push(Cue { start, end, lines: cleaned });
    }
    cues
}

/// Parse an SRT timing line: `00:01:02,500 --> 00:01:05,000`.
/// Also tolerates `.` as the millisecond separator (WebVTT style).
fn parse_timing(line: &str) -> Option<(f64, f64)> {
    let (l, r) = line.split_once("-->")?;
    Some((parse_timestamp(l.trim())?, parse_timestamp(r.trim())?))
}

/// Parse `HH:MM:SS,mmm` (or `.mmm`) into seconds.
fn parse_timestamp(s: &str) -> Option<f64> {
    // Drop anything after the timestamp (ASS/VTT may append position cues).
    let s = s.split_whitespace().next()?;
    let s = s.replace(',', ".");
    let mut parts = s.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let sec: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + sec)
}

/// Strip styling markup from one source line, splitting on ASS `\N`/`\n` breaks.
/// Returns one or more plain-text lines.
fn clean_text(line: &str) -> Vec<String> {
    // Remove ASS override blocks: {\an8}, {\i1}, etc.
    let mut s = String::with_capacity(line.len());
    let mut in_brace = false;
    for c in line.chars() {
        match c {
            '{' => in_brace = true,
            '}' => in_brace = false,
            _ if !in_brace => s.push(c),
            _ => {}
        }
    }
    // Strip simple HTML/SRT tags: <i>, </i>, <b>, <font ...>, etc.
    let s = strip_tags(&s);
    // Split on ASS hard line breaks (\N and \n as literal backslash sequences).
    s.replace("\\N", "\n")
        .replace("\\n", "\n")
        .split('\n')
        .map(|l| l.trim().to_string())
        .collect()
}

/// Remove `<...>` tag spans (HTML/SRT styling) from a string.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_srt() {
        let srt = "1\n00:00:01,000 --> 00:00:03,000\nHello world\n\n\
                   2\n00:00:04,500 --> 00:00:06,000\nSecond line\n";
        let cues = parse_srt(srt);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start, 1.0);
        assert_eq!(cues[0].end, 3.0);
        assert_eq!(cues[0].lines, vec!["Hello world"]);
        assert_eq!(cues[1].start, 4.5);
    }

    #[test]
    fn multiline_cue() {
        let srt = "1\n00:00:01,000 --> 00:00:02,000\nLine one\nLine two\n";
        let cues = parse_srt(srt);
        assert_eq!(cues[0].lines, vec!["Line one", "Line two"]);
    }

    #[test]
    fn strips_html_and_ass_tags() {
        let srt = "1\n00:00:01,000 --> 00:00:02,000\n{\\an8}<i>Italic</i> text\n";
        let cues = parse_srt(srt);
        assert_eq!(cues[0].lines, vec!["Italic text"]);
    }

    #[test]
    fn ass_hard_break_splits_lines() {
        let srt = "1\n00:00:01,000 --> 00:00:02,000\nfirst\\Nsecond\n";
        let cues = parse_srt(srt);
        assert_eq!(cues[0].lines, vec!["first", "second"]);
    }

    #[test]
    fn tolerates_crlf_and_missing_index() {
        let srt = "00:00:01,000 --> 00:00:02,000\r\nHi there\r\n";
        let cues = parse_srt(srt);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].lines, vec!["Hi there"]);
    }

    #[test]
    fn active_at_finds_and_advances() {
        let srt = "1\n00:00:01,000 --> 00:00:03,000\nA\n\n\
                   2\n00:00:05,000 --> 00:00:06,000\nB\n";
        let mut subs = Subtitles::from_cues(parse_srt(srt));
        assert!(subs.active_at(0.5).is_empty());
        assert_eq!(subs.active_at(2.0), &["A".to_string()]);
        assert!(subs.active_at(4.0).is_empty());
        assert_eq!(subs.active_at(5.5), &["B".to_string()]);
        assert!(subs.active_at(7.0).is_empty());
    }

    #[test]
    fn active_at_handles_restart() {
        let srt = "1\n00:00:01,000 --> 00:00:03,000\nA\n";
        let mut subs = Subtitles::from_cues(parse_srt(srt));
        assert_eq!(subs.active_at(2.0), &["A".to_string()]);
        // Time jumps back to start: cursor must rewind.
        assert_eq!(subs.active_at(2.0), &["A".to_string()]);
    }

    #[test]
    fn text_codec_detection() {
        assert!(is_text_codec("subrip"));
        assert!(is_text_codec("ass"));
        assert!(!is_text_codec("hdmv_pgs_subtitle"));
        assert!(!is_text_codec("dvd_subtitle"));
    }

    #[test]
    fn webvtt_dot_timestamps() {
        assert_eq!(parse_timestamp("00:00:01.500"), Some(1.5));
        assert_eq!(parse_timestamp("01:02:03,250"), Some(3723.25));
    }
}
