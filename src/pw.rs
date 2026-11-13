//! The PipeWire graph as `pw-dump` describes it, and the model NIRUIO shows.
//!
//! `pw-dump` prints a JSON array of objects. In monitor mode (`pw-dump -m`) it
//! prints one array per change; an object with `"info": null` was removed.
//! [`Graph`] merges those batches; [`Graph::model`] turns the raw objects into
//! sinks, sources, streams and cards.

use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default)]
pub struct Graph {
    objects: BTreeMap<u64, Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Sink,
    Source,
    /// A program that plays sound.
    Playback,
    /// A program that records sound.
    Recording,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: u32,
    pub serial: u64,
    pub kind: Kind,
    pub name: String,
    pub description: String,
    pub media_class: String,
    pub device_id: Option<u32>,
    pub profile_device: Option<i64>,
    pub api: String,
    pub bus: String,
    pub form_factor: String,
    pub alsa_card: Option<u32>,
    /// Channel positions, e.g. `FL`, `FR` or `AUX0`..`AUX17`.
    pub positions: Vec<String>,
    /// Per-channel volume in the cubic scale wpctl and pavucontrol show
    /// (1.0 = 100 %).
    pub volumes: Vec<f32>,
    pub mute: bool,
    pub state: String,
    pub app_name: String,
    pub app_binary: String,
    pub media_name: String,
    pub media_role: String,
    pub pid: Option<u32>,
    /// Linked device nodes (for streams) or linked streams (for devices).
    pub links: Vec<u32>,
    pub codec: String,
    pub virtual_node: bool,
    pub rate: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub available: String,
    pub priority: i64,
    pub sinks: u32,
    pub sources: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    pub index: u32,
    pub output: bool,
    pub name: String,
    pub description: String,
    pub available: String,
    pub priority: i64,
    pub profiles: Vec<u32>,
    pub devices: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActiveRoute {
    pub index: u32,
    pub device: i64,
    pub output: bool,
    pub name: String,
    pub description: String,
    pub available: String,
    pub mute: bool,
    pub volumes: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub api: String,
    pub bus: String,
    pub form_factor: String,
    pub alsa_card: Option<u32>,
    pub profiles: Vec<Profile>,
    pub profile: Option<u32>,
    pub routes: Vec<Route>,
    pub active_routes: Vec<ActiveRoute>,
}

impl Device {
    pub fn bluetooth(&self) -> bool {
        self.api == "bluez5"
    }
    pub fn active_profile(&self) -> Option<&Profile> {
        let index = self.profile?;
        self.profiles.iter().find(|p| p.index == index)
    }
    /// The route in use for one card device (`card.profile.device`).
    pub fn route_for(&self, device: i64) -> Option<&ActiveRoute> {
        self.active_routes.iter().find(|r| r.device == device)
    }
    /// Routes (ports) that the active profile offers for one card device.
    pub fn routes_for(&self, device: i64) -> Vec<&Route> {
        let profile = self.profile.unwrap_or(u32::MAX);
        self.routes
            .iter()
            .filter(|r| r.devices.contains(&device) && r.profiles.contains(&profile))
            .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    pub nodes: Vec<Node>,
    pub devices: Vec<Device>,
    pub default_sink: Option<String>,
    pub default_source: Option<String>,
    pub configured_sink: Option<String>,
    pub configured_source: Option<String>,
    /// A WirePlumber client is connected (the session manager runs).
    pub session_manager: bool,
    pub version: String,
}

/// Friendly name of a Bluetooth profile for people who never heard of A2DP.
pub fn profile_label(profile: &Profile) -> String {
    let name = profile.name.as_str();
    let kind = if name.starts_with("a2dp") {
        "Hi-fi playback (A2DP, no microphone)"
    } else if name.starts_with("headset-head-unit") || name.contains("hfp") || name.contains("hsp")
    {
        "Headset with microphone (HFP/HSP, lower quality)"
    } else if name == "off" {
        "Off"
    } else {
        ""
    };
    if kind.is_empty() {
        profile.description.clone()
    } else if profile.description.is_empty() || profile.description == "Off" {
        kind.to_string()
    } else {
        format!("{kind} · {}", profile.description)
    }
}

fn s(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

fn num(v: &Value, key: &str) -> Option<i64> {
    match v.get(key)? {
        Value::Number(n) => n.as_i64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn floats(v: Option<&Value>) -> Vec<f32> {
    v.and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_f64)
                .map(|x| x as f32)
                .collect()
        })
        .unwrap_or_default()
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn ints(v: Option<&Value>) -> Vec<i64> {
    v.and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}

/// PipeWire stores linear channel volumes; people see the cube root.
pub fn to_cubic(linear: f32) -> f32 {
    linear.max(0.0).cbrt()
}

pub fn to_linear(cubic: f32) -> f32 {
    let c = cubic.max(0.0);
    c * c * c
}

/// The `{ "name": "..." }` value of a `default` metadata entry.
fn metadata_name(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => map.get("name").and_then(Value::as_str).map(String::from),
        Value::String(text) => serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|v| v.get("name").and_then(Value::as_str).map(String::from))
            .or_else(|| Some(text.clone())),
        _ => None,
    }
}

/// Count the media classes a profile's `classes` entry lists.
fn profile_counts(classes: Option<&Value>) -> (u32, u32) {
    let mut sinks = 0;
    let mut sources = 0;
    if let Some(items) = classes.and_then(Value::as_array) {
        for item in items.iter().skip(1) {
            if let Some(entry) = item.as_array() {
                let class = entry.first().and_then(Value::as_str).unwrap_or_default();
                let count = entry.get(1).and_then(Value::as_u64).unwrap_or(0) as u32;
                match class {
                    "Audio/Sink" => sinks += count,
                    "Audio/Source" => sources += count,
                    _ => {}
                }
            }
        }
    }
    (sinks, sources)
}

fn parse_profile(p: &Value) -> Option<Profile> {
    let (sinks, sources) = profile_counts(p.get("classes"));
    Some(Profile {
        index: p.get("index")?.as_u64()? as u32,
        name: s(p, "name"),
        description: s(p, "description"),
        available: s(p, "available"),
        priority: num(p, "priority").unwrap_or(0),
        sinks,
        sources,
    })
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Merge one `pw-dump` batch. Returns true when anything changed.
    pub fn apply(&mut self, batch: Vec<Value>) -> bool {
        let mut changed = false;
        for object in batch {
            let Some(id) = object.get("id").and_then(Value::as_u64) else {
                continue;
            };
            let removed =
                object.get("info").is_some_and(Value::is_null) && object.get("metadata").is_none();
            if removed {
                changed |= self.objects.remove(&id).is_some();
            } else if let (Some(Value::Array(update)), Some(old)) =
                (object.get("metadata"), self.objects.get(&id))
            {
                // Monitor mode sends only the metadata entries that changed;
                // a null value removes one.
                let mut entries: Vec<Value> = old
                    .get("metadata")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let same = |a: &Value, b: &Value| {
                    a.get("subject") == b.get("subject") && a.get("key") == b.get("key")
                };
                for entry in update {
                    entries.retain(|e| !same(e, entry));
                    if !entry.get("value").is_none_or(Value::is_null) {
                        entries.push(entry.clone());
                    }
                }
                let mut merged = object.clone();
                merged["metadata"] = Value::Array(entries);
                if self.objects.get(&id) != Some(&merged) {
                    self.objects.insert(id, merged);
                    changed = true;
                }
            } else if let (Some(Value::Object(info)), Some(old)) =
                (object.get("info"), self.objects.get(&id))
            {
                // Monitor mode sends only what changed (see "change-mask"):
                // merge the info fields, and the params one id at a time.
                let mut merged = old.clone();
                if let (Some(dst), Some(src)) = (merged.as_object_mut(), object.as_object()) {
                    for (k, v) in src {
                        if k != "info" {
                            dst.insert(k.clone(), v.clone());
                        }
                    }
                }
                if !merged.get("info").is_some_and(Value::is_object) {
                    merged["info"] = Value::Object(serde_json::Map::new());
                }
                for (k, v) in info {
                    if k == "params" {
                        if !merged["info"].get("params").is_some_and(Value::is_object) {
                            merged["info"]["params"] = Value::Object(serde_json::Map::new());
                        }
                        if let Some(params) = v.as_object() {
                            for (pk, pv) in params {
                                merged["info"]["params"][pk] = pv.clone();
                            }
                        }
                    } else {
                        merged["info"][k] = v.clone();
                    }
                }
                if self.objects.get(&id) != Some(&merged) {
                    self.objects.insert(id, merged);
                    changed = true;
                }
            } else if self.objects.get(&id) != Some(&object) {
                self.objects.insert(id, object);
                changed = true;
            }
        }
        changed
    }

    pub fn from_json(text: &str) -> Result<Self, String> {
        let batch: Vec<Value> =
            serde_json::from_str(text).map_err(|e| format!("pw-dump output: {e}"))?;
        let mut graph = Graph::new();
        graph.apply(batch);
        Ok(graph)
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    pub fn model(&self) -> Model {
        let mut model = Model::default();
        let mut links: Vec<(u32, u32)> = Vec::new();
        for object in self.objects.values() {
            let kind = s(object, "type");
            let info = object.get("info").cloned().unwrap_or(Value::Null);
            let props = info.get("props").cloned().unwrap_or(Value::Null);
            if kind.ends_with(":Core") && model.version.is_empty() {
                model.version = s(&info, "version");
            }
            if kind.ends_with(":Client") {
                let app = s(&props, "application.name");
                if app.starts_with("WirePlumber") {
                    model.session_manager = true;
                }
            }
            if kind.ends_with(":Metadata") {
                let name = object
                    .get("props")
                    .map(|p| s(p, "metadata.name"))
                    .unwrap_or_default();
                if name == "default" {
                    for entry in object
                        .get("metadata")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        let key = s(entry, "key");
                        let value = entry.get("value").and_then(metadata_name);
                        match key.as_str() {
                            "default.audio.sink" => model.default_sink = value,
                            "default.audio.source" => model.default_source = value,
                            "default.configured.audio.sink" => model.configured_sink = value,
                            "default.configured.audio.source" => model.configured_source = value,
                            _ => {}
                        }
                    }
                }
            }
            if kind.ends_with(":Link") {
                let out = info.get("output-node-id").and_then(Value::as_u64);
                let inp = info.get("input-node-id").and_then(Value::as_u64);
                if let (Some(out), Some(inp)) = (out, inp) {
                    links.push((out as u32, inp as u32));
                }
            }
            if kind.ends_with(":Device") {
                let class = s(&props, "media.class");
                if class != "Audio/Device" {
                    continue;
                }
                let params = info.get("params").cloned().unwrap_or(Value::Null);
                let profiles: Vec<Profile> = params
                    .get("EnumProfile")
                    .and_then(Value::as_array)
                    .map(|items| items.iter().filter_map(parse_profile).collect())
                    .unwrap_or_default();
                let profile = params
                    .get("Profile")
                    .and_then(Value::as_array)
                    .and_then(|items| items.first())
                    .and_then(|p| p.get("index"))
                    .and_then(Value::as_u64)
                    .map(|x| x as u32);
                let routes = params
                    .get("EnumRoute")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|r| {
                                Some(Route {
                                    index: r.get("index")?.as_u64()? as u32,
                                    output: s(r, "direction") == "Output",
                                    name: s(r, "name"),
                                    description: s(r, "description"),
                                    available: s(r, "available"),
                                    priority: num(r, "priority").unwrap_or(0),
                                    profiles: ints(r.get("profiles"))
                                        .into_iter()
                                        .map(|x| x as u32)
                                        .collect(),
                                    devices: ints(r.get("devices")),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let active_routes = params
                    .get("Route")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|r| {
                                let props = r.get("props");
                                Some(ActiveRoute {
                                    index: r.get("index")?.as_u64()? as u32,
                                    device: r.get("device")?.as_i64()?,
                                    output: s(r, "direction") == "Output",
                                    name: s(r, "name"),
                                    description: s(r, "description"),
                                    available: s(r, "available"),
                                    mute: props
                                        .and_then(|p| p.get("mute"))
                                        .and_then(Value::as_bool)
                                        .unwrap_or(false),
                                    volumes: floats(props.and_then(|p| p.get("channelVolumes")))
                                        .into_iter()
                                        .map(to_cubic)
                                        .collect(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                model.devices.push(Device {
                    id: object.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
                    name: s(&props, "device.name"),
                    description: first_nonempty(&[
                        s(&props, "device.description"),
                        s(&props, "device.nick"),
                        s(&props, "device.name"),
                    ]),
                    api: s(&props, "device.api"),
                    bus: s(&props, "device.bus"),
                    form_factor: s(&props, "device.form-factor"),
                    alsa_card: num(&props, "api.alsa.card").map(|x| x as u32),
                    profiles,
                    profile,
                    routes,
                    active_routes,
                });
            }
            if kind.ends_with(":Node") {
                let class = s(&props, "media.class");
                let node_kind = match class.as_str() {
                    "Audio/Sink" | "Audio/Duplex" => Kind::Sink,
                    "Audio/Source" | "Audio/Source/Virtual" => Kind::Source,
                    "Stream/Output/Audio" => Kind::Playback,
                    "Stream/Input/Audio" => Kind::Recording,
                    _ => continue,
                };
                let name = s(&props, "node.name");
                let app_name = s(&props, "application.name");
                // NIRUIO's own meters and probes are not programs to show.
                if name.starts_with("niruio-") || app_name == "niruio" {
                    continue;
                }
                let params = info.get("params").cloned().unwrap_or(Value::Null);
                let p = params
                    .get("Props")
                    .and_then(Value::as_array)
                    .and_then(|items| {
                        items
                            .iter()
                            .find(|p| p.get("channelVolumes").is_some())
                            .or(items.first())
                    })
                    .cloned()
                    .unwrap_or(Value::Null);
                let mut positions = strings(p.get("channelMap"));
                if positions.is_empty() {
                    positions = s(&props, "audio.position")
                        .split(',')
                        .map(|x| x.trim().to_string())
                        .filter(|x| !x.is_empty())
                        .collect();
                }
                let channels = num(&props, "audio.channels").unwrap_or(0) as usize;
                if positions.is_empty() && channels > 0 {
                    positions = (0..channels).map(|i| format!("AUX{i}")).collect();
                }
                let volumes: Vec<f32> = floats(p.get("channelVolumes"))
                    .into_iter()
                    .map(to_cubic)
                    .collect();
                let rate = s(&props, "node.rate")
                    .split('/')
                    .nth(1)
                    .and_then(|x| x.parse().ok())
                    .or_else(|| num(&props, "audio.rate").map(|x| x as u32));
                model.nodes.push(Node {
                    id: object.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
                    serial: num(&props, "object.serial").unwrap_or(0) as u64,
                    kind: node_kind,
                    description: first_nonempty(&[
                        s(&props, "node.description"),
                        s(&props, "node.nick"),
                        app_name.clone(),
                        name.clone(),
                    ]),
                    name,
                    media_class: class,
                    device_id: num(&props, "device.id").map(|x| x as u32),
                    profile_device: num(&props, "card.profile.device"),
                    api: s(&props, "device.api"),
                    bus: s(&props, "device.bus"),
                    form_factor: s(&props, "device.form-factor"),
                    alsa_card: num(&props, "api.alsa.card")
                        .or_else(|| num(&props, "alsa.card"))
                        .map(|x| x as u32),
                    positions,
                    volumes,
                    mute: p.get("mute").and_then(Value::as_bool).unwrap_or(false),
                    state: s(&info, "state"),
                    app_name,
                    app_binary: s(&props, "application.process.binary"),
                    media_name: s(&props, "media.name"),
                    media_role: s(&props, "media.role"),
                    pid: num(&props, "application.process.id").map(|x| x as u32),
                    links: Vec::new(),
                    codec: s(&props, "api.bluez5.codec"),
                    virtual_node: props
                        .get("node.virtual")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    rate,
                });
            }
        }
        // Device API, bus and form factor are on the device for ALSA nodes.
        let devices = model.devices.clone();
        for node in &mut model.nodes {
            if let Some(device) = node
                .device_id
                .and_then(|id| devices.iter().find(|d| d.id == id))
            {
                if node.api.is_empty() {
                    node.api = device.api.clone();
                }
                if node.bus.is_empty() {
                    node.bus = device.bus.clone();
                }
                if node.form_factor.is_empty() {
                    node.form_factor = device.form_factor.clone();
                }
                if node.alsa_card.is_none() {
                    node.alsa_card = device.alsa_card;
                }
            }
        }
        for (out, inp) in links {
            for node in &mut model.nodes {
                if node.id == out && !node.links.contains(&inp) {
                    node.links.push(inp);
                }
                if node.id == inp && !node.links.contains(&out) {
                    node.links.push(out);
                }
            }
        }
        model.nodes.sort_by_key(|n| n.id);
        model.devices.sort_by_key(|d| d.id);
        model
    }
}

fn first_nonempty(options: &[String]) -> String {
    options
        .iter()
        .find(|x| !x.trim().is_empty())
        .cloned()
        .unwrap_or_default()
}

impl Node {
    /// The volume people see: the loudest channel.
    pub fn volume(&self) -> f32 {
        self.volumes.iter().copied().fold(0.0, f32::max)
    }

    /// -1.0 (left only) .. 1.0 (right only) for stereo, 0 otherwise.
    pub fn balance(&self) -> f32 {
        if self.volumes.len() != 2 || self.positions.len() != 2 {
            return 0.0;
        }
        let (l, r) = (self.volumes[0], self.volumes[1]);
        let max = l.max(r);
        if max <= 0.0001 {
            return 0.0;
        }
        if r >= l {
            1.0 - l / max
        } else {
            -(1.0 - r / max)
        }
    }

    pub fn stereo(&self) -> bool {
        self.positions.len() == 2
    }

    pub fn is_stream(&self) -> bool {
        matches!(self.kind, Kind::Playback | Kind::Recording)
    }

    /// The program's name for streams.
    pub fn program(&self) -> String {
        first_nonempty(&[
            self.app_name.clone(),
            self.app_binary.clone(),
            self.description.clone(),
            self.name.clone(),
        ])
    }

    pub fn bluetooth(&self) -> bool {
        self.api == "bluez5" || self.name.starts_with("bluez_")
    }

    /// The kind of connection, for a short tag in lists.
    pub fn connection(&self) -> &'static str {
        let name = self.name.to_ascii_lowercase();
        if self.bluetooth() {
            "Bluetooth"
        } else if name.contains("hdmi") || self.description.to_ascii_lowercase().contains("hdmi") {
            "HDMI"
        } else if self.description.contains("DisplayPort") || name.contains("displayport") {
            "DisplayPort"
        } else if self.bus == "usb" || name.contains(".usb-") {
            "USB"
        } else if self.api == "alsa" {
            "Built-in"
        } else {
            "Virtual"
        }
    }

    /// A monitor source of a sink (`*.monitor`), not a microphone.
    pub fn is_monitor(&self) -> bool {
        self.kind == Kind::Source && self.name.ends_with(".monitor")
    }

    /// The `WirePlumber` state key this stream's volume is remembered under,
    /// in the order WirePlumber picks it (`state-stream.lua` formKey).
    pub fn restore_key(&self, props_role: Option<&str>) -> Option<String> {
        let prefix = match self.kind {
            Kind::Playback => "Output/Audio",
            Kind::Recording => "Input/Audio",
            _ => return None,
        };
        let role = props_role.unwrap_or(&self.media_role);
        if !role.is_empty() {
            return Some(format!("{prefix}:media.role:{role}"));
        }
        if !self.app_name.is_empty() {
            return Some(format!("{prefix}:application.name:{}", self.app_name));
        }
        if !self.media_name.is_empty() {
            return Some(format!("{prefix}:media.name:{}", self.media_name));
        }
        if !self.name.is_empty() {
            return Some(format!("{prefix}:node.name:{}", self.name));
        }
        None
    }
}

impl Model {
    pub fn node(&self, id: u32) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn device(&self, id: u32) -> Option<&Device> {
        self.devices.iter().find(|d| d.id == id)
    }

    pub fn sinks(&self) -> Vec<&Node> {
        self.nodes.iter().filter(|n| n.kind == Kind::Sink).collect()
    }

    pub fn sources(&self) -> Vec<&Node> {
        self.nodes
            .iter()
            .filter(|n| n.kind == Kind::Source && !n.is_monitor())
            .collect()
    }

    pub fn streams(&self) -> Vec<&Node> {
        self.nodes.iter().filter(|n| n.is_stream()).collect()
    }

    pub fn default_sink_node(&self) -> Option<&Node> {
        let name = self.default_sink.as_deref()?;
        self.nodes
            .iter()
            .find(|n| n.kind == Kind::Sink && n.name == name)
    }

    pub fn default_source_node(&self) -> Option<&Node> {
        let name = self.default_source.as_deref()?;
        self.nodes
            .iter()
            .find(|n| n.kind == Kind::Source && n.name == name)
    }

    pub fn is_default(&self, node: &Node) -> bool {
        match node.kind {
            Kind::Sink => self.default_sink.as_deref() == Some(node.name.as_str()),
            Kind::Source => self.default_source.as_deref() == Some(node.name.as_str()),
            _ => false,
        }
    }

    /// The device nodes a stream is linked to, by name.
    pub fn stream_targets(&self, stream: &Node) -> Vec<&Node> {
        stream
            .links
            .iter()
            .filter_map(|id| self.node(*id))
            .filter(|n| !n.is_stream())
            .collect()
    }

    /// Where a node's volume really lives: the device route for ALSA and
    /// Bluetooth nodes that have one, else the node.
    pub fn route_of(&self, node: &Node) -> Option<(&Device, &ActiveRoute)> {
        let device = self.device(node.device_id?)?;
        let route = device.route_for(node.profile_device?)?;
        Some((device, route))
    }

    /// Only the null "Dummy Output" exists: PipeWire found no sound card.
    pub fn only_dummy_output(&self) -> bool {
        let sinks = self.sinks();
        !sinks.is_empty() && sinks.iter().all(|n| n.name == "auto_null")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn fixture() -> Graph {
        Graph::from_json(include_str!("../tests/fixtures/laptop.json")).unwrap()
    }

    #[test]
    fn parses_nodes_devices_defaults() {
        let m = fixture().model();
        assert_eq!(
            m.default_sink.as_deref(),
            Some("alsa_output.pci-0000_c4_00.6.analog-stereo")
        );
        let sinks = m.sinks();
        assert_eq!(sinks.len(), 3, "{sinks:?}");
        let speaker = m.default_sink_node().unwrap();
        assert!(
            (speaker.volume() - 0.4).abs() < 0.01,
            "{}",
            speaker.volume()
        );
        assert!(!speaker.mute);
        assert_eq!(speaker.connection(), "Built-in");
        let hdmi = sinks.iter().find(|n| n.connection() == "HDMI").unwrap();
        assert!(hdmi.mute);
        let bt = sinks.iter().find(|n| n.bluetooth()).unwrap();
        assert_eq!(bt.codec, "aac");
        let sources = m.sources();
        let umc = sources.iter().find(|n| n.positions.len() == 18).unwrap();
        assert_eq!(umc.positions[17], "AUX17");
        assert_eq!(umc.connection(), "USB");
        let streams = m.streams();
        let mpv = streams.iter().find(|n| n.app_name == "mpv").unwrap();
        assert_eq!(mpv.media_role, "Movie");
        assert!(mpv.mute);
        assert_eq!(
            mpv.restore_key(None).as_deref(),
            Some("Output/Audio:media.role:Movie")
        );
        assert_eq!(m.stream_targets(mpv)[0].id, speaker.id);
        assert!(m.session_manager);
        // NIRUIO's own meter streams are hidden.
        assert!(m.nodes.iter().all(|n| !n.name.starts_with("niruio-")));
    }

    #[test]
    fn cards_profiles_routes() {
        let m = fixture().model();
        let card = m
            .devices
            .iter()
            .find(|d| d.api == "alsa" && d.name.contains("c4_00.6"))
            .unwrap();
        assert_eq!(
            card.active_profile().unwrap().name,
            "output:analog-stereo+input:analog-stereo"
        );
        assert_eq!(card.routes_for(1).len(), 2);
        assert_eq!(card.route_for(1).unwrap().name, "analog-output-speaker");
        let bt = m.devices.iter().find(|d| d.bluetooth()).unwrap();
        let labels: Vec<String> = bt.profiles.iter().map(profile_label).collect();
        assert!(labels.iter().any(|l| l.starts_with("Hi-fi")));
        assert!(labels.iter().any(|l| l.starts_with("Headset")));
        let speaker = m.default_sink_node().unwrap();
        assert!(m.route_of(speaker).is_some());
    }

    #[test]
    fn monitor_batches_merge_and_remove() {
        let mut g = fixture();
        let before = g.model().nodes.len();
        let mpv = g
            .model()
            .streams()
            .iter()
            .find(|n| n.app_name == "mpv")
            .unwrap()
            .id;
        assert!(g.apply(vec![serde_json::json!({"id": mpv, "info": null})]));
        assert_eq!(g.model().nodes.len(), before - 1);
        assert!(!g.apply(vec![serde_json::json!({"id": 99999, "info": null})]));
        // A metadata update carries only the changed key.
        g.apply(vec![
            serde_json::json!({"id": 40, "type": "PipeWire:Interface:Metadata",
            "props": {"metadata.name": "default"},
            "metadata": [{"subject": 0, "key": "default.configured.audio.sink",
                          "type": "Spa:String:JSON", "value": {"name": "x"}}]}),
        ]);
        let m = g.model();
        assert_eq!(m.configured_sink.as_deref(), Some("x"));
        assert_eq!(
            m.default_sink.as_deref(),
            Some("alsa_output.pci-0000_c4_00.6.analog-stereo")
        );
        g.apply(vec![
            serde_json::json!({"id": 40, "type": "PipeWire:Interface:Metadata",
            "props": {"metadata.name": "default"},
            "metadata": [{"subject": 0, "key": "default.audio.sink", "value": null}]}),
        ]);
        assert_eq!(g.model().default_sink, None);
        // A node update carries only the changed params.
        g.apply(vec![
            serde_json::json!({"id": 60, "type": "PipeWire:Interface:Node",
            "info": {"change-mask": ["params"], "params": {"Props": [
                {"mute": true, "channelVolumes": [1.0, 1.0], "channelMap": ["FL", "FR"]}]}}}),
        ]);
        let m = g.model();
        let speaker = m.node(60).unwrap();
        assert!(speaker.mute);
        assert_eq!(
            speaker.description,
            "Family 17h/19h HD Audio Controller Speaker"
        );
    }

    #[test]
    fn balance_and_cubic() {
        let mut n = fixture().model().default_sink_node().unwrap().clone();
        n.volumes = vec![0.5, 1.0];
        assert!((n.balance() - 0.5).abs() < 0.001);
        n.volumes = vec![1.0, 0.25];
        assert!((n.balance() + 0.75).abs() < 0.001);
        assert!((to_linear(to_cubic(0.3)) - 0.3).abs() < 1e-5);
    }
}
