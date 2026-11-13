//! Live level meters. One `pw-record` per metered node writes raw 32-bit
//! float samples to a pipe; a reader thread keeps the peak of each channel
//! since the screen last asked. Meters run only for what the current tab
//! shows and stop when NIRUIO leaves the tab or quits.
//!
//! Outputs are metered from their monitor (`stream.capture.sink`), programs
//! from their own stream. Meter streams are passive where possible, so they
//! never keep an idle device awake, and are marked so WirePlumber neither
//! remembers nor moves them.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Spec {
    pub node: u32,
    /// `--target`: the node's serial.
    pub target: String,
    pub channels: usize,
    /// e.g. `FL,FR` or `AUX0,...,AUX17`; empty for the default.
    pub positions: String,
    /// Meter an output through its monitor.
    pub sink: bool,
    /// Passive: does not wake a suspended device.
    pub passive: bool,
}

struct Running {
    spec: Spec,
    child: Child,
    peaks: Arc<Mutex<Vec<f32>>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
pub struct Meters {
    running: HashMap<u32, Running>,
    raw: Option<bool>,
    pub disabled: bool,
}

/// Peak per channel of interleaved little-endian f32 samples.
pub fn peaks(bytes: &[u8], channels: usize, into: &mut [f32]) {
    let channels = channels.max(1);
    for (i, sample) in bytes.chunks_exact(4).enumerate() {
        let v = f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]).abs();
        let c = i % channels;
        if c < into.len() && v.is_finite() && v > into[c] {
            into[c] = v;
        }
    }
}

/// Skip a WAV header if the stream starts with one; return the offset of the
/// first sample. Older `pw-record` versions without `--raw` write WAV.
pub fn wav_data_offset(head: &[u8]) -> Option<usize> {
    if head.len() < 12 || &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return None;
    }
    let mut at = 12;
    while at + 8 <= head.len() {
        let id = &head[at..at + 4];
        let size = u32::from_le_bytes([head[at + 4], head[at + 5], head[at + 6], head[at + 7]]);
        if id == b"data" {
            return Some(at + 8);
        }
        at += 8 + size as usize;
    }
    None
}

pub fn to_db(peak: f32) -> f32 {
    if peak <= 0.000_001 {
        -120.0
    } else {
        20.0 * peak.log10()
    }
}

impl Meters {
    pub fn new() -> Self {
        Self {
            disabled: std::env::var("NIRUIO_NO_METERS").is_ok(),
            ..Self::default()
        }
    }

    pub fn off() -> Self {
        Self {
            disabled: true,
            ..Self::default()
        }
    }

    fn supports_raw(&mut self) -> bool {
        if let Some(raw) = self.raw {
            return raw;
        }
        let raw = Command::new("pw-record")
            .arg("--help")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("--raw"))
            .unwrap_or(false);
        self.raw = Some(raw);
        raw
    }

    /// Run exactly the meters in `wanted`.
    pub fn sync(&mut self, wanted: &[Spec]) {
        self.running
            .retain(|id, r| wanted.iter().any(|w| w.node == *id && *w == r.spec));
        if self.disabled {
            return;
        }
        for spec in wanted {
            if self.running.contains_key(&spec.node) {
                continue;
            }
            if let Some(r) = self.spawn(spec) {
                self.running.insert(spec.node, r);
            }
        }
    }

    pub fn stop_all(&mut self) {
        self.running.clear();
    }

    pub fn count(&self) -> usize {
        self.running.len()
    }

    fn spawn(&mut self, spec: &Spec) -> Option<Running> {
        let raw = self.supports_raw();
        let mut props = String::from(
            "{ application.name=niruio node.name=niruio-meter media.category=Monitor \
             node.dont-reconnect=true state.restore-props=false state.restore-target=false \
             stream.dont-remix=true",
        );
        if spec.sink {
            props.push_str(" stream.capture.sink=true");
        }
        if spec.passive {
            props.push_str(" node.passive=true");
        }
        props.push_str(" }");
        let channels = spec.channels.clamp(1, 64);
        let mut cmd = Command::new("pw-record");
        cmd.args(["--target", &spec.target])
            .args(["--rate", "48000", "--format", "f32"])
            .args(["--channels", &channels.to_string()])
            .args(["--latency", "40ms"])
            .args(["-P", &props]);
        if !spec.positions.is_empty() {
            cmd.args(["--channel-map", &spec.positions]);
        }
        if raw {
            cmd.arg("--raw");
        }
        cmd.arg("-");
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut out = child.stdout.take()?;
        let shared_peaks = Arc::new(Mutex::new(vec![0.0f32; channels]));
        let shared = shared_peaks.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 16384];
            let mut carry: Vec<u8> = Vec::new();
            let mut header_checked = false;
            let mut local = vec![0.0f32; channels];
            loop {
                let n = match out.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                carry.extend_from_slice(&buf[..n]);
                if !header_checked {
                    if carry.len() < 256 && carry.starts_with(b"RIFF") {
                        continue;
                    }
                    if let Some(at) = wav_data_offset(&carry) {
                        carry.drain(..at);
                    }
                    header_checked = true;
                }
                let frame = 4 * channels;
                let usable = carry.len() / frame * frame;
                local.iter_mut().for_each(|p| *p = 0.0);
                peaks(&carry[..usable], channels, &mut local);
                carry.drain(..usable);
                if let Ok(mut p) = shared.lock() {
                    for (dst, src) in p.iter_mut().zip(&local) {
                        if *src > *dst {
                            *dst = *src;
                        }
                    }
                }
            }
        });
        Some(Running {
            spec: spec.clone(),
            child,
            peaks: shared_peaks,
        })
    }

    /// Peaks since the last call, per channel (linear 0..1+).
    pub fn take(&mut self, node: u32) -> Option<Vec<f32>> {
        let r = self.running.get(&node)?;
        let mut p = r.peaks.lock().ok()?;
        let out = p.clone();
        p.iter_mut().for_each(|x| *x = 0.0);
        Some(out)
    }
}

/// Smoothed meter levels in dBFS for the screen: instant attack, steady
/// fall, and a peak-hold marker that clips stay visible on.
#[derive(Clone, Debug, Default)]
pub struct Level {
    pub db: Vec<f32>,
    pub hold: Vec<f32>,
    pub hold_age: Vec<f32>,
    pub clipped: bool,
    clip_age: f32,
}

impl Level {
    #[allow(clippy::needless_range_loop)]
    pub fn update(&mut self, peaks: &[f32], dt: f32) {
        let n = peaks.len();
        if self.db.len() != n {
            self.db = vec![-120.0; n];
            self.hold = vec![-120.0; n];
            self.hold_age = vec![0.0; n];
        }
        // A clip stays visible for three seconds.
        self.clip_age += dt;
        if self.clip_age > 3.0 {
            self.clipped = false;
        }
        for i in 0..n {
            let db = to_db(peaks[i]);
            self.db[i] = if db > self.db[i] {
                db
            } else {
                (self.db[i] - 30.0 * dt).max(db).max(-120.0)
            };
            self.hold_age[i] += dt;
            if db >= self.hold[i] || self.hold_age[i] > 1.5 {
                self.hold[i] = db.max(self.db[i]);
                self.hold_age[i] = 0.0;
            }
            if peaks[i] >= 0.999 {
                self.clipped = true;
                self.clip_age = 0.0;
            }
        }
    }

    /// The loudest channel.
    pub fn max_db(&self) -> f32 {
        self.db.iter().copied().fold(-120.0, f32::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_per_channel() {
        let mut bytes = Vec::new();
        for (l, r) in [(0.1f32, -0.5f32), (-0.25, 0.2)] {
            bytes.extend_from_slice(&l.to_le_bytes());
            bytes.extend_from_slice(&r.to_le_bytes());
        }
        let mut p = vec![0.0; 2];
        peaks(&bytes, 2, &mut p);
        assert_eq!(p, vec![0.25, 0.5]);
    }

    #[test]
    fn wav_header() {
        let mut h = Vec::new();
        h.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt ");
        h.extend_from_slice(&16u32.to_le_bytes());
        h.extend_from_slice(&[0u8; 16]);
        h.extend_from_slice(b"data\0\0\0\0");
        assert_eq!(wav_data_offset(&h), Some(44));
        assert_eq!(wav_data_offset(b"\0\0\0\0garbage!"), None);
    }

    #[test]
    fn level_decay_and_clip() {
        let mut l = Level::default();
        l.update(&[1.0, 0.1], 0.05);
        assert!(l.clipped);
        assert!((l.db[1] + 20.0).abs() < 0.01);
        l.update(&[0.0, 0.0], 0.1);
        assert!((l.db[1] + 23.0).abs() < 0.01);
        assert!((to_db(0.5) + 6.02).abs() < 0.01);
        l.update(&[0.0, 0.0], 0.5);
        assert!(l.clipped, "a clip stays visible for a while");
        for _ in 0..8 {
            l.update(&[0.0, 0.0], 0.5);
        }
        assert!(!l.clipped);
    }
}
