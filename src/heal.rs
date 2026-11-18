//! `niruio heal`: probe sound cards again when WirePlumber met them before
//! the hardware was ready. NIRUWM runs `niruio heal --login` after each login
//! (so after every reboot); it does nothing when every card looks right.

use crate::actions;
use crate::doctor;
use crate::pw::Model;
use std::io::Write;
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// Cards with a lost direction, as "Description (input)".
fn split(model: &Model) -> Vec<String> {
    model
        .devices
        .iter()
        .filter_map(|d| doctor::split_card(d).map(|m| format!("{} ({m})", d.description)))
        .collect()
}

/// The graph once PipeWire answers with at least one card, or None.
fn wait_for_cards(timeout: Duration) -> Option<Model> {
    let start = Instant::now();
    loop {
        if let Ok(graph) = actions::dump() {
            let model = graph.model();
            if !model.devices.is_empty() {
                return Some(model);
            }
        }
        if start.elapsed() >= timeout {
            return None;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn log(line: &str) {
    eprintln!("niruio heal: {line}");
    let dir = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".local/state"))
        });
    let Some(dir) = dir.map(|d| d.join("niruio")) else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let file = dir.join("heal.log");
    let mut lines: Vec<String> = std::fs::read_to_string(&file)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
    let stamp = std::process::Command::new("date")
        .arg("+%F %T")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    lines.push(format!("{stamp} {line}"));
    let keep = lines.len().saturating_sub(50);
    if let Ok(mut f) = std::fs::File::create(&file) {
        let _ = writeln!(f, "{}", lines[keep..].join("\n"));
    }
}

pub fn heal(login: bool, force: bool) -> ExitCode {
    let fixture = std::env::var("NIRUIO_PW_DUMP").is_ok();
    let wait = if fixture {
        Duration::ZERO
    } else {
        Duration::from_secs(30)
    };
    let Some(model) = wait_for_cards(wait) else {
        log("PipeWire showed no sound card within 60 s; nothing to do");
        return ExitCode::SUCCESS;
    };
    let mut before = split(&model);
    if force && before.is_empty() {
        before.push("every card (forced)".into());
    }
    if before.is_empty() {
        if login {
            log(&format!(
                "login check: {} sound card(s), all with their outputs and inputs",
                model.devices.iter().filter(|d| d.api == "alsa").count()
            ));
        } else {
            println!("Every sound card has its outputs and inputs; nothing to heal.");
        }
        return ExitCode::SUCCESS;
    }
    log(&format!(
        "{}: {}",
        if force {
            "probing again"
        } else {
            "probed too early at start"
        },
        before.join(", ")
    ));
    if fixture {
        if let Err(e) = actions::restart_wireplumber() {
            log(&format!("could not restart WirePlumber: {e}"));
            return ExitCode::FAILURE;
        }
        println!("restarted WirePlumber for {}", before.join(", "));
        return ExitCode::SUCCESS;
    }
    // Slow hardware (speaker amplifiers loading firmware, device permissions
    // that arrive with the login) can need more than one try: wait a little
    // longer before each of at most three restarts.
    let waits: &[u64] = if login { &[3, 6, 10] } else { &[0, 3, 6] };
    let mut still = before.clone();
    for (attempt, wait) in waits.iter().enumerate() {
        std::thread::sleep(Duration::from_secs(*wait));
        if let Err(e) = actions::restart_wireplumber() {
            log(&format!("could not restart WirePlumber: {e}"));
            return ExitCode::FAILURE;
        }
        std::thread::sleep(Duration::from_secs(2));
        still = match wait_for_cards(Duration::from_secs(15)) {
            Some(model) => split(&model),
            None => vec!["no sound card after the restart".into()],
        };
        if still.is_empty() {
            log(&format!(
                "WirePlumber restarted (try {}); every card has its outputs and inputs again",
                attempt + 1
            ));
            if !login {
                println!("Healed: {}", before.join(", "));
            }
            return ExitCode::SUCCESS;
        }
        log(&format!(
            "try {}: still missing: {}",
            attempt + 1,
            still.join(", ")
        ));
    }
    log(&format!(
        "gave up after {} restarts: {}; see niruio doctor",
        waits.len(),
        still.join(", ")
    ));
    if !login {
        println!(
            "Still missing after {} restarts: {}",
            waits.len(),
            still.join(", ")
        );
    }
    ExitCode::FAILURE
}
