//! Video decoding via ffmpeg/ffprobe.
//!
//! `VideoProbe` gathers metadata (fps, width, height) with a single ffprobe
//! call. `FrameReader` drives ffmpeg to resize to the target grid and emit raw
//! `rgb24` frames, which we read frame-by-frame off its stdout pipe.

use std::io::{self, Read};
use std::process::{Child, ChildStdout, Command, Stdio};

/// Video metadata, gathered with a single `ffprobe` call.
pub struct VideoProbe {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
}

impl VideoProbe {
    pub fn open(path: &str) -> io::Result<VideoProbe> {
        let out = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height,r_frame_rate,avg_frame_rate",
                "-of",
                "default=noprint_wrappers=1",
            ])
            .arg(path)
            .output()
            .map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("failed to run ffprobe (is it installed?): {e}"),
                )
            })?;

        if !out.status.success() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Could not open video file: {path:?}"),
            ));
        }

        let text = String::from_utf8_lossy(&out.stdout);
        let mut width = 0u32;
        let mut height = 0u32;
        let mut r_fps: Option<f64> = None;
        let mut avg_fps: Option<f64> = None;

        for line in text.lines() {
            let (k, v) = match line.split_once('=') {
                Some(kv) => kv,
                None => continue,
            };
            match k {
                "width" => width = v.trim().parse().unwrap_or(0),
                "height" => height = v.trim().parse().unwrap_or(0),
                "r_frame_rate" => r_fps = parse_rational(v.trim()),
                "avg_frame_rate" => avg_fps = parse_rational(v.trim()),
                _ => {}
            }
        }

        if width == 0 || height == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Could not read video dimensions: {path:?}"),
            ));
        }

        // Prefer avg_frame_rate (true playback rate); fall back to r_frame_rate,
        // then to a sensible default of 24.0 if neither is reported.
        let fps = avg_fps
            .filter(|f| *f > 0.0)
            .or(r_fps.filter(|f| *f > 0.0))
            .unwrap_or(24.0);

        Ok(VideoProbe { width, height, fps })
    }
}

/// Parse an ffprobe rational like "30000/1001" or "25/1" into f64.
fn parse_rational(s: &str) -> Option<f64> {
    if let Some((num, den)) = s.split_once('/') {
        let n: f64 = num.trim().parse().ok()?;
        let d: f64 = den.trim().parse().ok()?;
        if d == 0.0 {
            return None;
        }
        Some(n / d)
    } else {
        s.parse().ok()
    }
}

/// Streams raw `rgb24` frames at exactly `cols`x`rows`, one frame per `next`.
///
/// ffmpeg performs the resize (bilinear scaling), so a frame is always
/// `cols * rows * 3` bytes. Reads block until a full frame is available or the
/// stream ends.
pub struct FrameReader {
    child: Child,
    stdout: ChildStdout,
    frame_bytes: usize,
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn open(path: &str, cols: u32, rows: u32) -> io::Result<FrameReader> {
        let scale = format!("scale={cols}:{rows}:flags=bilinear");
        let mut child = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(path)
            .args([
                "-vf",
                &scale,
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "-", // stdout
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()
            .map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("failed to run ffmpeg (is it installed?): {e}"),
                )
            })?;

        let stdout = child
            .stdout
            .take()
            .expect("ffmpeg stdout was piped");

        let frame_bytes = (cols as usize) * (rows as usize) * 3;
        Ok(FrameReader {
            child,
            stdout,
            frame_bytes,
            buf: vec![0u8; frame_bytes],
        })
    }

    /// Read the next frame as a borrowed `rgb24` slice, or `None` at EOF.
    pub fn next_frame(&mut self) -> io::Result<Option<&[u8]>> {
        match read_exact_or_eof(&mut self.stdout, &mut self.buf)? {
            true => Ok(Some(&self.buf[..self.frame_bytes])),
            false => Ok(None),
        }
    }
}

impl Drop for FrameReader {
    fn drop(&mut self) {
        // Make sure ffmpeg goes away even if we stop reading early (e.g. Ctrl+C).
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Fill `buf` completely. Returns Ok(true) on a full read, Ok(false) on a clean
/// EOF at a frame boundary, or Err on a partial frame / I/O error.
fn read_exact_or_eof(r: &mut impl Read, buf: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => {
                if filled == 0 {
                    return Ok(false); // clean EOF
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "ffmpeg ended mid-frame",
                ));
            }
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}
