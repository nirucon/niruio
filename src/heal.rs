//! `niruio heal`: probe sound cards again when WirePlumber met them before
//! the hardware was ready. NIRUWM runs `niruio heal --login` once after each
//! login; it does nothing when every card looks right.

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

/// Once per login: a marker in the runtime dir, which goes away at logout.
fn first_time_this_login() -> bool {
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return true;
    };
    let marker = std::path::Path::new(&dir).join("niruio-heal-login");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker)
        .is_ok()
}

pub fn heal(login: bool, force: bool) -> ExitCode {
    let fixture = std::env::var("NIRUIO_PW_DUMP").is_ok();
    if login && !fixture && !first_time_this_login() {
        return ExitCode::SUCCESS;
    }
    let wait = if fixture {
        Duration::ZERO
    } else {
        Duration::from_secs(30)
    };
    let Some(model) = wait_for_cards(wait) else {
        log("PipeWire showed no sound card within 30 s; nothing to do");
        return ExitCode::SUCCESS;
    };
    let mut before = split(&model);
    if force && before.is_empty() {
        before.push("every card (forced)".into());
    }
    if before.is_empty() {
        if !login {
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
    if login && !fixture {
        // Give slow hardware (speaker amplifier firmware) a moment more.
        std::thread::sleep(Duration::from_secs(3));
    }
    if let Err(e) = actions::restart_wireplumber() {
        log(&format!("could not restart WirePlumber: {e}"));
        return ExitCode::FAILURE;
    }
    if fixture {
        println!("restarted WirePlumber for {}", before.join(", "));
        return ExitCode::SUCCESS;
    }
    std::thread::sleep(Duration::from_secs(2));
    let after = wait_for_cards(Duration::from_secs(15))
        .map(|m| split(&m))
        .unwrap_or_default();
    if after.is_empty() {
        log("WirePlumber restarted; every card has its outputs and inputs again");
        if !login {
            println!("Healed: {}", before.join(", "));
        }
        ExitCode::SUCCESS
    } else {
        log(&format!(
            "still missing after one restart: {}",
            after.join(", ")
        ));
        if !login {
            println!("Still missing after one restart: {}", after.join(", "));
        }
        ExitCode::FAILURE
    }
}
