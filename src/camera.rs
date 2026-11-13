//! V4L2 cameras: which exist, which program has each open, formats, controls,
//! and a live preview. Information comes from sysfs, `/proc` and `v4l2-ctl`
//! (`v4l-utils`); the preview from `ffmpeg`. A camera is only opened for
//! streaming while its preview is on.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Camera {
    /// `/dev/video0`
    pub path: String,
    pub name: String,
    pub driver: String,
    pub bus: String,
    /// The stable path niruclip's `camera =` takes (by-id or by-path).
    pub stable: String,
    pub readable: bool,
    pub infrared: bool,
    /// Programs that have the device open: (pid, name).
    pub users: Vec<(u32, String)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CtrlMenu {
    pub value: i64,
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ctrl {
    pub name: String,
    pub kind: String,
    pub min: i64,
    pub max: i64,
    pub step: i64,
    pub default: i64,
    pub value: i64,
    pub inactive: bool,
    pub menu: Vec<CtrlMenu>,
}

impl Ctrl {
    pub fn label(&self) -> String {
        self.name.replace('_', " ")
    }

    pub fn value_text(&self) -> String {
        match self.kind.as_str() {
            "bool" => if self.value != 0 { "on" } else { "off" }.into(),
            "menu" | "intmenu" => self
                .menu
                .iter()
                .find(|m| m.value == self.value)
                .map(|m| m.label.clone())
                .unwrap_or_else(|| self.value.to_string()),
            _ => self.value.to_string(),
        }
    }

    /// The next value one step up or down, inside the limits.
    pub fn stepped(&self, up: bool, big: bool) -> i64 {
        match self.kind.as_str() {
            "bool" => i64::from(self.value == 0),
            "menu" | "intmenu" => {
                let pos = self.menu.iter().position(|m| m.value == self.value);
                let next = match (pos, up) {
                    (Some(p), true) => (p + 1).min(self.menu.len().saturating_sub(1)),
                    (Some(p), false) => p.saturating_sub(1),
                    (None, _) => 0,
                };
                self.menu.get(next).map(|m| m.value).unwrap_or(self.value)
            }
            _ => {
                let step = self.step.max(1);
                let span = (self.max - self.min).max(1);
                let delta = if big {
                    (span / 10).max(step) / step * step
                } else {
                    step.max(span / 100 / step * step)
                };
                let v = if up {
                    self.value + delta
                } else {
                    self.value - delta
                };
                v.clamp(self.min, self.max)
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Format {
    pub fourcc: String,
    pub description: String,
    /// (width, height, best fps)
    pub sizes: Vec<(u32, u32, f32)>,
}

impl Format {
    pub fn summary(&self) -> String {
        let best = self
            .sizes
            .iter()
            .max_by_key(|(w, h, _)| (*w as u64) * (*h as u64));
        match best {
            Some((w, h, fps)) => format!(
                "{} up to {w}x{h} @ {fps:.0} fps ({} sizes)",
                self.fourcc,
                self.sizes.len()
            ),
            None => self.fourcc.clone(),
        }
    }
}

fn root() -> PathBuf {
    PathBuf::from(std::env::var("NIRUIO_ROOT").unwrap_or_default())
}

fn rooted(path: &str) -> PathBuf {
    let r = root();
    if r.as_os_str().is_empty() {
        PathBuf::from(path)
    } else {
        r.join(path.trim_start_matches('/'))
    }
}

/// Every V4L2 node that can capture video. Metadata nodes (the second node
/// a UVC camera creates) are left out.
pub fn list() -> Vec<Camera> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir(rooted("/sys/class/video4linux")) else {
        return out;
    };
    let mut names: Vec<String> = dir
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("video"))
        .collect();
    names.sort_by_key(|n| n[5..].parse::<u32>().unwrap_or(u32::MAX));
    let users = device_users();
    for node in names {
        let sys = rooted("/sys/class/video4linux").join(&node);
        let read = |f: &str| {
            std::fs::read_to_string(sys.join(f))
                .unwrap_or_default()
                .trim()
                .to_string()
        };
        // UVC: index 0 is the picture, index 1 the metadata node.
        let index = read("index");
        if !index.is_empty() && index != "0" {
            continue;
        }
        let path = format!("/dev/{node}");
        let name = read("name");
        let real = rooted(&path);
        // access(2), not open(2): opening a UVC camera wakes it from USB
        // autosuspend, which takes a quarter of a second and can light it.
        let readable = std::ffi::CString::new(real.as_os_str().as_encoded_bytes())
            .map(|c| unsafe { libc::access(c.as_ptr(), libc::R_OK | libc::W_OK) } == 0)
            .unwrap_or(false);
        let lower = name.to_ascii_lowercase();
        out.push(Camera {
            stable: stable_path(&node),
            infrared: lower.contains(" ir ")
                || lower.ends_with(" ir")
                || lower.contains("infrared"),
            users: users
                .iter()
                .filter(|(dev, _, _)| *dev == path)
                .map(|(_, pid, comm)| (*pid, comm.clone()))
                .collect(),
            path,
            name,
            readable,
            ..Camera::default()
        });
    }
    // A camera with two picture nodes (the Logitech BRIO's colour and
    // infrared sensors) keeps both; the infrared one is marked when v4l2-ctl
    // shows only grey formats (see `details`).
    out
}

/// A by-id link that points at this node and no other, else by-path.
fn stable_path(node: &str) -> String {
    for dir in ["/dev/v4l/by-id", "/dev/v4l/by-path"] {
        let Ok(entries) = std::fs::read_dir(rooted(dir)) else {
            continue;
        };
        let mut found: Vec<String> = entries
            .flatten()
            .filter(|e| {
                std::fs::read_link(e.path())
                    .ok()
                    .and_then(|t| t.file_name().map(|f| f.to_string_lossy() == node))
                    .unwrap_or(false)
            })
            .map(|e| format!("{dir}/{}", e.file_name().to_string_lossy()))
            .collect();
        found.sort_by_key(|p| (p.contains("usbv2"), p.clone()));
        if let Some(first) = found.into_iter().next() {
            return first;
        }
    }
    format!("/dev/{node}")
}

/// (device, pid, program) for every `/dev/video*` a readable process holds.
/// Processes of other users are not visible without root.
pub fn device_users() -> Vec<(String, u32, String)> {
    let mut out = Vec::new();
    let Ok(procs) = std::fs::read_dir(rooted("/proc")) else {
        return out;
    };
    let me = std::process::id();
    for p in procs.flatten() {
        let Ok(pid) = p.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if pid == me {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(p.path().join("fd")) else {
            continue;
        };
        let mut seen: Vec<String> = Vec::new();
        for fd in fds.flatten() {
            if let Ok(target) = std::fs::read_link(fd.path()) {
                let t = target.to_string_lossy().into_owned();
                if t.starts_with("/dev/video") && !seen.contains(&t) {
                    seen.push(t);
                }
            }
        }
        if seen.is_empty() {
            continue;
        }
        let comm = std::fs::read_to_string(p.path().join("comm"))
            .unwrap_or_default()
            .trim()
            .to_string();
        for dev in seen {
            out.push((dev, pid, comm.clone()));
        }
    }
    out
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Details {
    pub driver: String,
    pub card: String,
    pub bus: String,
    pub formats: Vec<Format>,
    pub controls: Vec<Ctrl>,
}

impl Details {
    /// Only grey formats: an infrared sensor (face login), not a camera to
    /// film with.
    pub fn infrared(&self) -> bool {
        !self.formats.is_empty()
            && self.formats.iter().all(|f| {
                matches!(
                    f.fourcc.trim(),
                    "GREY" | "Y8" | "Y10" | "Y12" | "Y16" | "Y8I" | "Y10B"
                )
            })
    }
}

pub fn parse_info(text: &str, d: &mut Details) {
    for line in text.lines() {
        let line = line.trim();
        if let Some((k, v)) = line.split_once(':') {
            match k.trim() {
                "Driver name" if d.driver.is_empty() => d.driver = v.trim().into(),
                "Card type" if d.card.is_empty() => d.card = v.trim().into(),
                "Bus info" if d.bus.is_empty() => d.bus = v.trim().into(),
                _ => {}
            }
        }
    }
}

pub fn parse_formats(text: &str) -> Vec<Format> {
    let mut out: Vec<Format> = Vec::new();
    let mut size: Option<(u32, u32)> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.contains("]: '") {
            // [0]: 'MJPG' (Motion-JPEG, compressed)
            let fourcc = line.split('\'').nth(1).unwrap_or_default().to_string();
            let description = line
                .split_once('(')
                .map(|(_, d)| d.trim_end_matches(')').to_string())
                .unwrap_or_default();
            out.push(Format {
                fourcc,
                description,
                sizes: Vec::new(),
            });
            size = None;
        } else if let Some(rest) = line.strip_prefix("Size: Discrete ") {
            size = rest.split_once('x').and_then(|(w, h)| {
                Some((
                    w.trim().parse().ok()?,
                    h.split_whitespace().next()?.parse().ok()?,
                ))
            });
            if let (Some((w, h)), Some(f)) = (size, out.last_mut()) {
                f.sizes.push((w, h, 0.0));
            }
        } else if line.starts_with("Interval:") {
            // Interval: Discrete 0.033s (30.000 fps)
            let fps: f32 = line
                .split('(')
                .nth(1)
                .and_then(|x| x.split_whitespace().next())
                .and_then(|x| x.parse().ok())
                .unwrap_or(0.0);
            if let (Some(_), Some(f)) = (size, out.last_mut()) {
                if let Some(last) = f.sizes.last_mut() {
                    last.2 = last.2.max(fps);
                }
            }
        }
    }
    out
}

pub fn parse_controls(text: &str) -> Vec<Ctrl> {
    let mut out: Vec<Ctrl> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        // Menu entries: "1: 50 Hz" under a menu control.
        if raw.starts_with('\t') || raw.starts_with("\t\t") {
            if let (Some(last), Some((v, label))) = (out.last_mut(), line.split_once(':')) {
                if let Ok(value) = v.trim().parse() {
                    if last.kind == "menu" || last.kind == "intmenu" {
                        last.menu.push(CtrlMenu {
                            value,
                            label: label.trim().to_string(),
                        });
                        continue;
                    }
                }
            }
        }
        // brightness 0x00980900 (int)    : min=-64 max=64 step=1 default=0 value=0
        let Some((head, tail)) = line.split_once(" : ") else {
            continue;
        };
        let mut words = head.split_whitespace();
        let name = words.next().unwrap_or_default().to_string();
        let kind = head
            .split('(')
            .nth(1)
            .and_then(|x| x.split(')').next())
            .unwrap_or_default()
            .to_string();
        if name.is_empty() || kind.is_empty() || kind == "button" || kind.starts_with("ctrl") {
            continue;
        }
        let mut c = Ctrl {
            name,
            kind,
            step: 1,
            ..Ctrl::default()
        };
        for kv in tail.split_whitespace() {
            if let Some((k, v)) = kv.split_once('=') {
                let n = v.parse().unwrap_or(0);
                match k {
                    "min" => c.min = n,
                    "max" => c.max = n,
                    "step" => c.step = n,
                    "default" => c.default = n,
                    "value" => c.value = n,
                    "flags" => c.inactive = v.contains("inactive"),
                    _ => {}
                }
            }
        }
        if c.kind == "bool" {
            c.min = 0;
            c.max = 1;
        }
        out.push(c);
    }
    out
}

/// Driver, formats and controls. Reading them does not start streaming.
pub fn details(path: &str) -> Details {
    let mut d = Details::default();
    if let Ok(text) = crate::actions::run(&["v4l2-ctl", "-d", path, "-D"]) {
        parse_info(&text, &mut d);
    }
    if let Ok(text) = crate::actions::run(&["v4l2-ctl", "-d", path, "--list-formats-ext"]) {
        d.formats = parse_formats(&text);
    }
    if let Ok(text) = crate::actions::run(&["v4l2-ctl", "-d", path, "--list-ctrls-menus"]) {
        d.controls = parse_controls(&text);
    }
    d
}

pub fn set_control(path: &str, name: &str, value: i64) -> Result<(), String> {
    crate::actions::run(&[
        "v4l2-ctl",
        "-d",
        path,
        &format!("--set-ctrl={name}={value}"),
    ])
    .map(drop)
}

/// The niruclip config file.
pub fn niruclip_config() -> PathBuf {
    crate::theme::config_dir().join("niruclip/config.toml")
}

/// Set or replace a top-level `key = "value"` in a TOML text, before the
/// first table, keeping everything else (comments, profiles) as it is.
pub fn set_top_level(text: &str, key: &str, value: Option<&str>) -> String {
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let first_table = lines
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .unwrap_or(lines.len());
    let existing = lines[..first_table].iter().position(|l| {
        l.split_once('=')
            .is_some_and(|(k, _)| k.trim() == key && !l.trim_start().starts_with('#'))
    });
    let escaped = value.map(|v| v.replace('\\', "\\\\").replace('"', "\\\""));
    match (existing, escaped) {
        (Some(i), Some(v)) => lines[i] = format!("{key} = \"{v}\""),
        (Some(i), None) => {
            lines.remove(i);
        }
        (None, Some(v)) => {
            let mut at = first_table;
            while at > 0 && lines[at - 1].trim().is_empty() {
                at -= 1;
            }
            if lines.is_empty() {
                lines.push("schema = 1".into());
                at = 1;
            }
            lines.insert(at, format!("{key} = \"{v}\""));
        }
        (None, None) => {}
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

pub fn niruclip_get(key: &str) -> Option<String> {
    let text = std::fs::read_to_string(niruclip_config()).ok()?;
    crate::theme::toml_top_level(&text, key)
}

pub fn niruclip_set(key: &str, value: Option<&str>) -> Result<PathBuf, String> {
    let path = niruclip_config();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let next = set_top_level(&text, key, value);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("toml.niruio");
    std::fs::write(&tmp, next).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// The newest frame and its sequence number.
type SharedFrame = Arc<Mutex<Option<(u64, Vec<u8>)>>>;

/// A live preview: ffmpeg reads the camera and writes small RGB frames.
pub struct Preview {
    pub path: String,
    pub width: u32,
    pub height: u32,
    child: Child,
    frame: SharedFrame,
    pub error: Arc<Mutex<Option<String>>>,
}

impl Preview {
    /// `NIRUIO_PREVIEW_INPUT` replaces the camera with an ffmpeg input
    /// (for tests: `-f lavfi -i testsrc`).
    pub fn start(path: &str, width: u32, height: u32, fps: u32) -> Result<Self, String> {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
        if let Ok(input) = std::env::var("NIRUIO_PREVIEW_INPUT") {
            cmd.args(input.split_whitespace());
        } else {
            cmd.args(["-f", "v4l2", "-i", path]);
        }
        cmd.args([
            "-vf",
            &format!(
                "fps={fps},scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2"
            ),
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "-",
        ]);
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("ffmpeg: {e}"))?;
        let mut out = child.stdout.take().ok_or("ffmpeg: no output")?;
        let mut errpipe = child.stderr.take();
        let frame: SharedFrame = Arc::new(Mutex::new(None));
        let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let size = (width * height * 3) as usize;
        let f2 = frame.clone();
        let e2 = error.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; size];
            let mut n = 0u64;
            loop {
                if out.read_exact(&mut buf).is_err() {
                    let mut msg = String::new();
                    if let Some(mut e) = errpipe.take() {
                        let _ = e.read_to_string(&mut msg);
                    }
                    let msg = msg
                        .lines()
                        .last()
                        .unwrap_or("the camera stopped")
                        .to_string();
                    if let Ok(mut slot) = e2.lock() {
                        *slot = Some(msg);
                    }
                    return;
                }
                n += 1;
                if let Ok(mut slot) = f2.lock() {
                    *slot = Some((n, buf.clone()));
                }
            }
        });
        Ok(Preview {
            path: path.to_string(),
            width,
            height,
            child,
            frame,
            error,
        })
    }

    /// The newest frame with its sequence number.
    pub fn frame(&self) -> Option<(u64, Vec<u8>)> {
        self.frame.lock().ok()?.clone()
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().ok()?.clone()
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Encode one RGB frame for the kitty graphics protocol, placed at the cursor
/// and scaled to `cols` x `rows` cells. Image id 7341 is NIRUIO's.
pub fn kitty_frame(rgb: &[u8], width: u32, height: u32, cols: u16, rows: u16) -> String {
    const CHUNK: usize = 4096;
    let data = base64(rgb);
    let mut out = String::with_capacity(data.len() + 256);
    let chunks: Vec<&[u8]> = data.as_bytes().chunks(CHUNK).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i + 1 < chunks.len());
        let body = std::str::from_utf8(chunk).unwrap_or_default();
        if i == 0 {
            out.push_str(&format!(
                "\x1b_Ga=T,f=24,s={width},v={height},i=7341,p=1,q=2,C=1,c={cols},r={rows},m={more};{body}\x1b\\"
            ));
        } else {
            out.push_str(&format!("\x1b_Gm={more};{body}\x1b\\"));
        }
    }
    out
}

pub fn kitty_delete() -> &'static str {
    "\x1b_Ga=d,d=I,i=7341,q=2\x1b\\"
}

/// True inside kitty (or a terminal that says it speaks its protocol).
pub fn kitty_graphics() -> bool {
    if std::env::var("NIRUIO_NO_KITTY").is_ok() {
        return false;
    }
    std::env::var_os("KITTY_WINDOW_ID").is_some()
        || std::env::var("TERM").is_ok_and(|t| t.contains("kitty"))
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRLS: &str = "
User Controls

                     brightness 0x00980900 (int)    : min=-64 max=64 step=1 default=0 value=0
           power_line_frequency 0x00980918 (menu)   : min=0 max=2 default=2 value=1 (50 Hz)
				0: Disabled
				1: 50 Hz
				2: 60 Hz

Camera Controls

                  auto_exposure 0x009a0901 (menu)   : min=0 max=3 default=3 value=3 (Aperture Priority Mode)
				1: Manual Mode
				3: Aperture Priority Mode
         exposure_time_absolute 0x009a0902 (int)    : min=1 max=5000 step=1 default=156 value=156 flags=inactive
     focus_automatic_continuous 0x009a090c (bool)   : default=1 value=1
";

    #[test]
    fn controls() {
        let c = parse_controls(CTRLS);
        assert_eq!(c.len(), 5);
        assert_eq!(c[1].menu.len(), 3);
        assert_eq!(c[1].value_text(), "50 Hz");
        assert_eq!(c[1].stepped(true, false), 2);
        assert_eq!(c[2].stepped(false, false), 1);
        assert!(c[3].inactive);
        assert_eq!(c[4].value_text(), "on");
        assert_eq!(c[4].stepped(true, false), 0);
        assert_eq!(c[0].stepped(true, false), 1);
        assert_eq!(c[0].stepped(true, true), 12);
        assert_eq!(c[3].stepped(false, true), 1);
    }

    #[test]
    fn formats() {
        let f = parse_formats(
            "ioctl: VIDIOC_ENUM_FMT
	Type: Video Capture

	[0]: 'YUYV' (YUYV 4:2:2)
		Size: Discrete 640x480
			Interval: Discrete 0.033s (30.000 fps)
			Interval: Discrete 0.067s (15.000 fps)
		Size: Discrete 1280x720
			Interval: Discrete 0.100s (10.000 fps)
	[1]: 'MJPG' (Motion-JPEG, compressed)
		Size: Discrete 1920x1080
			Interval: Discrete 0.017s (60.000 fps)
",
        );
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].sizes, vec![(640, 480, 30.0), (1280, 720, 10.0)]);
        assert_eq!(f[1].summary(), "MJPG up to 1920x1080 @ 60 fps (1 sizes)");
    }

    #[test]
    fn toml_edit_keeps_profiles() {
        let text = "schema = 1\nmic = \"default\"\n\n[profiles.demo]\ncamera = \"/dev/video9\"\n";
        let next = set_top_level(text, "camera", Some("/dev/v4l/by-id/x"));
        assert_eq!(
            next,
            "schema = 1\nmic = \"default\"\ncamera = \"/dev/v4l/by-id/x\"\n\n[profiles.demo]\ncamera = \"/dev/video9\"\n"
        );
        let again = set_top_level(&next, "camera", Some("/dev/video2"));
        assert!(again.contains("camera = \"/dev/video2\"\n\n[profiles"));
        assert_eq!(
            set_top_level("", "mic", Some("UMC")),
            "schema = 1\nmic = \"UMC\"\n"
        );
        let removed = set_top_level(&again, "camera", None);
        assert!(!removed.lines().next().unwrap().contains("camera"));
        assert!(removed.contains("[profiles.demo]\ncamera"));
    }

    #[test]
    fn base64_and_kitty() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
        let k = kitty_frame(&vec![0u8; 3 * 64 * 64], 64, 64, 10, 5);
        assert!(k.starts_with("\x1b_Ga=T,f=24,s=64,v=64,i=7341"));
        assert!(k.contains("m=0;"));
        assert!(k.matches("\x1b_G").count() >= 3);
    }

    #[test]
    fn sysfs_fixture() {
        let dir = std::env::temp_dir().join(format!("niruio-cam-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (node, name, index) in [
            ("video0", "BRIO 4K", "0"),
            ("video1", "BRIO 4K", "1"),
            ("video2", "Integrated IR Camera", "0"),
        ] {
            let d = dir.join("sys/class/video4linux").join(node);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("name"), name).unwrap();
            std::fs::write(d.join("index"), index).unwrap();
        }
        std::fs::create_dir_all(dir.join("proc")).unwrap();
        // NIRUIO_ROOT is only read by this test in this module.
        std::env::set_var("NIRUIO_ROOT", &dir);
        let cams = list();
        std::env::remove_var("NIRUIO_ROOT");
        assert_eq!(cams.len(), 2);
        assert_eq!(cams[0].path, "/dev/video0");
        assert!(!cams[0].readable);
        assert!(cams[1].infrared);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
