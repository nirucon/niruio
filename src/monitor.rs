//! Follow the PipeWire graph with `pw-dump --monitor`: one JSON array per
//! change, read on a thread and handed to the interface. If pw-dump stops
//! (PipeWire restarted), it is started again after a short pause.

use serde_json::Value;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub enum Event {
    /// A fresh start: replace the whole graph with what follows.
    Reset,
    Batch(Vec<Value>),
    Down(String),
}

pub struct Monitor {
    pub rx: Receiver<Event>,
    child: Arc<Mutex<Option<Child>>>,
    stop: Arc<Mutex<bool>>,
}

impl Monitor {
    pub fn start() -> Monitor {
        let (tx, rx) = std::sync::mpsc::channel();
        let child: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
        let stop = Arc::new(Mutex::new(false));
        if std::env::var("NIRUIO_PW_DUMP").is_ok() {
            // Fixture mode: the interface loads the file once.
            return Monitor { rx, child, stop };
        }
        let c2 = child.clone();
        let s2 = stop.clone();
        std::thread::spawn(move || run(tx, c2, s2));
        Monitor { rx, child, stop }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        if let Ok(mut s) = self.stop.lock() {
            *s = true;
        }
        if let Ok(mut c) = self.child.lock() {
            if let Some(mut child) = c.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn run(tx: Sender<Event>, slot: Arc<Mutex<Option<Child>>>, stop: Arc<Mutex<bool>>) {
    loop {
        if stop.lock().map(|s| *s).unwrap_or(true) {
            return;
        }
        let spawned = Command::new("pw-dump")
            .args(["-m", "-N"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(mut child) => {
                let out = child.stdout.take();
                if let Ok(mut s) = slot.lock() {
                    *s = Some(child);
                }
                if tx.send(Event::Reset).is_err() {
                    return;
                }
                if let Some(out) = out {
                    let reader = std::io::BufReader::new(out);
                    let stream =
                        serde_json::Deserializer::from_reader(reader).into_iter::<Vec<Value>>();
                    for batch in stream {
                        match batch {
                            Ok(batch) => {
                                if tx.send(Event::Batch(batch)).is_err() {
                                    return;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                }
                if let Ok(mut s) = slot.lock() {
                    if let Some(mut child) = s.take() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                }
                if tx
                    .send(Event::Down("PipeWire stopped answering; retrying".into()))
                    .is_err()
                {
                    return;
                }
            }
            Err(e) => {
                if tx.send(Event::Down(format!("pw-dump: {e}"))).is_err() {
                    return;
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}
