//! NIRUIO, "niru in out": sound outputs, inputs, programs, cameras, cards and
//! what WirePlumber remembers, with a doctor that explains silence.

mod actions;
mod alsa;
mod app;
mod camera;
mod doctor;
mod meter;
mod mictest;
mod monitor;
mod pw;
mod saved;
mod status;
mod theme;
mod ui;

use std::process::ExitCode;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "NIRUIO: sound in and out, cameras and a sound doctor

Usage:
  niruio [--tab TAB]           open the terminal interface (TAB: out, in, apps,
                               cameras, cards, saved, doctor); out, in, apps,
                               cameras and cards also work without --tab
  niruio --window [TAB]        open it in its own floating kitty window
  niruio status [--json]       default devices, programs, cameras, problems
  niruio doctor [--json] [--brief] [--fix]
                               why is nothing heard, recorded or filmed;
                               --fix applies every fix for problems and warnings
  niruio saved [--json]        what WirePlumber remembers per role and program
  niruio reset KEY|--silencing reset remembered entries to 100 % unmuted
  niruio --version | --help

Keys: Tab/1-7 switch tabs, arrows or hjkl select and change, m mute,
d default, ? all keys, q quit.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("niruio {VERSION}");
            ExitCode::SUCCESS
        }
        Some("--help" | "-h" | "help") => {
            println!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("status") => status::status(flag("--json")),
        Some("doctor") => status::doctor(flag("--json"), flag("--fix"), flag("--brief")),
        Some("saved") => status::saved(flag("--json")),
        Some("reset") => status::reset(args.get(1).map(String::as_str)),
        Some("--window") => window(args.get(1).map(String::as_str)),
        other => {
            let other = if other == Some("--tab") {
                match args.get(1) {
                    Some(t) => Some(t.as_str()),
                    None => {
                        eprintln!("niruio: --tab needs a tab name");
                        return ExitCode::from(2);
                    }
                }
            } else {
                other
            };
            let tab = match other {
                None => app::Tab::Out,
                Some(name) => match app::Tab::parse(name) {
                    Some(tab) => tab,
                    None => {
                        eprintln!("niruio: unknown command or tab: {name}\n\n{HELP}");
                        return ExitCode::from(2);
                    }
                },
            };
            match ui::run(tab) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("niruio: {e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

/// Open NIRUIO in a kitty window of class `niruio`, which NIRUWM floats and
/// centres. Falls back to x-terminal-emulator.
fn window(tab: Option<&str>) -> ExitCode {
    use std::process::Command;
    let me = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "niruio".into());
    let mut argv: Vec<String> = vec![me];
    if let Some(tab) = tab {
        if app::Tab::parse(tab).is_none() {
            eprintln!("niruio: unknown tab: {tab}");
            return ExitCode::from(2);
        }
        argv.push("--tab".into());
        argv.push(tab.into());
    }
    let result = if actions::have("kitty") {
        Command::new("kitty")
            .args([
                "--class",
                "niruio",
                "--title",
                "NIRUIO",
                "-o",
                "remember_window_size=no",
                "-o",
                "initial_window_width=112c",
                "-o",
                "initial_window_height=36c",
                "-e",
            ])
            .args(&argv)
            .spawn()
    } else {
        Command::new("x-terminal-emulator")
            .args(["-T", "NIRUIO", "-e"])
            .args(&argv)
            .spawn()
    };
    match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("niruio: could not open a terminal window: {e}");
            ExitCode::FAILURE
        }
    }
}
