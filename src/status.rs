//! The command-line side: `niruio status`, `doctor`, `saved`, `reset`.

use crate::doctor::{self, Finding, Severity};
use crate::pw::{Model, Node};
use crate::{actions, alsa, camera, saved};
use serde_json::{json, Value};
use std::process::ExitCode;

pub fn gather() -> (Option<Model>, Option<String>) {
    match actions::dump() {
        Ok(graph) if !graph.is_empty() => (Some(graph.model()), None),
        Ok(_) => (None, Some("PipeWire returned nothing".into())),
        Err(e) => (None, Some(e)),
    }
}

fn camera_dev_present() -> bool {
    std::fs::read_dir("/dev")
        .map(|d| {
            d.flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("video"))
        })
        .unwrap_or(false)
}

pub fn findings(model: Option<&Model>, error: Option<String>) -> Vec<Finding> {
    let state = saved::read();
    let cameras = camera::list();
    let amixer = actions::have("amixer");
    let lookup = move |card: u32| {
        if amixer {
            alsa::controls(card)
        } else {
            Vec::new()
        }
    };
    doctor::run(&doctor::Input {
        model,
        error,
        saved: &state,
        cameras: &cameras,
        alsa: &lookup,
        camera_dev_present: camera_dev_present(),
    })
}

fn node_json(model: &Model, n: &Node) -> Value {
    json!({
        "id": n.id,
        "name": n.name,
        "description": n.description,
        "connection": n.connection(),
        "volume": (n.volume() * 100.0).round() / 100.0,
        "mute": n.mute,
        "channels": n.positions.len(),
        "default": model.is_default(n),
        "state": n.state,
    })
}

fn stream_json(model: &Model, n: &Node) -> Value {
    json!({
        "id": n.id,
        "program": n.program(),
        "media": n.media_name,
        "role": n.media_role,
        "direction": if n.kind == crate::pw::Kind::Playback { "playback" } else { "recording" },
        "volume": (n.volume() * 100.0).round() / 100.0,
        "mute": n.mute,
        "device": model.stream_targets(n).first().map(|t| t.description.clone()),
        "pid": n.pid,
    })
}

fn pct(v: f32) -> String {
    format!("{:.0}%", v * 100.0)
}

pub fn status(as_json: bool) -> ExitCode {
    let (model, error) = gather();
    let found = findings(model.as_ref(), error.clone());
    let problems = found
        .iter()
        .filter(|f| f.severity == Severity::Problem)
        .count();
    let warnings = found
        .iter()
        .filter(|f| f.severity == Severity::Warning)
        .count();
    let cameras = camera::list();
    let state = saved::read();
    if as_json {
        let m = model.clone().unwrap_or_default();
        let out = json!({
            "version": crate::VERSION,
            "pipewire": model.as_ref().map(|m| m.version.clone()),
            "error": error,
            "default_output": m.default_sink_node().map(|n| node_json(&m, n)),
            "default_input": m.default_source_node().map(|n| node_json(&m, n)),
            "outputs": m.sinks().iter().map(|n| node_json(&m, n)).collect::<Vec<_>>(),
            "inputs": m.sources().iter().map(|n| node_json(&m, n)).collect::<Vec<_>>(),
            "apps": m.streams().iter().map(|n| stream_json(&m, n)).collect::<Vec<_>>(),
            "cameras": cameras.iter().map(|c| json!({
                "path": c.path, "name": c.name, "stable": c.stable,
                "readable": c.readable, "users": c.users.iter().map(|(p, n)| json!({"pid": p, "name": n})).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "saved_silencing": state.silencing().iter().map(|e| e.key.clone()).collect::<Vec<_>>(),
            "problems": problems,
            "warnings": warnings,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return ExitCode::SUCCESS;
    }
    let Some(m) = model else {
        println!("PipeWire: {}", error.unwrap_or_default());
        return ExitCode::FAILURE;
    };
    let line = |label: &str, n: Option<&Node>| match n {
        Some(n) => println!(
            "{label:<8}{}  {}{}",
            n.description,
            pct(n.volume()),
            if n.mute { "  muted" } else { "" }
        ),
        None => println!("{label:<8}none"),
    };
    line("Output", m.default_sink_node());
    line("Input", m.default_source_node());
    for s in m.streams() {
        let dir = if s.kind == crate::pw::Kind::Playback {
            "plays"
        } else {
            "records"
        };
        println!(
            "App     {} {dir}{}  {}{}",
            s.program(),
            m.stream_targets(s)
                .first()
                .map(|t| format!(" on {}", t.description))
                .unwrap_or_default(),
            pct(s.volume()),
            if s.mute { "  muted" } else { "" }
        );
    }
    for c in &cameras {
        println!(
            "Camera  {} ({}){}",
            c.name,
            c.path,
            if c.users.is_empty() {
                String::new()
            } else {
                format!(
                    "  in use by {}",
                    c.users
                        .iter()
                        .map(|(_, n)| n.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        );
    }
    println!(
        "Doctor  {problems} problem(s), {warnings} warning(s){}",
        if problems + warnings > 0 {
            ": run niruio doctor"
        } else {
            ""
        }
    );
    ExitCode::SUCCESS
}

fn print_findings(found: &[Finding]) {
    for f in found {
        println!("{:<8} {}", f.severity.word(), f.title);
        println!("         {}", f.detail);
        if let Some(fix) = &f.fix_text {
            println!("         fix: {fix}");
        }
    }
}

pub fn doctor(as_json: bool, fix: bool, brief: bool) -> ExitCode {
    let (model, error) = gather();
    let mut found = findings(model.as_ref(), error.clone());
    if fix {
        let state = saved::read();
        let mut applied = Vec::new();
        for f in found
            .iter()
            .filter(|f| f.severity <= Severity::Warning && f.fix.is_some())
        {
            let fx = f.fix.as_ref().expect("filtered");
            match doctor::apply(fx, model.as_ref(), &state) {
                Ok(msg) => applied.push(json!({"id": f.id, "ok": true, "message": msg})),
                Err(e) => applied.push(json!({"id": f.id, "ok": false, "message": e})),
            }
        }
        if !as_json {
            if applied.is_empty() {
                println!("Nothing for niruio doctor --fix to repair.");
            }
            for a in &applied {
                println!(
                    "{} {}",
                    if a["ok"] == true {
                        "fixed:"
                    } else {
                        "could not fix:"
                    },
                    a["message"].as_str().unwrap_or_default()
                );
            }
        }
        if !applied.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let (model, error) = gather();
            found = findings(model.as_ref(), error);
        }
    }
    let problems = found
        .iter()
        .filter(|f| f.severity == Severity::Problem)
        .count();
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "version": crate::VERSION,
                "findings": found,
                "problems": problems,
            }))
            .unwrap_or_default()
        );
    } else if brief {
        for f in found.iter().filter(|f| f.severity <= Severity::Warning) {
            println!(
                "{}: {}{}",
                f.severity.word(),
                f.title,
                f.fix_text
                    .as_ref()
                    .map(|t| format!(" (fix: {t})"))
                    .unwrap_or_default()
            );
        }
    } else {
        println!("NIRUIO doctor");
        print_findings(&found);
        let warnings = found
            .iter()
            .filter(|f| f.severity == Severity::Warning)
            .count();
        let fixable = found
            .iter()
            .filter(|f| f.severity <= Severity::Warning && f.fix.is_some())
            .count();
        println!();
        println!(
            "{problems} problem(s), {warnings} warning(s).{}",
            if fixable > 0 && !fix {
                " Run niruio doctor --fix to apply the fixes."
            } else {
                ""
            }
        );
    }
    if problems > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

pub fn saved(as_json: bool) -> ExitCode {
    let state = saved::read();
    if as_json {
        let entries: Vec<Value> = state
            .entries
            .iter()
            .map(|e| {
                json!({
                    "key": e.key, "label": e.label(), "volume": e.level(),
                    "mute": e.mute, "target": e.target, "silencing": e.silencing(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "file": state.stream_file.as_ref().map(|p| p.display().to_string()),
                "entries": entries,
                "default_nodes": state.default_nodes,
            }))
            .unwrap_or_default()
        );
        return ExitCode::SUCCESS;
    }
    match &state.stream_file {
        Some(f) => println!("{}", f.display()),
        None => println!("No WirePlumber stream state in {}", state.dir.display()),
    }
    for e in &state.entries {
        println!(
            "{:<5} {:<6} {:>5}  {}{}",
            if e.output() { "out" } else { "in" },
            match e.mute {
                Some(true) => "muted",
                _ => "",
            },
            e.level().map(pct).unwrap_or_default(),
            e.key,
            if e.silencing() { "  <- silences" } else { "" }
        );
    }
    ExitCode::SUCCESS
}

pub fn reset(what: Option<&str>) -> ExitCode {
    let state = saved::read();
    let keys: Vec<String> = match what {
        Some("--silencing") => state.silencing().iter().map(|e| e.key.clone()).collect(),
        Some(key) => vec![key.to_string()],
        None => {
            eprintln!("niruio reset KEY|--silencing  (see niruio saved)");
            return ExitCode::from(2);
        }
    };
    if keys.is_empty() {
        println!("Nothing remembered as muted or silent.");
        return ExitCode::SUCCESS;
    }
    let mut failed = false;
    for key in keys {
        let entry = state
            .find(&key)
            .cloned()
            .or_else(|| saved::parse_entry(&key, "{}"));
        let Some(entry) = entry else {
            eprintln!("niruio: not a WirePlumber key: {key}");
            failed = true;
            continue;
        };
        match actions::reset_saved(&entry) {
            Ok(()) => println!("reset: {}", entry.key),
            Err(e) => {
                eprintln!("could not reset {}: {e}", entry.key);
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
