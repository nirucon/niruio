//! Interface state and everything a key or click does. Drawing lives in
//! `ui.rs`.

use crate::camera::{self, Camera, Details, Preview};
use crate::doctor::{self, Finding, Severity};
use crate::meter::{Level, Meters, Spec};
use crate::monitor::{Event, Monitor};
use crate::pw::{Graph, Kind, Model, Node};
use crate::saved::State;
use crate::theme::Theme;
use crate::{actions, alsa, mictest, saved};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Out,
    In,
    Apps,
    Cameras,
    Cards,
    Saved,
    Doctor,
}

impl Tab {
    pub const ALL: [Tab; 7] = [
        Tab::Out,
        Tab::In,
        Tab::Apps,
        Tab::Cameras,
        Tab::Cards,
        Tab::Saved,
        Tab::Doctor,
    ];
    // MIDI is planned for a later version and gets its own tab here.

    pub fn title(self) -> &'static str {
        match self {
            Tab::Out => "Out",
            Tab::In => "In",
            Tab::Apps => "Apps",
            Tab::Cameras => "Cameras",
            Tab::Cards => "Cards",
            Tab::Saved => "Saved",
            Tab::Doctor => "Doctor",
        }
    }

    pub fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    pub fn parse(name: &str) -> Option<Tab> {
        match name.to_ascii_lowercase().as_str() {
            "out" | "output" | "outputs" => Some(Tab::Out),
            "in" | "input" | "inputs" | "mic" => Some(Tab::In),
            "apps" | "app" | "programs" | "streams" => Some(Tab::Apps),
            "cameras" | "camera" | "cam" | "video" => Some(Tab::Cameras),
            "cards" | "card" | "profiles" | "bluetooth" => Some(Tab::Cards),
            "saved" | "state" | "remembered" => Some(Tab::Saved),
            "doctor" | "diagnose" => Some(Tab::Doctor),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PopupAction {
    Move {
        stream: u32,
        target: u32,
    },
    Route {
        device: u32,
        index: u32,
        card_device: i64,
    },
    Profile {
        device: u32,
        index: u32,
    },
    MicChannels {
        source: u32,
        value: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct Popup {
    pub title: String,
    pub items: Vec<(String, PopupAction, bool)>,
    pub sel: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Tab(Tab),
    Row(usize),
    /// Volume bar of a row: x start and width in cells.
    Volume(usize, u16, u16),
    Control(usize),
    PopupItem(usize),
}

pub struct App {
    pub theme: Theme,
    pub tab: Tab,
    graph: Graph,
    pending_reset: bool,
    pub model: Model,
    pub model_error: Option<String>,
    monitor: Option<Monitor>,
    pub sel: [usize; 7],
    pub saved: State,
    saved_at: Instant,
    pub findings: Vec<Finding>,
    doctor_at: Option<Instant>,
    pub cameras: Vec<Camera>,
    cameras_at: Option<Instant>,
    pub details: HashMap<String, Details>,
    pub cam_controls: bool,
    pub ctrl_sel: usize,
    pub preview_wanted: bool,
    pub preview: Option<Preview>,
    pub preview_rect: Option<Rect>,
    pub kitty: bool,
    pub kitty_seq: u64,
    pub kitty_visible: bool,
    pub meters: Meters,
    pub levels: HashMap<u32, Level>,
    pub popup: Option<Popup>,
    pub help: bool,
    pub status: Option<(String, Instant, bool)>,
    pub mictest: Option<mictest::Running>,
    pub mic_result: Option<Result<mictest::Result, String>>,
    player: Option<std::process::Child>,
    pub alsa_cache: HashMap<u32, (Instant, Vec<alsa::Control>)>,
    pub hits: Vec<(Rect, Hit)>,
    jobs: Vec<(String, Receiver<Result<String, String>>)>,
    pub busy: Option<String>,
    pub quit: bool,
    last_tick: Instant,
    /// Rows of the current tab that fit, for page keys.
    pub page: usize,
}

fn spawn_job<F>(f: F) -> Receiver<Result<String, String>>
where
    F: FnOnce() -> Result<String, String> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx
}

impl App {
    pub fn new(tab: Tab, live: bool) -> App {
        let (graph, model_error) = match actions::dump() {
            Ok(g) => (g, None),
            Err(e) => (Graph::new(), Some(e)),
        };
        let model = graph.model();
        let mut app = App {
            theme: crate::theme::load(),
            tab,
            graph,
            pending_reset: false,
            model,
            model_error,
            monitor: if live { Some(Monitor::start()) } else { None },
            sel: [0; 7],
            saved: saved::read(),
            saved_at: Instant::now(),
            findings: Vec::new(),
            doctor_at: None,
            cameras: Vec::new(),
            cameras_at: None,
            details: HashMap::new(),
            cam_controls: false,
            ctrl_sel: 0,
            preview_wanted: false,
            preview: None,
            preview_rect: None,
            kitty: camera::kitty_graphics(),
            kitty_seq: 0,
            kitty_visible: false,
            meters: if live { Meters::new() } else { Meters::off() },
            levels: HashMap::new(),
            popup: None,
            help: false,
            status: None,
            mictest: None,
            mic_result: None,
            player: None,
            alsa_cache: HashMap::new(),
            hits: Vec::new(),
            jobs: Vec::new(),
            busy: None,
            quit: false,
            last_tick: Instant::now(),
            page: 5,
        };
        // Select the default device so the first screen shows the active one.
        for (t, list) in [(Tab::Out, app.sinks()), (Tab::In, app.sources())] {
            if let Some(i) = list.iter().position(|n| app.model.is_default(n)) {
                app.sel[t.index()] = i;
            }
        }
        if live {
            actions::cleanup_reset_nodes_later();
        }
        app.enter_tab(tab);
        app
    }

    pub fn say(&mut self, text: impl Into<String>) {
        self.status = Some((text.into(), Instant::now(), false));
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.status = Some((text.into(), Instant::now(), true));
    }

    fn report(&mut self, r: Result<(), String>, ok: impl Into<String>) {
        match r {
            Ok(()) => self.say(ok),
            Err(e) => self.error(e),
        }
    }

    pub fn sinks(&self) -> Vec<Node> {
        self.model.sinks().into_iter().cloned().collect()
    }

    pub fn sources(&self) -> Vec<Node> {
        self.model.sources().into_iter().cloned().collect()
    }

    /// Programs: players first, then recorders.
    pub fn streams(&self) -> Vec<Node> {
        let mut s: Vec<Node> = self.model.streams().into_iter().cloned().collect();
        s.sort_by_key(|n| (n.kind != Kind::Playback, n.program().to_lowercase(), n.id));
        s
    }

    pub fn rows(&self) -> usize {
        match self.tab {
            Tab::Out => self.model.sinks().len(),
            Tab::In => self.model.sources().len(),
            Tab::Apps => self.model.streams().len(),
            Tab::Cameras => self.cameras.len(),
            Tab::Cards => self.model.devices.len(),
            Tab::Saved => self.saved.entries.len(),
            Tab::Doctor => self.findings.len(),
        }
    }

    pub fn selected(&self) -> usize {
        let n = self.rows();
        self.sel[self.tab.index()].min(n.saturating_sub(1))
    }

    pub fn selected_node(&self) -> Option<Node> {
        let i = self.selected();
        match self.tab {
            Tab::Out => self.sinks().get(i).cloned(),
            Tab::In => self.sources().get(i).cloned(),
            Tab::Apps => self.streams().get(i).cloned(),
            _ => None,
        }
    }

    pub fn selected_camera(&self) -> Option<&Camera> {
        self.cameras.get(self.sel[Tab::Cameras.index()])
    }

    pub fn camera_details(&self) -> Option<&Details> {
        self.details.get(&self.selected_camera()?.path)
    }

    pub fn problem_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity <= Severity::Warning)
            .count()
    }

    pub fn enter_tab(&mut self, tab: Tab) {
        if self.tab != tab {
            self.popup = None;
        }
        self.tab = tab;
        match tab {
            Tab::Cameras => {
                self.refresh_cameras(true);
            }
            Tab::Saved => {
                self.saved = saved::read();
                self.saved_at = Instant::now();
            }
            Tab::Doctor => self.run_doctor(),
            _ => {}
        }
        if self.doctor_at.is_none() {
            self.run_doctor();
        }
    }

    pub fn refresh_cameras(&mut self, details: bool) {
        self.cameras = camera::list();
        self.cameras_at = Some(Instant::now());
        let sel = self.sel[Tab::Cameras.index()].min(self.cameras.len().saturating_sub(1));
        self.sel[Tab::Cameras.index()] = sel;
        if details {
            if let Some(cam) = self.cameras.get(sel).cloned() {
                if cam.readable {
                    let d = camera::details(&cam.path);
                    self.details.insert(cam.path.clone(), d);
                }
            }
            // Formats tell an infrared sensor from a colour camera.
            for cam in self.cameras.clone() {
                if cam.readable && !self.details.contains_key(&cam.path) {
                    let d = camera::details(&cam.path);
                    self.details.insert(cam.path.clone(), d);
                }
            }
        }
        for cam in self.cameras.iter_mut() {
            if let Some(d) = self.details.get(&cam.path) {
                cam.infrared |= d.infrared();
            }
        }
    }

    pub fn run_doctor(&mut self) {
        let model = if self.model_error.is_some() && self.graph.is_empty() {
            None
        } else {
            Some(&self.model)
        };
        if self.cameras_at.is_none() {
            self.cameras = camera::list();
            self.cameras_at = Some(Instant::now());
        }
        let amixer = actions::have("amixer");
        let lookup = move |card: u32| {
            if amixer {
                alsa::controls(card)
            } else {
                Vec::new()
            }
        };
        let camera_dev = !self.cameras.is_empty()
            || std::fs::read_dir("/dev").is_ok_and(|d| {
                d.flatten()
                    .any(|e| e.file_name().to_string_lossy().starts_with("video"))
            });
        self.findings = doctor::run(&doctor::Input {
            model,
            error: self.model_error.clone(),
            saved: &self.saved,
            cameras: &self.cameras,
            alsa: &lookup,
            camera_dev_present: camera_dev,
        });
        self.doctor_at = Some(Instant::now());
    }

    /// The ALSA controls of a card, cached for a few seconds.
    pub fn alsa_controls(&mut self, card: u32) -> Vec<alsa::Control> {
        if let Some((at, c)) = self.alsa_cache.get(&card) {
            if at.elapsed() < Duration::from_secs(3) {
                return c.clone();
            }
        }
        let c = if actions::have("amixer") {
            alsa::controls(card)
        } else {
            Vec::new()
        };
        self.alsa_cache.insert(card, (Instant::now(), c.clone()));
        c
    }

    fn wanted_meters(&self) -> Vec<Spec> {
        let spec = |n: &Node, sink: bool, passive: bool, key: u32| Spec {
            node: key,
            target: n.serial.to_string(),
            channels: n.positions.len().clamp(1, 32),
            positions: n.positions.join(","),
            sink,
            passive,
        };
        match self.tab {
            Tab::Out => self
                .sinks()
                .iter()
                .take(12)
                .map(|n| spec(n, true, true, n.id))
                .collect(),
            Tab::In => self
                .sources()
                .iter()
                .take(8)
                .map(|n| spec(n, false, false, n.id))
                .collect(),
            Tab::Apps => self
                .streams()
                .iter()
                .take(16)
                .filter_map(|s| {
                    if s.kind == Kind::Playback {
                        Some(spec(s, false, true, s.id))
                    } else {
                        // A recorder is metered by what it records.
                        let src = self.model.stream_targets(s).into_iter().next()?;
                        Some(spec(src, src.kind == Kind::Sink, true, s.id))
                    }
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Everything time-based: graph updates, meters, tests, jobs.
    pub fn tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32().min(0.5);
        self.last_tick = now;
        // Graph changes.
        let mut changed = false;
        if let Some(m) = &self.monitor {
            while let Ok(ev) = m.rx.try_recv() {
                match ev {
                    Event::Reset => self.pending_reset = true,
                    Event::Batch(batch) => {
                        if self.pending_reset {
                            self.graph = Graph::new();
                            self.pending_reset = false;
                        }
                        changed |= self.graph.apply(batch);
                        self.model_error = None;
                    }
                    Event::Down(e) => {
                        self.model_error = Some(e);
                    }
                }
            }
        }
        if changed {
            self.model = self.graph.model();
        }
        // Meters for what this tab shows.
        let wanted = self.wanted_meters();
        self.meters.sync(&wanted);
        let ids: Vec<u32> = wanted.iter().map(|w| w.node).collect();
        for id in &ids {
            if let Some(p) = self.meters.take(*id) {
                self.levels.entry(*id).or_default().update(&p, dt);
            }
        }
        self.levels.retain(|id, _| ids.contains(id));
        // Mic test.
        if let Some(t) = self.mictest.as_mut() {
            if let Some(result) = t.poll() {
                self.mictest = None;
                match &result {
                    Ok(r) => {
                        let (ch, peak) = r.loudest();
                        let msg = format!(
                            "Mic test: {}, peak {peak:.1} dBFS{}",
                            r.verdict(),
                            if r.channels > 2 {
                                format!(" on input {}", ch + 1)
                            } else {
                                String::new()
                            }
                        );
                        let bad = r.clipped.iter().any(|c| *c > 0) || peak < -50.0;
                        if bad {
                            self.error(msg);
                        } else {
                            self.say(msg);
                        }
                    }
                    Err(e) => {
                        let e = e.clone();
                        self.error(e);
                    }
                }
                self.mic_result = Some(result);
            }
        }
        if let Some(p) = self.player.as_mut() {
            if let Ok(Some(_)) = p.try_wait() {
                self.player = None;
            }
        }
        // Background jobs (resets, fixes).
        let mut done = Vec::new();
        for (i, (_, rx)) in self.jobs.iter().enumerate() {
            if let Ok(r) = rx.try_recv() {
                done.push((i, r));
            }
        }
        for (i, r) in done.into_iter().rev() {
            self.jobs.remove(i);
            match r {
                Ok(m) => self.say(m),
                Err(e) => self.error(e),
            }
            self.saved = saved::read();
            self.saved_at = Instant::now();
            self.doctor_at = None;
        }
        self.busy = self.jobs.first().map(|(l, _)| l.clone());
        // Preview only while the Cameras tab shows it.
        let want_preview = self.tab == Tab::Cameras
            && self.preview_wanted
            && self.popup.is_none()
            && !self.help
            && self.selected_camera().is_some_and(|c| c.readable);
        if want_preview {
            let path = self
                .selected_camera()
                .map(|c| c.path.clone())
                .unwrap_or_default();
            if let Some(rect) = self.preview_rect {
                let (w, h) = self.preview_size(rect);
                let restart = self
                    .preview
                    .as_ref()
                    .is_none_or(|p| p.path != path || p.width != w || p.height != h);
                if restart && rect.width >= 8 && rect.height >= 4 {
                    self.preview = None;
                    match Preview::start(&path, w, h, 5) {
                        Ok(p) => self.preview = Some(p),
                        Err(e) => {
                            self.preview_wanted = false;
                            self.error(e);
                        }
                    }
                }
            }
            if let Some(e) = self.preview.as_ref().and_then(|p| p.error()) {
                self.preview = None;
                self.preview_wanted = false;
                self.error(format!("Preview stopped: {e}"));
            }
        } else if self.preview.is_some() {
            self.preview = None;
        }
        // Slow refreshes.
        if self.saved_at.elapsed() > Duration::from_secs(2) {
            self.saved = saved::read();
            self.saved_at = Instant::now();
        }
        if self.tab == Tab::Cameras
            && self
                .cameras_at
                .is_none_or(|t| t.elapsed() > Duration::from_secs(3))
        {
            self.refresh_cameras(false);
        }
        let doctor_every = if self.tab == Tab::Doctor { 3 } else { 6 };
        if self
            .doctor_at
            .is_none_or(|t| t.elapsed() > Duration::from_secs(doctor_every))
        {
            self.run_doctor();
        }
        if self.status.as_ref().is_some_and(|(_, at, err)| {
            at.elapsed() > Duration::from_secs(if *err { 8 } else { 4 })
        }) {
            self.status = None;
        }
    }

    /// Pixel size of preview frames for a cell rectangle.
    pub fn preview_size(&self, rect: Rect) -> (u32, u32) {
        let cols = rect.width.max(1) as u32;
        let rows = rect.height.max(1) as u32;
        if self.kitty {
            // Cell shape from the terminal's pixel size when it reports one.
            let ratio = crossterm::terminal::window_size()
                .ok()
                .filter(|s| s.width > 0 && s.height > 0 && s.columns > 0 && s.rows > 0)
                .map(|s| (s.height as f32 / s.rows as f32) / (s.width as f32 / s.columns as f32))
                .unwrap_or(2.0)
                .clamp(1.2, 3.0);
            let w = 480u32;
            let h = ((w as f32 * rows as f32 * ratio / cols as f32) as u32).clamp(60, 480) / 2 * 2;
            (w, h)
        } else {
            (cols, rows * 2)
        }
    }

    /// How often to redraw: fast while meters or a preview run.
    pub fn frame_interval(&self) -> Duration {
        if self.meters.count() > 0 || self.preview.is_some() || self.mictest.is_some() {
            Duration::from_millis(60)
        } else if !self.jobs.is_empty() {
            Duration::from_millis(150)
        } else {
            Duration::from_millis(400)
        }
    }

    fn move_sel(&mut self, delta: i64) {
        if self.tab == Tab::Cameras && self.cam_controls {
            let n = self.camera_details().map(|d| d.controls.len()).unwrap_or(0);
            self.ctrl_sel =
                (self.ctrl_sel as i64 + delta).clamp(0, n.saturating_sub(1) as i64) as usize;
            return;
        }
        let n = self.rows();
        let i = self.tab.index();
        let next = (self.sel[i].min(n.saturating_sub(1)) as i64 + delta)
            .clamp(0, n.saturating_sub(1) as i64) as usize;
        if next != self.sel[i] {
            self.sel[i] = next;
            if self.tab == Tab::Cameras {
                self.ctrl_sel = 0;
                self.refresh_cameras(true);
            }
        }
    }

    fn change_volume(&mut self, delta: f32) {
        let Some(n) = self.selected_node() else {
            return;
        };
        let v = ((n.volume() + delta) * 100.0).round() / 100.0;
        let v = v.clamp(0.0, actions::MAX_VOLUME);
        let r = actions::set_volume(&n, v);
        self.report(r, format!("{} {:.0} %", short(&label_of(&n)), v * 100.0));
    }

    pub fn set_volume_at(&mut self, row: usize, fraction: f32) {
        self.sel[self.tab.index()] = row;
        let Some(n) = self.selected_node() else {
            return;
        };
        let v = (fraction.clamp(0.0, 1.0) * actions::MAX_VOLUME * 100.0).round() / 100.0;
        let r = actions::set_volume(&n, v);
        self.report(r, format!("{} {:.0} %", short(&label_of(&n)), v * 100.0));
    }

    fn toggle_mute(&mut self) {
        let Some(n) = self.selected_node() else {
            return;
        };
        let r = actions::set_mute(&n, !n.mute);
        self.report(
            r,
            format!(
                "{} {}",
                short(&label_of(&n)),
                if n.mute { "unmuted" } else { "muted" }
            ),
        );
    }

    fn make_default(&mut self) {
        let Some(n) = self.selected_node() else {
            return;
        };
        if n.is_stream() {
            return;
        }
        let r = actions::set_default(&n);
        self.report(r, format!("{} is now the default", short(&n.description)));
    }

    fn balance(&mut self, delta: Option<f32>) {
        let Some(n) = self.selected_node() else {
            return;
        };
        if !n.stereo() {
            self.error("Balance needs a stereo device");
            return;
        }
        let b = match delta {
            Some(d) => (n.balance() + d).clamp(-1.0, 1.0),
            None => 0.0,
        };
        let r = actions::set_balance(&self.model, &n, b);
        self.report(r, format!("Balance {}", balance_text(b)));
    }

    fn open_ports(&mut self) {
        let Some(n) = self.selected_node() else {
            return;
        };
        let (Some(dev), Some(cd)) = (
            n.device_id.and_then(|d| self.model.device(d)),
            n.profile_device,
        ) else {
            self.error("This device has no ports to choose");
            return;
        };
        let current = dev.route_for(cd).map(|r| r.index);
        let items: Vec<(String, PopupAction, bool)> = dev
            .routes_for(cd)
            .into_iter()
            .map(|r| {
                let avail = match r.available.as_str() {
                    "no" => "  (not connected)",
                    "yes" => "  (connected)",
                    _ => "",
                };
                (
                    format!("{}{avail}", r.description),
                    PopupAction::Route {
                        device: dev.id,
                        index: r.index,
                        card_device: cd,
                    },
                    Some(r.index) == current,
                )
            })
            .collect();
        if items.is_empty() {
            self.error("This device has no ports to choose");
            return;
        }
        let sel = items.iter().position(|i| i.2).unwrap_or(0);
        self.popup = Some(Popup {
            title: format!("Port of {}", short(&n.description)),
            items,
            sel,
        });
    }

    pub fn open_profiles(&mut self, device: u32) {
        let Some(dev) = self.model.device(device) else {
            return;
        };
        let items: Vec<(String, PopupAction, bool)> = dev
            .profiles
            .iter()
            .map(|p| {
                let avail = if p.available == "no" {
                    "  (unavailable)"
                } else {
                    ""
                };
                (
                    format!("{}{avail}", crate::pw::profile_label(p)),
                    PopupAction::Profile {
                        device: dev.id,
                        index: p.index,
                    },
                    Some(p.index) == dev.profile,
                )
            })
            .collect();
        let sel = items.iter().position(|i| i.2).unwrap_or(0);
        self.popup = Some(Popup {
            title: format!("Profile of {}", short(&dev.description)),
            items,
            sel,
        });
    }

    fn open_move(&mut self) {
        let Some(s) = self.selected_node() else {
            return;
        };
        if !s.is_stream() {
            return;
        }
        let current: Vec<u32> = self.model.stream_targets(&s).iter().map(|n| n.id).collect();
        let items: Vec<(String, PopupAction, bool)> = actions::move_targets(&self.model, &s)
            .into_iter()
            .map(|t| {
                (
                    format!("{}  [{}]", t.description, t.connection()),
                    PopupAction::Move {
                        stream: s.id,
                        target: t.id,
                    },
                    current.contains(&t.id),
                )
            })
            .collect();
        if items.is_empty() {
            self.error("Nowhere to move it");
            return;
        }
        let sel = items.iter().position(|i| i.2).unwrap_or(0);
        self.popup = Some(Popup {
            title: format!("Move {} to", short(&s.program())),
            items,
            sel,
        });
    }

    fn open_channels(&mut self) {
        let Some(n) = self.selected_node() else {
            return;
        };
        if n.kind != Kind::Source {
            return;
        }
        let count = n.positions.len();
        let mut items = vec![(
            format!("All {count} inputs (as the source is)"),
            PopupAction::MicChannels {
                source: n.id,
                value: None,
            },
            false,
        )];
        if count > 2 {
            for i in 1..=count {
                items.push((
                    format!("Input {i} as mono"),
                    PopupAction::MicChannels {
                        source: n.id,
                        value: Some(i.to_string()),
                    },
                    false,
                ));
            }
            for i in (1..count).step_by(2) {
                items.push((
                    format!("Inputs {i} and {} as stereo", i + 1),
                    PopupAction::MicChannels {
                        source: n.id,
                        value: Some(format!("{i},{}", i + 1)),
                    },
                    false,
                ));
            }
        }
        let current_mic = camera::niruclip_get("mic");
        let current = camera::niruclip_get("mic_channels");
        if current_mic.as_deref() == Some(n.name.as_str()) {
            for it in items.iter_mut() {
                if let PopupAction::MicChannels { value, .. } = &it.1 {
                    it.2 = value.as_deref() == current.as_deref().filter(|v| *v != "all");
                }
            }
        }
        let sel = items.iter().position(|i| i.2).unwrap_or(0);
        self.popup = Some(Popup {
            title: format!("NIRUCLIP records {}", short(&n.description)),
            items,
            sel,
        });
    }

    fn popup_choose(&mut self) {
        let Some(p) = self.popup.take() else { return };
        let Some((label, action, _)) = p.items.get(p.sel).cloned() else {
            return;
        };
        match action {
            PopupAction::Move { stream, target } => {
                let (Some(s), Some(t)) = (
                    self.model.node(stream).cloned(),
                    self.model.node(target).cloned(),
                ) else {
                    return self.error("The program or device is gone");
                };
                let r = actions::move_stream(&s, &t);
                self.report(
                    r,
                    format!("{} → {}", short(&s.program()), short(&t.description)),
                );
            }
            PopupAction::Route {
                device,
                index,
                card_device,
            } => {
                let r = actions::set_route(device, index, card_device);
                self.report(r, format!("Port: {label}"));
            }
            PopupAction::Profile { device, index } => {
                let r = actions::set_profile(device, index);
                self.report(r, format!("Profile: {label}"));
            }
            PopupAction::MicChannels { source, value } => {
                let Some(n) = self.model.node(source).cloned() else {
                    return;
                };
                let r = camera::niruclip_set("mic", Some(&n.name))
                    .and_then(|_| camera::niruclip_set("mic_channels", value.as_deref()));
                match r {
                    Ok(path) => self.say(format!(
                        "NIRUCLIP: {} ({}) in {}",
                        short(&n.description),
                        value
                            .as_deref()
                            .map(|v| format!("inputs {v}"))
                            .unwrap_or_else(|| "all inputs".into()),
                        path.display()
                    )),
                    Err(e) => self.error(e),
                }
            }
        }
    }

    fn adjust_boost(&mut self, up: bool) {
        let Some(n) = self.selected_node() else {
            return;
        };
        let Some(card) = n.alsa_card else {
            return self.error("No ALSA boost on this input");
        };
        let controls = self.alsa_controls(card);
        let Some(b) = alsa::boosts(&controls).into_iter().next().cloned() else {
            return self.error("No ALSA boost on this input");
        };
        let v = b.value.unwrap_or(b.min);
        let next = if up {
            (v + 1).min(b.max)
        } else {
            (v - 1).max(b.min)
        };
        let r = actions::amixer_set(card, &b.id(), &next.to_string());
        self.alsa_cache.remove(&card);
        self.report(r, format!("{} {next} of {}", b.name, b.max));
    }

    fn start_mictest(&mut self) {
        if self.mictest.is_some() {
            return;
        }
        let Some(n) = self.selected_node() else {
            return;
        };
        if n.kind != Kind::Source {
            return;
        }
        match mictest::start(n.id, n.serial, n.positions.len().max(1), 5) {
            Ok(r) => {
                self.mictest = Some(r);
                self.mic_result = None;
                self.say(format!(
                    "Recording 5 s from {}… speak now",
                    short(&n.description)
                ));
            }
            Err(e) => self.error(e),
        }
    }

    fn play_mictest(&mut self) {
        let Some(Ok(r)) = &self.mic_result else {
            return self.error("Run a mic test first (t)");
        };
        let file = r.file.clone();
        match mictest::play(&file) {
            Ok(child) => {
                self.player = Some(child);
                self.say("Playing the test recording");
            }
            Err(e) => self.error(e),
        }
    }

    fn reset_selected_saved(&mut self, all: bool) {
        let entries: Vec<saved::Entry> = if all {
            self.saved.silencing().into_iter().cloned().collect()
        } else {
            match self.saved.entries.get(self.selected()) {
                Some(e) if e.is_stream() => vec![e.clone()],
                Some(_) => {
                    return self.error("Device entries follow the device; change the device itself")
                }
                None => return,
            }
        };
        if entries.is_empty() {
            return self.say("Nothing remembered as muted or silent");
        }
        let label = if all {
            format!("Resetting {} entries…", entries.len())
        } else {
            format!("Resetting {}…", entries[0].label())
        };
        let rx = spawn_job(move || {
            let mut done = 0;
            for e in &entries {
                actions::reset_saved(e).map_err(|err| format!("{}: {err}", e.label()))?;
                done += 1;
            }
            Ok(format!(
                "Reset {done} remembered entr{} to 100 % unmuted",
                if done == 1 { "y" } else { "ies" }
            ))
        });
        self.jobs.push((label, rx));
    }

    fn apply_fixes(&mut self, all: bool) {
        let fixes: Vec<doctor::Fix> = if all {
            self.findings
                .iter()
                .filter(|f| f.severity <= Severity::Warning)
                .filter_map(|f| f.fix.clone())
                .collect()
        } else {
            match self
                .findings
                .get(self.selected())
                .and_then(|f| f.fix.clone())
            {
                Some(f) => vec![f],
                None => return self.say("No automatic fix for this one"),
            }
        };
        if fixes.is_empty() {
            return self.say("Nothing to fix");
        }
        let model = self.model.clone();
        let state = self.saved.clone();
        let n = fixes.len();
        let rx = spawn_job(move || {
            let mut msgs = Vec::new();
            for f in &fixes {
                msgs.push(doctor::apply(f, Some(&model), &state)?);
            }
            Ok(if msgs.len() == 1 {
                format!("Fixed: {}", msgs[0])
            } else {
                format!("Applied {} fixes", msgs.len())
            })
        });
        self.jobs.push((
            format!("Applying {n} fix{}…", if n == 1 { "" } else { "es" }),
            rx,
        ));
    }

    fn camera_key(&mut self, code: KeyCode, shift: bool) -> bool {
        let Some(cam) = self.selected_camera().cloned() else {
            return false;
        };
        match code {
            KeyCode::Char('v') | KeyCode::Char(' ') => {
                self.preview_wanted = !self.preview_wanted;
                if !self.preview_wanted {
                    self.preview = None;
                }
                true
            }
            KeyCode::Char('n') => {
                match camera::niruclip_set("camera", Some(&cam.stable)) {
                    Ok(p) => self.say(format!("NIRUCLIP camera: {} ({})", cam.stable, p.display())),
                    Err(e) => self.error(e),
                }
                true
            }
            KeyCode::Enter | KeyCode::Tab if !shift => {
                self.cam_controls = !self.cam_controls;
                true
            }
            KeyCode::Esc if self.cam_controls => {
                self.cam_controls = false;
                true
            }
            KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('h')
            | KeyCode::Char('l')
            | KeyCode::Char('0')
                if self.cam_controls =>
            {
                let Some(ctrl) = self
                    .camera_details()
                    .and_then(|d| d.controls.get(self.ctrl_sel))
                    .cloned()
                else {
                    return true;
                };
                let value = match code {
                    KeyCode::Char('0') => ctrl.default,
                    KeyCode::Right | KeyCode::Char('l') => ctrl.stepped(true, shift),
                    _ => ctrl.stepped(false, shift),
                };
                if ctrl.inactive {
                    self.error(format!(
                        "{} is inactive (an automatic mode controls it)",
                        ctrl.label()
                    ));
                    return true;
                }
                match camera::set_control(&cam.path, &ctrl.name, value) {
                    Ok(()) => {
                        if let Some(d) = self.details.get_mut(&cam.path) {
                            if let Some(c) = d.controls.get_mut(self.ctrl_sel) {
                                c.value = value;
                            }
                        }
                        let text = self
                            .camera_details()
                            .and_then(|d| d.controls.get(self.ctrl_sel))
                            .map(|c| format!("{} {}", c.label(), c.value_text()))
                            .unwrap_or_default();
                        self.say(text);
                        // Automatic modes switch other controls on or off.
                        if ctrl.kind != "int" {
                            let d = camera::details(&cam.path);
                            self.details.insert(cam.path.clone(), d);
                        }
                    }
                    Err(e) => self.error(e),
                }
                true
            }
            _ => false,
        }
    }

    pub fn key(&mut self, k: KeyEvent) {
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.help {
            self.help = false;
            return;
        }
        if let Some(p) = self.popup.as_mut() {
            match k.code {
                KeyCode::Esc | KeyCode::Char('q') => self.popup = None,
                KeyCode::Up | KeyCode::Char('k') => p.sel = p.sel.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    p.sel = (p.sel + 1).min(p.items.len().saturating_sub(1))
                }
                KeyCode::Enter | KeyCode::Char(' ') => self.popup_choose(),
                _ => {}
            }
            return;
        }
        if self.tab == Tab::Cameras && self.camera_key(k.code, shift) {
            return;
        }
        let step = if shift { 0.01 } else { 0.05 };
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('?') | KeyCode::F(1) => self.help = true,
            KeyCode::Tab => {
                let next = Tab::ALL[(self.tab.index() + 1) % Tab::ALL.len()];
                self.enter_tab(next);
            }
            KeyCode::BackTab => {
                let next = Tab::ALL[(self.tab.index() + Tab::ALL.len() - 1) % Tab::ALL.len()];
                self.enter_tab(next);
            }
            KeyCode::Char(c @ '1'..='7') => {
                let i = c as usize - '1' as usize;
                self.enter_tab(Tab::ALL[i]);
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::PageUp => self.move_sel(-(self.page.max(1) as i64)),
            KeyCode::PageDown => self.move_sel(self.page.max(1) as i64),
            KeyCode::Home | KeyCode::Char('g') => self.move_sel(-100_000),
            KeyCode::End | KeyCode::Char('G') => self.move_sel(100_000),
            KeyCode::Char('r') if matches!(self.tab, Tab::Doctor) => {
                self.run_doctor();
                self.say("Checked again");
            }
            KeyCode::Char('r') | KeyCode::Enter if self.tab == Tab::Saved => {
                self.reset_selected_saved(false)
            }
            KeyCode::Char('R') if self.tab == Tab::Saved => self.reset_selected_saved(true),
            KeyCode::Char('f') | KeyCode::Enter if self.tab == Tab::Doctor => {
                self.apply_fixes(false)
            }
            KeyCode::Char('F') if self.tab == Tab::Doctor => self.apply_fixes(true),
            KeyCode::Enter | KeyCode::Char('p') if self.tab == Tab::Cards => {
                if let Some(d) = self.model.devices.get(self.selected()) {
                    let id = d.id;
                    self.open_profiles(id);
                }
            }
            KeyCode::Char('o') if self.tab == Tab::Cards => {
                // Ports of the card: through its first node.
                if let Some(d) = self.model.devices.get(self.selected()).cloned() {
                    let node = self
                        .model
                        .nodes
                        .iter()
                        .find(|n| n.device_id == Some(d.id))
                        .cloned();
                    if let Some(n) = node {
                        let tab = if n.kind == Kind::Sink {
                            Tab::Out
                        } else {
                            Tab::In
                        };
                        let list = if tab == Tab::Out {
                            self.sinks()
                        } else {
                            self.sources()
                        };
                        if let Some(i) = list.iter().position(|x| x.id == n.id) {
                            let old = self.tab;
                            self.tab = tab;
                            self.sel[tab.index()] = i;
                            self.open_ports();
                            self.tab = old;
                        }
                    } else {
                        self.error("This card has no active device; choose a profile first");
                    }
                }
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('-') if self.is_device_tab() => {
                self.change_volume(-step)
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('+') | KeyCode::Char('=')
                if self.is_device_tab() =>
            {
                self.change_volume(step)
            }
            KeyCode::Char('m') if self.is_device_tab() => self.toggle_mute(),
            KeyCode::Char('d') | KeyCode::Enter if matches!(self.tab, Tab::Out | Tab::In) => {
                self.make_default()
            }
            KeyCode::Char('[') if self.is_device_tab() => self.balance(Some(-0.1)),
            KeyCode::Char(']') if self.is_device_tab() => self.balance(Some(0.1)),
            KeyCode::Char('c') if self.is_device_tab() => self.balance(None),
            KeyCode::Char('p') if matches!(self.tab, Tab::Out | Tab::In) => self.open_ports(),
            KeyCode::Char('P') if matches!(self.tab, Tab::Out | Tab::In) => {
                if let Some(d) = self.selected_node().and_then(|n| n.device_id) {
                    self.open_profiles(d);
                }
            }
            KeyCode::Char('o') | KeyCode::Enter if self.tab == Tab::Apps => self.open_move(),
            KeyCode::Char('t') if self.tab == Tab::In => self.start_mictest(),
            KeyCode::Char('y') if self.tab == Tab::In => self.play_mictest(),
            KeyCode::Char('b') if self.tab == Tab::In => self.adjust_boost(false),
            KeyCode::Char('B') if self.tab == Tab::In => self.adjust_boost(true),
            KeyCode::Char('n') if self.tab == Tab::In => self.open_channels(),
            _ => {}
        }
    }

    pub fn is_device_tab(&self) -> bool {
        matches!(self.tab, Tab::Out | Tab::In | Tab::Apps)
    }

    pub fn mouse(&mut self, m: MouseEvent) {
        let hit = self
            .hits
            .iter()
            .rev()
            .find(|(r, _)| {
                m.column >= r.x
                    && m.column < r.x + r.width
                    && m.row >= r.y
                    && m.row < r.y + r.height
            })
            .map(|(_, h)| *h);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.help {
                    self.help = false;
                    return;
                }
                match hit {
                    Some(Hit::Tab(t)) => self.enter_tab(t),
                    Some(Hit::PopupItem(i)) => {
                        if let Some(p) = self.popup.as_mut() {
                            p.sel = i;
                        }
                        self.popup_choose();
                    }
                    Some(_) if self.popup.is_some() => {}
                    None if self.popup.is_some() => self.popup = None,
                    Some(Hit::Row(i)) => {
                        if self.tab == Tab::Cameras && self.sel[self.tab.index()] != i {
                            self.sel[self.tab.index()] = i;
                            self.ctrl_sel = 0;
                            self.refresh_cameras(true);
                        } else {
                            self.sel[self.tab.index()] = i;
                        }
                    }
                    Some(Hit::Volume(i, x, w)) => {
                        let f = (m.column.saturating_sub(x)) as f32 / w.max(1) as f32;
                        self.set_volume_at(i, f);
                    }
                    Some(Hit::Control(i)) => {
                        self.cam_controls = true;
                        self.ctrl_sel = i;
                    }
                    None => {}
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if let Some(Hit::Row(i) | Hit::Volume(i, _, _)) = hit {
                    self.sel[self.tab.index()] = i;
                    if self.is_device_tab() {
                        self.toggle_mute();
                    }
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = m.kind == MouseEventKind::ScrollUp;
                if let Some(p) = self.popup.as_mut() {
                    p.sel = if up {
                        p.sel.saturating_sub(1)
                    } else {
                        (p.sel + 1).min(p.items.len().saturating_sub(1))
                    };
                    return;
                }
                match hit {
                    Some(Hit::Volume(i, _, _)) => {
                        self.sel[self.tab.index()] = i;
                        self.change_volume(if up { 0.05 } else { -0.05 });
                    }
                    _ => self.move_sel(if up { -1 } else { 1 }),
                }
            }
            _ => {}
        }
    }

    pub fn shutdown(&mut self) {
        self.meters.stop_all();
        self.preview = None;
        self.mictest = None;
        if let Some(mut p) = self.player.take() {
            let _ = p.kill();
            let _ = p.wait();
        }
        let _ = std::fs::remove_file(mictest::temp_file());
    }
}

pub fn label_of(n: &Node) -> String {
    if n.is_stream() {
        n.program()
    } else {
        n.description.clone()
    }
}

pub fn short(text: &str) -> String {
    let t: String = text.chars().take(40).collect();
    if text.chars().count() > 40 {
        format!("{t}…")
    } else {
        t
    }
}

pub fn balance_text(b: f32) -> String {
    if b.abs() < 0.005 {
        "centre".into()
    } else if b < 0.0 {
        format!("{:.0} % left", -b * 100.0)
    } else {
        format!("{:.0} % right", b * 100.0)
    }
}
