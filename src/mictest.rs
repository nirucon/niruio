//! Microphone test: record a few seconds from one input into a private
//! temporary WAV file, then report peak, RMS and clipping per channel. The
//! recording is played back only when asked, and deleted when NIRUIO quits.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Running {
    pub source: u32,
    pub started: Instant,
    pub seconds: u64,
    child: Child,
    pub file: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Result {
    pub source: u32,
    pub file: PathBuf,
    pub channels: usize,
    pub peak_db: Vec<f32>,
    pub rms_db: Vec<f32>,
    pub clipped: Vec<usize>,
    pub seconds: f32,
}

impl Result {
    pub fn loudest(&self) -> (usize, f32) {
        self.peak_db
            .iter()
            .copied()
            .enumerate()
            .fold(
                (0, -120.0),
                |acc, (i, v)| if v > acc.1 { (i, v) } else { acc },
            )
    }

    pub fn verdict(&self) -> &'static str {
        let (_, peak) = self.loudest();
        if self.clipped.iter().any(|c| *c > 0) {
            "too loud: it clips, lower the gain or boost"
        } else if peak < -50.0 {
            "silent: nothing reached this input"
        } else if peak < -30.0 {
            "very quiet: raise the gain or move closer"
        } else if peak < -12.0 {
            "a little quiet but usable"
        } else {
            "good level"
        }
    }
}

pub fn temp_file() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join(format!("niruio-mictest-{}.wav", std::process::id()))
}

pub fn start(
    source: u32,
    serial: u64,
    channels: usize,
    seconds: u64,
) -> std::result::Result<Running, String> {
    let file = temp_file();
    let _ = std::fs::remove_file(&file);
    let channels = channels.clamp(1, 32);
    let child = Command::new("pw-record")
        .args(["--target", &serial.to_string()])
        .args(["--rate", "48000", "--format", "s16"])
        .args(["--channels", &channels.to_string()])
        .args([
            "-P",
            "{ application.name=niruio node.name=niruio-mictest state.restore-props=false \
             state.restore-target=false node.dont-reconnect=true stream.dont-remix=true }",
        ])
        .arg(&file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("pw-record: {e}"))?;
    Ok(Running {
        source,
        started: Instant::now(),
        seconds,
        child,
        file,
    })
}

impl Running {
    pub fn remaining(&self) -> u64 {
        self.seconds
            .saturating_sub(self.started.elapsed().as_secs())
    }

    /// Stop when the time is up; then analyse the file.
    pub fn poll(&mut self) -> Option<std::result::Result<Result, String>> {
        if let Ok(Some(_)) = self.child.try_wait() {
            return Some(analyse(self.source, &self.file));
        }
        if self.started.elapsed() < Duration::from_secs(self.seconds) {
            return None;
        }
        // SIGINT lets pw-record finish the WAV header.
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGINT);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        Some(analyse(self.source, &self.file))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn db(x: f64) -> f32 {
    if x <= 1e-9 {
        -120.0
    } else {
        (20.0 * x.log10()) as f32
    }
}

/// Analyse 16-bit PCM WAV data. A header with a zero or bogus data size (a
/// recording that was cut off) is read to the end of the file.
pub fn analyse_bytes(
    source: u32,
    file: PathBuf,
    bytes: &[u8],
) -> std::result::Result<Result, String> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("the test recording is not a WAV file".into());
    }
    let mut at = 12;
    let mut channels = 0usize;
    let mut rate = 48000u32;
    let mut bits = 16u16;
    let mut data: Option<&[u8]> = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]])
            as usize;
        let body = at + 8;
        if id == b"fmt " && body + 16 <= bytes.len() {
            channels = u16::from_le_bytes([bytes[body + 2], bytes[body + 3]]) as usize;
            rate = u32::from_le_bytes([
                bytes[body + 4],
                bytes[body + 5],
                bytes[body + 6],
                bytes[body + 7],
            ]);
            bits = u16::from_le_bytes([bytes[body + 14], bytes[body + 15]]);
        }
        if id == b"data" {
            let end = if size == 0 || body + size > bytes.len() {
                bytes.len()
            } else {
                body + size
            };
            data = Some(&bytes[body..end]);
            break;
        }
        at = body + size + (size & 1);
    }
    let data = data.ok_or("the test recording has no sound data")?;
    if channels == 0 || bits != 16 {
        return Err("unexpected WAV format".into());
    }
    let mut peak = vec![0f64; channels];
    let mut sum = vec![0f64; channels];
    let mut clipped = vec![0usize; channels];
    let mut frames = 0usize;
    for frame in data.chunks_exact(2 * channels) {
        for c in 0..channels {
            let v = i16::from_le_bytes([frame[2 * c], frame[2 * c + 1]]);
            let x = (v as f64 / 32768.0).abs();
            if x > peak[c] {
                peak[c] = x;
            }
            sum[c] += x * x;
            if v == i16::MAX || v == i16::MIN {
                clipped[c] += 1;
            }
        }
        frames += 1;
    }
    Ok(Result {
        source,
        file,
        channels,
        peak_db: peak.iter().map(|p| db(*p)).collect(),
        rms_db: sum
            .iter()
            .map(|s| db((s / frames.max(1) as f64).sqrt()))
            .collect(),
        clipped,
        seconds: frames as f32 / rate.max(1) as f32,
    })
}

pub fn analyse(source: u32, file: &PathBuf) -> std::result::Result<Result, String> {
    let bytes = std::fs::read(file).map_err(|e| format!("test recording: {e}"))?;
    analyse_bytes(source, file.clone(), &bytes)
}

/// Play the test recording on the default output (only on request).
pub fn play(file: &PathBuf) -> std::result::Result<Child, String> {
    Command::new("pw-play")
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("pw-play: {e}"))
}

#[cfg(test)]
pub fn wav(channels: u16, samples: &[i16]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&channels.to_le_bytes());
    b.extend_from_slice(&48000u32.to_le_bytes());
    b.extend_from_slice(&(48000u32 * 2 * channels as u32).to_le_bytes());
    b.extend_from_slice(&(2 * channels).to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&0u32.to_le_bytes());
    for s in samples {
        b.extend_from_slice(&s.to_le_bytes());
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyse_levels_and_clipping() {
        let samples: Vec<i16> = (0..4800)
            .flat_map(|i| [if i == 7 { i16::MAX } else { 100 }, 16384])
            .collect();
        let r = analyse_bytes(1, PathBuf::from("x"), &wav(2, &samples)).unwrap();
        assert_eq!(r.channels, 2);
        assert_eq!(r.clipped, vec![1, 0]);
        assert!((r.peak_db[1] + 6.02).abs() < 0.05);
        assert!((r.rms_db[1] + 6.02).abs() < 0.05);
        assert!((r.seconds - 0.1).abs() < 0.001);
        assert_eq!(r.verdict(), "too loud: it clips, lower the gain or boost");
        let silent = analyse_bytes(1, PathBuf::from("x"), &wav(1, &[0; 480])).unwrap();
        assert_eq!(silent.verdict(), "silent: nothing reached this input");
        assert!(analyse_bytes(1, PathBuf::from("x"), b"nope").is_err());
    }
}
