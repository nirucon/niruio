//! The sound doctor: why is nothing heard, why is nothing recorded, why is the
//! camera missing. It walks the chain the way sound travels (program stream,
//! remembered role or program volume, output/input device, card profile and
//! port, ALSA switches, device presence) and offers one fix per finding.

use crate::alsa;
use crate::camera::Camera;
use crate::pw::{Kind, Model, Node};
use crate::saved::State;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Problem,
    Warning,
    Info,
    Ok,
}

impl Severity {
    pub fn word(self) -> &'static str {
        match self {
            Severity::Problem => "problem",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Ok => "ok",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Fix {
    Unmute {
        node: u32,
    },
    Volume {
        node: u32,
        volume: f32,
    },
    ResetSaved {
        key: String,
    },
    Profile {
        device: u32,
        index: u32,
    },
    Route {
        device: u32,
        index: u32,
        card_device: i64,
    },
    Alsa {
        card: u32,
        control: String,
        value: String,
    },
    Default {
        node: u32,
    },
    RestartAudio,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Finding {
    pub severity: Severity,
    /// `output`, `input`, `apps`, `saved`, `cards`, `camera`, `system`.
    pub area: &'static str,
    pub id: String,
    pub title: String,
    pub detail: String,
    pub fix: Option<Fix>,
    pub fix_text: Option<String>,
}

pub struct Input<'a> {
    pub model: Option<&'a Model>,
    pub error: Option<String>,
    pub saved: &'a State,
    pub cameras: &'a [Camera],
    /// ALSA controls of a card; injected so tests need no amixer.
    pub alsa: &'a dyn Fn(u32) -> Vec<alsa::Control>,
    pub camera_dev_present: bool,
}

fn pct(v: f32) -> String {
    format!("{:.0} %", v * 100.0)
}

fn finding(
    severity: Severity,
    area: &'static str,
    id: impl Into<String>,
    title: impl Into<String>,
    detail: impl Into<String>,
) -> Finding {
    Finding {
        severity,
        area,
        id: id.into(),
        title: title.into(),
        detail: detail.into(),
        fix: None,
        fix_text: None,
    }
}

impl Finding {
    fn with_fix(mut self, fix: Fix, text: impl Into<String>) -> Self {
        self.fix = Some(fix);
        self.fix_text = Some(text.into());
        self
    }
}

/// The best profile of a card that gives outputs (or inputs): available and
/// highest priority, never "off" and never a fallback when a proper one exists.
fn best_profile(device: &crate::pw::Device, want_output: bool) -> Option<&crate::pw::Profile> {
    device
        .profiles
        .iter()
        .filter(|p| p.name != "off" && p.available != "no")
        .filter(|p| {
            if want_output {
                p.sinks > 0
            } else {
                p.sources > 0
            }
        })
        .max_by_key(|p| (!p.name.contains("fallback"), p.priority))
}

fn check_device(
    out: &mut Vec<Finding>,
    model: &Model,
    node: &Node,
    input: &Input,
    area: &'static str,
) {
    let what = if node.kind == Kind::Sink {
        "output"
    } else {
        "input"
    };
    let name = &node.description;
    if node.mute {
        out.push(
            finding(
                Severity::Problem,
                area,
                format!("{what}-muted"),
                format!("The default {what} is muted"),
                format!("{name} is muted in PipeWire."),
            )
            .with_fix(Fix::Unmute { node: node.id }, format!("unmute {name}")),
        );
    } else if node.volume() < 0.05 {
        out.push(
            finding(
                Severity::Problem,
                area,
                format!("{what}-silent"),
                format!("The default {what} volume is {}", pct(node.volume())),
                format!("{name} is turned down to {}.", pct(node.volume())),
            )
            .with_fix(
                Fix::Volume {
                    node: node.id,
                    volume: 0.5,
                },
                format!("set {name} to 50 %"),
            ),
        );
    } else if node.volume() < 0.15 {
        out.push(finding(
            Severity::Info,
            area,
            format!("{what}-low"),
            format!("The default {what} volume is low ({})", pct(node.volume())),
            format!("{name} is at {}.", pct(node.volume())),
        ));
    }
    // Card profile and port.
    if let Some(device) = node.device_id.and_then(|id| model.device(id)) {
        if let Some(profile) = device.active_profile() {
            if profile.name.contains("fallback") {
                let mut f = finding(
                    Severity::Warning,
                    "cards",
                    format!("{what}-fallback-profile"),
                    format!("{} uses a fallback profile", device.description),
                    format!(
                        "The card is in \"{}\" ({}): PipeWire could not match its proper \
                         profile, so ports may be missing or wrong.",
                        profile.description, profile.name
                    ),
                );
                if let Some(best) = best_profile(device, node.kind == Kind::Sink)
                    .filter(|b| b.index != profile.index && !b.name.contains("fallback"))
                {
                    f = f.with_fix(
                        Fix::Profile {
                            device: device.id,
                            index: best.index,
                        },
                        format!("switch {} to \"{}\"", device.description, best.description),
                    );
                }
                out.push(f);
            }
            if device.bluetooth() && node.kind == Kind::Sink && profile.sources > 0 {
                let mut f = finding(
                    Severity::Info,
                    "cards",
                    "bluetooth-headset-profile",
                    format!("{} is in headset mode", device.description),
                    "The hands-free profile has a microphone but much lower sound quality. \
                     Switch to hi-fi (A2DP) when you are not in a call.",
                );
                if let Some(a2dp) = device
                    .profiles
                    .iter()
                    .filter(|p| p.name.starts_with("a2dp") && p.available != "no")
                    .max_by_key(|p| p.priority)
                {
                    f = f.with_fix(
                        Fix::Profile {
                            device: device.id,
                            index: a2dp.index,
                        },
                        format!("switch {} to hi-fi playback", device.description),
                    );
                }
                out.push(f);
            }
        }
        if let Some(card_device) = node.profile_device {
            if let Some(route) = device.route_for(card_device) {
                if route.available == "no" {
                    let mut f = finding(
                        Severity::Problem,
                        area,
                        format!("{what}-port-unavailable"),
                        format!("The {what} port \"{}\" is not connected", route.description),
                        format!(
                            "{name} sends sound to \"{}\", which reports nothing plugged in.",
                            route.description
                        ),
                    );
                    if let Some(alt) = device
                        .routes_for(card_device)
                        .into_iter()
                        .filter(|r| r.available != "no" && r.index != route.index)
                        .max_by_key(|r| r.priority)
                    {
                        f = f.with_fix(
                            Fix::Route {
                                device: device.id,
                                index: alt.index,
                                card_device,
                            },
                            format!("use the port \"{}\"", alt.description),
                        );
                    }
                    out.push(f);
                }
            }
        }
        // ALSA switches that silence the card behind PipeWire's back.
        if let (Some(card), false) = (device.alsa_card, node.mute) {
            let controls = (input.alsa)(card);
            for c in &controls {
                let off_playback =
                    node.kind == Kind::Sink && c.name == "Master" && c.playback_on == Some(false);
                let off_capture = node.kind == Kind::Source
                    && (c.name == "Capture" || c.name == "Mic")
                    && c.capture_on == Some(false);
                if off_playback || off_capture {
                    let value = if off_capture { "cap" } else { "on" };
                    out.push(
                        finding(
                            Severity::Problem,
                            area,
                            format!("alsa-{}-off", c.name.to_lowercase()),
                            format!("The ALSA switch \"{}\" is off", c.name),
                            format!(
                                "PipeWire shows {name} unmuted, but the card's own \"{}\" \
                                 switch is off.",
                                c.name
                            ),
                        )
                        .with_fix(
                            Fix::Alsa {
                                card,
                                control: c.id(),
                                value: value.into(),
                            },
                            format!("turn \"{}\" on (amixer)", c.name),
                        ),
                    );
                }
            }
        }
    }
}

fn check_devices_present(out: &mut Vec<Finding>, model: &Model, want_output: bool) -> bool {
    let present = if want_output {
        model.sinks().iter().any(|n| n.name != "auto_null")
    } else {
        !model.sources().is_empty()
    };
    if present {
        return true;
    }
    let what = if want_output { "output" } else { "input" };
    // A desktop without a microphone is normal; a machine without output is not.
    let mut f = finding(
        if want_output {
            Severity::Problem
        } else {
            Severity::Warning
        },
        if want_output { "output" } else { "input" },
        format!("no-{what}"),
        format!("No sound {what} device"),
        if want_output && model.only_dummy_output() {
            "PipeWire only offers \"Dummy Output\": no sound card is active.".to_string()
        } else {
            format!("PipeWire has no {what} device.")
        },
    );
    // A card switched off is the common, fixable cause.
    if let Some((device, best)) = model.devices.iter().find_map(|d| {
        let off = d.active_profile().is_none_or(|p| p.name == "off");
        if off {
            best_profile(d, want_output).map(|b| (d, b))
        } else {
            None
        }
    }) {
        f.detail
            .push_str(&format!(" {} is switched off.", device.description));
        f = f.with_fix(
            Fix::Profile {
                device: device.id,
                index: best.index,
            },
            format!("switch {} to \"{}\"", device.description, best.description),
        );
    } else if model.devices.is_empty() && want_output {
        f.detail.push_str(" No sound card was found at all; check that the device is connected, or restart the sound services.");
        f = f.with_fix(Fix::RestartAudio, "restart PipeWire and WirePlumber");
    }
    out.push(f);
    false
}

pub fn run(input: &Input) -> Vec<Finding> {
    let mut out = Vec::new();
    let Some(model) = input.model else {
        out.push(
            finding(
                Severity::Problem,
                "system",
                "pipewire-down",
                "PipeWire is not running",
                input
                    .error
                    .clone()
                    .unwrap_or_else(|| "pw-dump could not reach PipeWire.".into()),
            )
            .with_fix(Fix::RestartAudio, "restart PipeWire and WirePlumber"),
        );
        check_cameras(&mut out, input);
        return out;
    };
    if !model.session_manager {
        out.push(
            finding(
                Severity::Problem,
                "system",
                "wireplumber-down",
                "WirePlumber is not running",
                "Without the session manager nothing is connected and no device is chosen.",
            )
            .with_fix(Fix::RestartAudio, "restart PipeWire and WirePlumber"),
        );
    }

    // Why is nothing heard?
    if check_devices_present(&mut out, model, true) {
        match model.default_sink_node() {
            Some(sink) => check_device(&mut out, model, sink, input, "output"),
            None => {
                let mut f = finding(
                    Severity::Problem,
                    "output",
                    "no-default-output",
                    "No default output",
                    "WirePlumber has not chosen an output.",
                );
                if let Some(best) = model.sinks().into_iter().find(|n| n.name != "auto_null") {
                    f = f.with_fix(
                        Fix::Default { node: best.id },
                        format!("use {} as the default output", best.description),
                    );
                }
                out.push(f);
            }
        }
    }
    // Programs: muted streams and the remembered value behind them.
    for stream in model.streams() {
        let program = stream.program();
        let dir = if stream.kind == Kind::Playback {
            "plays"
        } else {
            "records"
        };
        let key = stream.restore_key(None);
        let remembered = key.as_deref().and_then(|k| input.saved.find(k));
        if stream.mute || stream.volume() < 0.05 {
            let why = match remembered {
                Some(e) if e.silencing() => format!(
                    " WirePlumber remembers \"{}\" as {} and applies it every time.",
                    e.label(),
                    if e.mute == Some(true) {
                        "muted".to_string()
                    } else {
                        pct(e.level().unwrap_or(0.0))
                    }
                ),
                _ => String::new(),
            };
            let state = if stream.mute {
                "muted".to_string()
            } else {
                format!("at {}", pct(stream.volume()))
            };
            let mut f = finding(
                Severity::Problem,
                "apps",
                format!("stream-{}", stream.id),
                format!("{program} {dir} {state}"),
                format!(
                    "{program} ({}) is {state}.{why}",
                    if stream.media_name.is_empty() {
                        stream.description.clone()
                    } else {
                        stream.media_name.clone()
                    }
                ),
            );
            f = match remembered.filter(|e| e.silencing()) {
                Some(e) => f.with_fix(
                    Fix::ResetSaved { key: e.key.clone() },
                    format!("reset the remembered \"{}\" to 100 % unmuted", e.label()),
                ),
                None if stream.mute => {
                    f.with_fix(Fix::Unmute { node: stream.id }, format!("unmute {program}"))
                }
                None => f.with_fix(
                    Fix::Volume {
                        node: stream.id,
                        volume: 1.0,
                    },
                    format!("set {program} to 100 %"),
                ),
            };
            out.push(f);
        } else if stream.kind == Kind::Playback
            && model.stream_targets(stream).is_empty()
            && stream.state != "suspended"
        {
            out.push(finding(
                Severity::Info,
                "apps",
                format!("stream-unlinked-{}", stream.id),
                format!("{program} is not connected to an output"),
                "The stream exists but is not linked to any device.",
            ));
        }
    }
    // Remembered entries that will silence programs the next time they start.
    let live: Vec<String> = model
        .streams()
        .iter()
        .filter_map(|s| s.restore_key(None))
        .collect();
    for e in input.saved.silencing() {
        if live.contains(&e.key) {
            continue;
        }
        let state = if e.mute == Some(true) {
            "muted".to_string()
        } else {
            pct(e.level().unwrap_or(0.0))
        };
        let severity = if e.property == "media.role" {
            Severity::Warning
        } else {
            Severity::Info
        };
        out.push(
            finding(
                severity,
                "saved",
                format!("saved-{}", e.key),
                format!("Remembered: {} is {state}", e.label()),
                format!(
                    "WirePlumber applies this to every {} that starts{}. It is stored in {}.",
                    if e.output() { "player" } else { "recorder" },
                    if e.property == "media.role" && e.value == "Movie" {
                        " (mpv tags the sound of every video as Movie)"
                    } else {
                        ""
                    },
                    input
                        .saved
                        .stream_file
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "the WirePlumber state".into())
                ),
            )
            .with_fix(
                Fix::ResetSaved { key: e.key.clone() },
                format!("reset \"{}\" to 100 % unmuted", e.label()),
            ),
        );
    }

    // Why is nothing recorded?
    if check_devices_present(&mut out, model, false) {
        match model.default_source_node() {
            Some(source) => check_device(&mut out, model, source, input, "input"),
            None => {
                let mut f = finding(
                    Severity::Warning,
                    "input",
                    "no-default-input",
                    "No default input",
                    "WirePlumber has not chosen a microphone.",
                );
                if let Some(best) = model.sources().first() {
                    f = f.with_fix(
                        Fix::Default { node: best.id },
                        format!("use {} as the default input", best.description),
                    );
                }
                out.push(f);
            }
        }
    }
    check_cameras(&mut out, input);
    if out.iter().all(|f| f.severity >= Severity::Info) {
        let sink = model
            .default_sink_node()
            .map(|n| format!("{} at {}", n.description, pct(n.volume())))
            .unwrap_or_default();
        out.insert(
            0,
            finding(
                Severity::Ok,
                "system",
                "ok",
                "Sound looks fine",
                format!("Default output: {sink}."),
            ),
        );
    }
    out.sort_by_key(|f| f.severity);
    out
}

fn check_cameras(out: &mut Vec<Finding>, input: &Input) {
    if input.cameras.is_empty() {
        out.push(finding(
            Severity::Info,
            "camera",
            "no-camera",
            "No camera found",
            if input.camera_dev_present {
                "Video devices exist but none can capture pictures."
            } else {
                "There is no /dev/video device: no camera is connected, it is switched off \
                 (a privacy switch or Fn key), or its driver is not loaded."
            },
        ));
        return;
    }
    for cam in input.cameras {
        if !cam.readable {
            out.push(finding(
                Severity::Problem,
                "camera",
                format!("camera-access-{}", cam.path),
                format!("No access to {}", cam.name),
                format!(
                    "{} cannot be opened by you. On NIRUX your user must be in the \
                     \"video\" group: sudo usermod -aG video $USER, then log in again.",
                    cam.path
                ),
            ));
        }
        if !cam.users.is_empty() {
            let who: Vec<String> = cam
                .users
                .iter()
                .map(|(pid, name)| format!("{name} ({pid})"))
                .collect();
            out.push(finding(
                Severity::Info,
                "camera",
                format!("camera-busy-{}", cam.path),
                format!("{} is in use", cam.name),
                format!(
                    "Held open by {}. Most cameras serve one program at a time.",
                    who.join(", ")
                ),
            ));
        }
    }
}

/// Apply one fix. Returns a short confirmation.
pub fn apply(fix: &Fix, model: Option<&Model>, saved: &State) -> Result<String, String> {
    use crate::actions;
    let node = |id: u32| -> Result<Node, String> {
        model
            .and_then(|m| m.node(id))
            .cloned()
            .ok_or_else(|| "the device is gone".to_string())
    };
    match fix {
        Fix::Unmute { node: id } => {
            let n = node(*id)?;
            actions::set_mute(&n, false)?;
            Ok(format!("unmuted {}", n.description))
        }
        Fix::Volume { node: id, volume } => {
            let n = node(*id)?;
            actions::set_volume(&n, *volume)?;
            Ok(format!("{} set to {}", n.description, pct(*volume)))
        }
        Fix::Default { node: id } => {
            let n = node(*id)?;
            actions::set_default(&n)?;
            Ok(format!("{} is the default", n.description))
        }
        Fix::ResetSaved { key } => {
            let entry = saved
                .find(key)
                .cloned()
                .or_else(|| crate::saved::parse_entry(key, "{}"))
                .ok_or("unknown entry")?;
            actions::reset_saved(&entry)?;
            Ok(format!("reset {}", entry.label()))
        }
        Fix::Profile { device, index } => {
            actions::set_profile(*device, *index)?;
            Ok("profile switched".into())
        }
        Fix::Route {
            device,
            index,
            card_device,
        } => {
            actions::set_route(*device, *index, *card_device)?;
            Ok("port switched".into())
        }
        Fix::Alsa {
            card,
            control,
            value,
        } => {
            actions::amixer_set(*card, control, value)?;
            Ok(format!("{control} turned on"))
        }
        Fix::RestartAudio => {
            actions::restart_audio()?;
            Ok("sound services restarted".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pw::Graph;

    fn model() -> Model {
        Graph::from_json(include_str!("../tests/fixtures/laptop.json"))
            .unwrap()
            .model()
    }

    fn saved() -> State {
        let mut s = State::default();
        s.entries.push(
            crate::saved::parse_entry(
                "Output/Audio:media.role:Movie",
                r#"{"volume":1.0,"mute":true,"channelVolumes":[1.0,1.0],"channelMap":["FL","FR"]}"#,
            )
            .unwrap(),
        );
        s.entries.push(
            crate::saved::parse_entry(
                "Output/Audio:media.role:Music",
                r#"{"volume":1.0,"mute":false,"channelVolumes":[0.0,0.0],"channelMap":["FL","FR"]}"#,
            )
            .unwrap(),
        );
        s
    }

    fn no_alsa(_: u32) -> Vec<alsa::Control> {
        Vec::new()
    }

    #[test]
    fn mpv_movie_role_is_found_with_reset() {
        let m = model();
        let s = saved();
        let f = run(&Input {
            model: Some(&m),
            error: None,
            saved: &s,
            cameras: &[],
            alsa: &no_alsa,
            camera_dev_present: false,
        });
        let mpv = f.iter().find(|f| f.id == "stream-70").unwrap();
        assert_eq!(mpv.severity, Severity::Problem);
        assert!(mpv.detail.contains("Role Movie"), "{}", mpv.detail);
        assert_eq!(
            mpv.fix,
            Some(Fix::ResetSaved {
                key: "Output/Audio:media.role:Movie".into()
            })
        );
        // Music is not playing now but will be silent when it does.
        let music = f
            .iter()
            .find(|f| f.id == "saved-Output/Audio:media.role:Music")
            .unwrap();
        assert_eq!(music.severity, Severity::Warning);
        // Movie is reported once, with the live stream.
        assert!(!f
            .iter()
            .any(|f| f.id == "saved-Output/Audio:media.role:Movie"));
        assert!(f.iter().any(|f| f.id == "no-camera"));
        assert!(f.windows(2).all(|w| w[0].severity <= w[1].severity));
    }

    #[test]
    fn muted_default_and_alsa_switch() {
        let mut m = model();
        let sink = m
            .nodes
            .iter_mut()
            .find(|n| n.name == "alsa_output.pci-0000_c4_00.6.analog-stereo")
            .unwrap();
        sink.mute = true;
        let s = State::default();
        let alsa = |_card: u32| {
            alsa::parse(
                "Simple mixer control 'Capture',0\n  Capabilities: cvolume cswitch\n  Limits: Capture 0 - 63\n  Front Left: Capture 39 [62%] [12.00dB] [off]\n",
            )
        };
        let f = run(&Input {
            model: Some(&m),
            error: None,
            saved: &s,
            cameras: &[],
            alsa: &alsa,
            camera_dev_present: false,
        });
        let muted = f.iter().find(|f| f.id == "output-muted").unwrap();
        assert_eq!(muted.fix, Some(Fix::Unmute { node: 60 }));
        let cap = f.iter().find(|f| f.id == "alsa-capture-off").unwrap();
        assert_eq!(
            cap.fix,
            Some(Fix::Alsa {
                card: 1,
                control: "Capture".into(),
                value: "cap".into()
            })
        );
    }

    #[test]
    fn port_unplugged_and_card_off() {
        let mut m = model();
        let card = m.devices.iter_mut().find(|d| d.id == 50).unwrap();
        card.active_routes[0].available = "no".into();
        let s = State::default();
        let f = run(&Input {
            model: Some(&m),
            error: None,
            saved: &s,
            cameras: &[],
            alsa: &no_alsa,
            camera_dev_present: false,
        });
        let port = f
            .iter()
            .find(|f| f.id == "output-port-unavailable")
            .unwrap();
        // Headphones are "no" too; nothing better than the speaker: no fix.
        assert!(port.fix.is_none());

        let mut m = model();
        m.nodes.retain(|n| n.kind != Kind::Sink);
        m.devices.iter_mut().find(|d| d.id == 50).unwrap().profile = Some(0);
        let f = run(&Input {
            model: Some(&m),
            error: None,
            saved: &s,
            cameras: &[],
            alsa: &no_alsa,
            camera_dev_present: false,
        });
        let none = f.iter().find(|f| f.id == "no-output").unwrap();
        assert_eq!(
            none.fix,
            Some(Fix::Profile {
                device: 50,
                index: 1
            })
        );
    }

    #[test]
    fn pipewire_down() {
        let s = State::default();
        let f = run(&Input {
            model: None,
            error: Some("timeout".into()),
            saved: &s,
            cameras: &[],
            alsa: &no_alsa,
            camera_dev_present: true,
        });
        assert_eq!(f[0].id, "pipewire-down");
        assert_eq!(f[0].fix, Some(Fix::RestartAudio));
    }

    #[test]
    fn camera_findings() {
        let m = model();
        let s = State::default();
        let cams = vec![Camera {
            path: "/dev/video0".into(),
            name: "Cam".into(),
            readable: false,
            users: vec![(12, "zoom".into())],
            ..Camera::default()
        }];
        let f = run(&Input {
            model: Some(&m),
            error: None,
            saved: &s,
            cameras: &cams,
            alsa: &no_alsa,
            camera_dev_present: true,
        });
        assert!(f.iter().any(|f| f.id == "camera-access-/dev/video0"));
        assert!(f
            .iter()
            .any(|f| f.id == "camera-busy-/dev/video0" && f.detail.contains("zoom (12)")));
    }
}
