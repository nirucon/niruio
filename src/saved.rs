//! What WirePlumber remembers between sessions, read from its state files in
//! `~/.local/state/wireplumber`:
//!
//! * `stream-properties` (WirePlumber 0.5) or `restore-stream` (0.4): volume
//!   and mute per media role (`Output/Audio:media.role:Movie`), per program
//!   (`application.name`) and per virtual device.
//! * `default-nodes`: the chosen default output and input.
//! * `default-routes`: per card port volume and mute, and the port of each
//!   profile.
//!
//! Editing these files while WirePlumber runs does not work: it keeps the
//! table in memory and writes it back. A reset therefore goes through
//! WirePlumber itself; see [`crate::actions::reset_saved`].

use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// The key exactly as WirePlumber forms it, unescaped.
    pub key: String,
    /// `Output/Audio`, `Input/Audio`, `Audio/Sink`, `Audio/Source`.
    pub class: String,
    /// `media.role`, `application.name`, `media.name`, `node.name`, ...
    pub property: String,
    pub value: String,
    pub volume: Option<f32>,
    pub volumes: Vec<f32>,
    pub positions: Vec<String>,
    pub mute: Option<bool>,
    pub target: Option<String>,
}

impl Entry {
    /// Loudest channel in the cubic scale people see (1.0 = 100 %).
    pub fn level(&self) -> Option<f32> {
        if !self.volumes.is_empty() {
            Some(
                self.volumes
                    .iter()
                    .map(|v| crate::pw::to_cubic(*v))
                    .fold(0.0, f32::max),
            )
        } else {
            self.volume.map(crate::pw::to_cubic)
        }
    }

    /// Muted or nearly silent: the kind of entry that makes a program quiet
    /// every time it starts.
    pub fn silencing(&self) -> bool {
        self.mute == Some(true) || self.level().is_some_and(|l| l < 0.05)
    }

    pub fn is_stream(&self) -> bool {
        self.class == "Output/Audio" || self.class == "Input/Audio"
    }

    pub fn output(&self) -> bool {
        self.class == "Output/Audio" || self.class == "Audio/Sink"
    }

    /// A human label: "Role Movie (mpv videos)", "Program Firefox", ...
    pub fn label(&self) -> String {
        let what = match self.property.as_str() {
            "media.role" => "Role",
            "application.name" | "application.id" => "Program",
            "media.name" => "Media",
            "node.name" => "Device",
            other => other,
        };
        let hint = if self.property == "media.role" {
            match self.value.as_str() {
                "Movie" => " (video players such as mpv)",
                "Music" => " (music players)",
                "Notification" => " (event sounds)",
                "Game" => " (games)",
                "Communication" | "Phone" => " (calls)",
                _ => "",
            }
        } else {
            ""
        };
        format!("{what} {}{hint}", self.value)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub dir: PathBuf,
    /// Which file the stream entries came from.
    pub stream_file: Option<PathBuf>,
    pub entries: Vec<Entry>,
    pub default_nodes: Vec<(String, String)>,
    pub default_routes: Vec<(String, String)>,
}

pub fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        })
        .join("wireplumber")
}

/// WirePlumber's key escaping (`wp_state`): `\s` space, `\e` `=`, `\o` `[`,
/// `\c` `]`, `\\` backslash.
pub fn unescape(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut chars = key.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('e') => out.push('='),
            Some('o') => out.push('['),
            Some('c') => out.push(']'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// `[section]` + `key=value` lines of a GKeyFile-style state file.
fn keyfile(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('[') && !l.trim_start().starts_with('#'))
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            Some((unescape(k.trim()), v.trim().to_string()))
        })
        .collect()
}

fn split_key(key: &str) -> Option<(String, String, String)> {
    // "Output/Audio:media.role:Movie" -> class, property, value. The value
    // itself may contain ':' (application names rarely do, node names can).
    let (class, rest) = key.split_once(':')?;
    let (property, value) = rest.split_once(':')?;
    Some((class.into(), property.into(), value.into()))
}

/// Parse one value: JSON in WirePlumber 0.5, `volume:0.5;mute:true;...` in
/// 0.4.
pub fn parse_entry(key: &str, value: &str) -> Option<Entry> {
    let (class, property, name) = split_key(key)?;
    let mut entry = Entry {
        key: key.to_string(),
        class,
        property,
        value: name,
        volume: None,
        volumes: Vec::new(),
        positions: Vec::new(),
        mute: None,
        target: None,
    };
    if value.trim_start().starts_with('{') {
        let json: Value = serde_json::from_str(value).ok()?;
        entry.volume = json.get("volume").and_then(Value::as_f64).map(|v| v as f32);
        entry.mute = json.get("mute").and_then(Value::as_bool);
        entry.volumes = json
            .get("channelVolumes")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_f64)
                    .map(|v| v as f32)
                    .collect()
            })
            .unwrap_or_default();
        entry.positions = json
            .get("channelMap")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        entry.target = json.get("target").and_then(Value::as_str).map(String::from);
    } else {
        for part in value.split(';') {
            let Some((k, v)) = part.split_once(':') else {
                continue;
            };
            match k.trim() {
                "volume" => entry.volume = v.trim().parse().ok(),
                "mute" => entry.mute = v.trim().parse().ok(),
                "channelVolumes" => {
                    entry.volumes = v.split(',').filter_map(|x| x.trim().parse().ok()).collect()
                }
                "channelMap" => {
                    entry.positions = v
                        .split(',')
                        .map(|x| x.trim().to_string())
                        .filter(|x| !x.is_empty())
                        .collect()
                }
                "target" | "target.node" => entry.target = Some(v.trim().to_string()),
                _ => {}
            }
        }
    }
    Some(entry)
}

/// The fields WirePlumber 0.4 stores one per line (`KEY:mute=true`).
const FIELDS_04: &[&str] = &[
    "volume",
    "mute",
    "channelVolumes",
    "channelMap",
    "target.node",
    "target.object",
];

/// All stream entries of a state file: one JSON line per key on WirePlumber
/// 0.5, one line per field on 0.4 (`Output/Audio:media.role:Movie:mute=true`,
/// lists as `1.0;1.0;`).
pub fn parse_stream_file(text: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut fields: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for (k, v) in keyfile(text) {
        if v.trim_start().starts_with('{') {
            out.extend(parse_entry(&k, &v));
            continue;
        }
        let split = FIELDS_04.iter().find_map(|f| {
            k.strip_suffix(f)
                .and_then(|base| base.strip_suffix(':'))
                .map(|base| (base.to_string(), f.to_string()))
        });
        match split {
            Some((base, field)) => match fields.iter_mut().find(|(b, _)| *b == base) {
                Some((_, list)) => list.push((field, v)),
                None => fields.push((base, vec![(field, v)])),
            },
            None => out.extend(parse_entry(&k, &v)),
        }
    }
    for (base, list) in fields {
        let joined: Vec<String> = list
            .iter()
            .map(|(f, v)| {
                let f = if f.starts_with("target") { "target" } else { f };
                format!("{f}:{}", v.trim_end_matches(';').replace(';', ","))
            })
            .collect();
        out.extend(parse_entry(&base, &joined.join(";")));
    }
    out
}

pub fn read_dir(dir: &Path) -> State {
    let mut state = State {
        dir: dir.to_path_buf(),
        ..State::default()
    };
    for file in ["stream-properties", "restore-stream"] {
        let path = dir.join(file);
        if let Ok(text) = std::fs::read_to_string(&path) {
            state.stream_file = Some(path);
            state.entries.extend(parse_stream_file(&text));
            break;
        }
    }
    if let Ok(text) = std::fs::read_to_string(dir.join("default-nodes")) {
        state.default_nodes = keyfile(&text);
    }
    if let Ok(text) = std::fs::read_to_string(dir.join("default-routes")) {
        state.default_routes = keyfile(&text);
    }
    // Streams first (they are what silences programs), then by key.
    state
        .entries
        .sort_by(|a, b| b.is_stream().cmp(&a.is_stream()).then(a.key.cmp(&b.key)));
    state
}

pub fn read() -> State {
    read_dir(&state_dir())
}

impl State {
    pub fn find(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }

    pub fn silencing(&self) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|e| e.is_stream() && e.silencing())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WP05: &str = r#"[stream-properties]
Output/Audio:media.role:Movie={"channelVolumes":[1.000000, 1.000000], "channelMap":["FL", "FR"], "volume":1.000000, "mute":true}
Output/Audio:application.name:PipeWire\sALSA\s\ofluidsynth\c={"volume":1.0, "mute":false, "channelVolumes":[0.000100, 0.000100], "channelMap":["FL", "FR"]}
Output/Audio:application.name:Firefox={"volume":1.0, "mute":false, "channelVolumes":[0.5, 0.5], "channelMap":["FL", "FR"], "target":"alsa_output.usb"}
Input/Audio:application.name:REAPER={"volume":1.0, "mute":false, "channelVolumes":[1.0, 1.0], "channelMap":["FL", "FR"]}
Audio/Sink:node.name:auto_null={"volume":1.0, "mute":false, "channelVolumes":[1.0, 1.0], "channelMap":["FL", "FR"]}
"#;

    #[test]
    fn wireplumber_05() {
        let dir = std::env::temp_dir().join(format!("niruio-saved-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stream-properties"), WP05).unwrap();
        std::fs::write(
            dir.join("default-nodes"),
            "[default-nodes]\ndefault.configured.audio.sink=alsa_output.x\n",
        )
        .unwrap();
        let s = read_dir(&dir);
        assert_eq!(s.entries.len(), 5);
        let movie = s.find("Output/Audio:media.role:Movie").unwrap();
        assert_eq!(movie.mute, Some(true));
        assert!(movie.silencing());
        assert!(movie.label().contains("mpv"));
        let fluid = s
            .find("Output/Audio:application.name:PipeWire ALSA [fluidsynth]")
            .unwrap();
        assert!(fluid.silencing(), "volume 0.0001 linear is about 4.6 %");
        let ff = s.find("Output/Audio:application.name:Firefox").unwrap();
        assert!(!ff.silencing());
        assert_eq!(ff.target.as_deref(), Some("alsa_output.usb"));
        assert_eq!(s.silencing().len(), 2);
        assert_eq!(s.default_nodes[0].1, "alsa_output.x");
        // Streams sort before device entries.
        assert!(!s.entries.last().unwrap().is_stream());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wireplumber_04() {
        let e = parse_entry(
            "Output/Audio:media.role:Music",
            "volume:0.000000;mute:false;channelVolumes:0.0,0.0;channelMap:FL,FR;",
        )
        .unwrap();
        assert_eq!(e.volumes, vec![0.0, 0.0]);
        assert_eq!(e.positions, vec!["FL", "FR"]);
        assert!(e.silencing());
    }

    #[test]
    fn wireplumber_04_lines() {
        let e = parse_stream_file(
            "[restore-stream]\n\
             Audio/Sink:node.name:test:mute=false\n\
             Output/Audio:media.role:Movie:channelMap=FL;FR;\n\
             Output/Audio:media.role:Movie:channelVolumes=1.0;1.0;\n\
             Output/Audio:media.role:Movie:mute=true\n\
             Output/Audio:media.role:Movie:volume=1.0\n\
             Output/Audio:application.name:Firefox:target.node=alsa_output.usb\n",
        );
        assert_eq!(e.len(), 3);
        let movie = e
            .iter()
            .find(|x| x.key == "Output/Audio:media.role:Movie")
            .unwrap();
        assert_eq!(movie.mute, Some(true));
        assert_eq!(movie.volumes, vec![1.0, 1.0]);
        assert_eq!(movie.positions, vec!["FL", "FR"]);
        assert!(movie.silencing());
        let ff = e.iter().find(|x| x.value == "Firefox").unwrap();
        assert_eq!(ff.target.as_deref(), Some("alsa_output.usb"));
        assert_eq!(
            e.iter().find(|x| x.class == "Audio/Sink").unwrap().mute,
            Some(false)
        );
    }

    #[test]
    fn escapes() {
        assert_eq!(unescape(r"a\sb\e\oc\c\\"), r"a b=[c]\");
    }
}
