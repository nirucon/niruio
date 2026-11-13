//! Changes NIRUIO makes, all through the tools PipeWire and WirePlumber ship
//! (`wpctl`, `pw-cli`, `pw-metadata`, `pw-loopback`) and `amixer`, so the
//! session manager remembers them the way it remembers any other change.
//!
//! `NIRUIO_DRY_RUN=FILE` records every command in FILE instead of running it;
//! the tests use it.

use crate::pw::{self, Kind, Model, Node};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const MAX_VOLUME: f32 = 1.5;

thread_local! {
    static RECORD: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
}

/// Run `f` with every command recorded instead of run (for tests).
#[cfg(test)]
pub fn record<F: FnOnce()>(f: F) -> Vec<String> {
    RECORD.with(|r| *r.borrow_mut() = Some(Vec::new()));
    f();
    RECORD.with(|r| r.borrow_mut().take().unwrap_or_default())
}

fn recording() -> bool {
    RECORD.with(|r| r.borrow().is_some())
}

fn dry_run() -> Option<String> {
    if recording() {
        return Some(String::new());
    }
    std::env::var("NIRUIO_DRY_RUN")
        .ok()
        .filter(|v| !v.is_empty())
}

/// Run a command and return its standard output.
pub fn run(argv: &[&str]) -> Result<String, String> {
    if recording() {
        RECORD.with(|r| {
            if let Some(log) = r.borrow_mut().as_mut() {
                log.push(argv.join(" "));
            }
        });
        return Ok(String::new());
    }
    if let Some(file) = dry_run() {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
            .map_err(|e| e.to_string())?;
        writeln!(f, "{}", argv.join(" ")).map_err(|e| e.to_string())?;
        return Ok(String::new());
    }
    let output = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", argv[0]))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        let err = err.trim();
        Err(if err.is_empty() {
            format!("{} failed ({})", argv[0], output.status)
        } else {
            format!("{}: {}", argv[0], err.lines().last().unwrap_or(err))
        })
    }
}

pub fn have(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

/// The current graph, once. `NIRUIO_PW_DUMP=FILE` reads a fixture instead.
pub fn dump() -> Result<pw::Graph, String> {
    if let Ok(file) = std::env::var("NIRUIO_PW_DUMP") {
        let text = std::fs::read_to_string(&file).map_err(|e| format!("{file}: {e}"))?;
        return pw::Graph::from_json(&text);
    }
    let mut child = Command::new("pw-dump")
        .arg("-N")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("pw-dump: {e} (is PipeWire installed?)"))?;
    // Read on a thread: pw-dump blocks when nobody empties the pipe, and it
    // waits forever when no daemon answers, so give it a few seconds.
    let mut out = child.stdout.take().ok_or("pw-dump: no output")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut text = String::new();
        let _ = out.read_to_string(&mut text);
        let _ = tx.send(text);
    });
    let text = match rx.recv_timeout(Duration::from_secs(4)) {
        Ok(text) => text,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("PipeWire does not answer (pw-dump timed out)".into());
        }
    };
    let _ = child.wait();
    if text.trim().is_empty() {
        return Err("PipeWire is not running (pw-dump found no daemon)".into());
    }
    pw::Graph::from_json(&text)
}

pub fn set_volume(node: &Node, volume: f32) -> Result<(), String> {
    let v = volume.clamp(0.0, MAX_VOLUME);
    run(&[
        "wpctl",
        "set-volume",
        &node.id.to_string(),
        &format!("{v:.3}"),
    ])
    .map(drop)
}

pub fn set_mute(node: &Node, mute: bool) -> Result<(), String> {
    run(&[
        "wpctl",
        "set-mute",
        &node.id.to_string(),
        if mute { "1" } else { "0" },
    ])
    .map(drop)
}

pub fn set_default(node: &Node) -> Result<(), String> {
    run(&["wpctl", "set-default", &node.id.to_string()]).map(drop)
}

fn float_list(values: &[f32]) -> String {
    values
        .iter()
        .map(|v| format!("{v:.6}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Left/right balance for a stereo node: -1.0 left only .. 1.0 right only.
pub fn set_balance(model: &Model, node: &Node, balance: f32) -> Result<(), String> {
    if !node.stereo() {
        return Err("balance needs a stereo device".into());
    }
    let b = balance.clamp(-1.0, 1.0);
    let v = node.volume().max(0.01);
    let left = v * (1.0 - b.max(0.0));
    let right = v * (1.0 + b.min(0.0));
    let linear = [pw::to_linear(left), pw::to_linear(right)];
    if let Some((device, route)) = model.route_of(node) {
        let pod = format!(
            "{{ \"index\": {}, \"device\": {}, \"props\": {{ \"channelVolumes\": [ {} ] }}, \"save\": true }}",
            route.index,
            route.device,
            float_list(&linear)
        );
        run(&["pw-cli", "set-param", &device.id.to_string(), "Route", &pod]).map(drop)
    } else {
        let pod = format!("{{ \"channelVolumes\": [ {} ] }}", float_list(&linear));
        run(&["pw-cli", "set-param", &node.id.to_string(), "Props", &pod]).map(drop)
    }
}

/// Move a program's stream to another output or input. WirePlumber follows
/// the `target.object` metadata and remembers the choice for the program.
pub fn move_stream(stream: &Node, target: &Node) -> Result<(), String> {
    run(&[
        "pw-metadata",
        "-n",
        "default",
        &stream.id.to_string(),
        "target.object",
        &target.serial.to_string(),
        "Spa:Id",
    ])
    .map(drop)
}

pub fn set_profile(device_id: u32, index: u32) -> Result<(), String> {
    let pod = format!("{{ \"index\": {index}, \"save\": true }}");
    run(&[
        "pw-cli",
        "set-param",
        &device_id.to_string(),
        "Profile",
        &pod,
    ])
    .map(drop)
}

pub fn set_route(device_id: u32, index: u32, card_device: i64) -> Result<(), String> {
    let pod = format!("{{ \"index\": {index}, \"device\": {card_device}, \"save\": true }}");
    run(&["pw-cli", "set-param", &device_id.to_string(), "Route", &pod]).map(drop)
}

/// Restart the user's sound services: systemd user units where they exist.
pub fn restart_audio() -> Result<(), String> {
    if have("systemctl") && run(&["systemctl", "--user", "cat", "wireplumber.service"]).is_ok() {
        return run(&[
            "systemctl",
            "--user",
            "restart",
            "pipewire",
            "pipewire-pulse",
            "wireplumber",
        ])
        .map(drop);
    }
    Err("PipeWire is started by the session here; log out and in again".into())
}

fn node_id_by_name(name: &str) -> Option<u32> {
    let text = run(&["pw-dump", "-N"]).ok()?;
    let objects: Vec<serde_json::Value> = serde_json::from_str(&text).ok()?;
    objects.iter().find_map(|o| {
        let props = o.get("info")?.get("props")?;
        if props.get("node.name")?.as_str()? == name {
            o.get("id")?.as_u64().map(|x| x as u32)
        } else {
            None
        }
    })
}

fn wait_for_node(name: &str, timeout: Duration) -> Option<u32> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Some(id) = node_id_by_name(name) {
            return Some(id);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

const RESET_SINK: &str = "niruio-reset-sink";

/// Reset one remembered stream entry (unmute, 100 %) through WirePlumber.
///
/// WirePlumber keeps its table in memory and rewrites the file, so the file
/// cannot be edited. Instead NIRUIO briefly creates a silent stream with the
/// same key (for example `media.role=Movie`), linked only to a private null
/// sink. WirePlumber applies the remembered values to it; NIRUIO then sets
/// 100 % and unmute with `wpctl`, and WirePlumber stores that for the key.
/// Nothing is played: the stream carries no sound and reaches no device.
pub fn reset_saved(entry: &crate::saved::Entry) -> Result<(), String> {
    if !entry.is_stream() {
        return Err("only program and role entries can be reset this way".into());
    }
    if dry_run().is_some() {
        return run(&["niruio-reset", &entry.key]).map(drop);
    }
    for tool in ["pw-cli", "pw-loopback", "wpctl", "pw-dump"] {
        if !have(tool) {
            return Err(format!("{tool} is missing"));
        }
    }
    let channels = if entry.positions.is_empty()
        || entry.positions.iter().any(|p| p == "UNK" || p.is_empty())
    {
        vec!["FL".to_string(), "FR".to_string()]
    } else {
        entry.positions.clone()
    };
    let n = channels.len().to_string();
    let map = channels.join(",");
    let sink_props = format!(
        "{{ factory.name=support.null-audio-sink node.name={RESET_SINK} node.description=\"NIRUIO reset\" \
         media.class=Audio/Sink audio.channels={n} audio.position=[{}] object.linger=true \
         priority.session=1 priority.driver=1 state.restore-props=false node.virtual=true }}",
        channels.join(",")
    );
    // A leftover from an interrupted reset is removed first.
    cleanup_reset_nodes();
    run(&["pw-cli", "create-node", "adapter", &sink_props])?;
    let Some(sink_id) = wait_for_node(RESET_SINK, Duration::from_secs(3)) else {
        cleanup_reset_nodes();
        return Err("could not create the private null sink".into());
    };
    let quote = |v: &str| v.replace('\\', "\\\\").replace('"', "\\\"");
    let key_prop = format!("{}=\"{}\"", entry.property, quote(&entry.value));
    let (playback, capture) = if entry.output() {
        (
            format!(
                "{{ node.name=niruio-reset-probe media.class=Stream/Output/Audio {key_prop} \
                 target.object={RESET_SINK} node.dont-fallback=true node.dont-reconnect=true \
                 state.restore-target=false }}"
            ),
            "{ node.name=niruio-reset-in media.class=Stream/Input/Audio node.autoconnect=false \
             state.restore-props=false state.restore-target=false }"
                .to_string(),
        )
    } else {
        (
            "{ node.name=niruio-reset-out media.class=Stream/Output/Audio node.autoconnect=false \
             state.restore-props=false state.restore-target=false }"
                .to_string(),
            format!(
                "{{ node.name=niruio-reset-probe media.class=Stream/Input/Audio {key_prop} \
                 target.object={RESET_SINK} stream.capture.sink=true node.dont-fallback=true \
                 node.dont-reconnect=true state.restore-target=false }}"
            ),
        )
    };
    let mut child = Command::new("pw-loopback")
        .args(["-n", "niruio-reset", "-c", &n, "-m", &format!("[{map}]")])
        .args(["--capture-props", &capture, "--playback-props", &playback])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("pw-loopback: {e}"))?;
    let result = (|| {
        let probe = wait_for_node("niruio-reset-probe", Duration::from_secs(3))
            .ok_or("the probe stream did not appear")?;
        // Let WirePlumber restore the remembered values first.
        std::thread::sleep(Duration::from_millis(400));
        let id = probe.to_string();
        run(&["wpctl", "set-volume", &id, "1.0"])?;
        run(&["wpctl", "set-mute", &id, "0"])?;
        // WirePlumber writes its state file about a second after a change.
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(250));
            let state = crate::saved::read();
            if let Some(e) = state.find(&entry.key) {
                if !e.silencing() && e.mute != Some(true) {
                    return Ok(());
                }
            }
        }
        Err("WirePlumber did not store the new values".to_string())
    })();
    let _ = child.kill();
    let _ = child.wait();
    let _ = run(&["pw-cli", "destroy", &sink_id.to_string()]);
    cleanup_reset_nodes();
    result
}

/// The same, on a thread, so the interface starts at once.
pub fn cleanup_reset_nodes_later() {
    if dry_run().is_some() || std::env::var("NIRUIO_PW_DUMP").is_ok() {
        return;
    }
    std::thread::spawn(cleanup_reset_nodes);
}

/// Remove a private null sink a crashed reset left behind.
pub fn cleanup_reset_nodes() {
    if dry_run().is_some() {
        return;
    }
    while let Some(id) = node_id_by_name(RESET_SINK) {
        if run(&["pw-cli", "destroy", &id.to_string()]).is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `amixer -c CARD sset CONTROL VALUE`.
pub fn amixer_set(card: u32, control: &str, value: &str) -> Result<(), String> {
    run(&[
        "amixer",
        "-q",
        "-c",
        &card.to_string(),
        "sset",
        control,
        value,
    ])
    .map(drop)
}

/// Streams that could move to `target`: same direction.
pub fn move_targets<'a>(model: &'a Model, stream: &Node) -> Vec<&'a Node> {
    match stream.kind {
        Kind::Playback => model.sinks(),
        Kind::Recording => model.sources(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_recorded() {
        let graph = pw::Graph::from_json(include_str!("../tests/fixtures/laptop.json")).unwrap();
        let m = graph.model();
        let speaker = m.default_sink_node().unwrap().clone();
        let mpv = m
            .streams()
            .into_iter()
            .find(|n| n.app_name == "mpv")
            .unwrap()
            .clone();
        let hdmi = m
            .sinks()
            .into_iter()
            .find(|n| n.connection() == "HDMI")
            .unwrap()
            .clone();
        let lines = record(|| {
            set_volume(&speaker, 2.0).unwrap();
            set_mute(&speaker, true).unwrap();
            set_default(&speaker).unwrap();
            set_balance(&m, &speaker, -0.5).unwrap();
            move_stream(&mpv, &hdmi).unwrap();
            set_profile(52, 2).unwrap();
            set_route(50, 3, 1).unwrap();
        });
        assert_eq!(lines[0], "wpctl set-volume 60 1.500");
        assert_eq!(lines[1], "wpctl set-mute 60 1");
        assert_eq!(lines[2], "wpctl set-default 60");
        assert!(
            lines[3].starts_with("pw-cli set-param 50 Route { \"index\": 2, \"device\": 1"),
            "{}",
            lines[3]
        );
        assert_eq!(
            lines[4],
            "pw-metadata -n default 70 target.object 61 Spa:Id"
        );
        assert_eq!(
            lines[5],
            "pw-cli set-param 52 Profile { \"index\": 2, \"save\": true }"
        );
        assert_eq!(
            lines[6],
            "pw-cli set-param 50 Route { \"index\": 3, \"device\": 1, \"save\": true }"
        );
    }
}
