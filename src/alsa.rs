//! ALSA mixer controls under PipeWire: microphone boost, and hardware
//! switches that silence a card even when PipeWire shows it unmuted.
//! Read from `amixer -c CARD scontents`.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Control {
    pub name: String,
    pub index: u32,
    pub capabilities: Vec<String>,
    /// Playback switch per channel, when the control has one.
    pub playback_on: Option<bool>,
    /// Capture switch, when the control has one.
    pub capture_on: Option<bool>,
    pub min: i64,
    pub max: i64,
    /// The first channel's value.
    pub value: Option<i64>,
    pub db: Option<String>,
}

impl Control {
    pub fn is_boost(&self) -> bool {
        self.name.contains("Boost") && self.max > self.min
    }

    /// The name amixer wants, with the index for repeated names.
    pub fn id(&self) -> String {
        if self.index == 0 {
            self.name.clone()
        } else {
            format!("{},{}", self.name, self.index)
        }
    }
}

fn parse_name(line: &str) -> Option<(String, u32)> {
    // Simple mixer control 'Mic Boost',0
    let rest = line.strip_prefix("Simple mixer control '")?;
    let (name, index) = rest.rsplit_once("',")?;
    Some((name.to_string(), index.trim().parse().unwrap_or(0)))
}

pub fn parse(text: &str) -> Vec<Control> {
    let mut out: Vec<Control> = Vec::new();
    for line in text.lines() {
        if let Some((name, index)) = parse_name(line) {
            out.push(Control {
                name,
                index,
                ..Control::default()
            });
            continue;
        }
        let Some(c) = out.last_mut() else { continue };
        let line = line.trim();
        if let Some(caps) = line.strip_prefix("Capabilities:") {
            c.capabilities = caps.split_whitespace().map(String::from).collect();
        } else if let Some(limits) = line.strip_prefix("Limits:") {
            let nums: Vec<i64> = limits
                .split(|ch: char| !(ch.is_ascii_digit() || ch == '-'))
                .filter(|x| !x.is_empty() && *x != "-")
                .filter_map(|x| x.parse().ok())
                .collect();
            if nums.len() >= 2 {
                c.min = nums[nums.len() - 2];
                c.max = nums[nums.len() - 1];
            }
        } else if let Some((_, values)) = line.split_once(':') {
            // "Front Left: Playback 56 [64%] [-23.25dB] [on]",
            // "Front Left: Capture 0 [0%] [0.00dB] [off]", "Mono: 2 [67%]".
            let values = values.trim();
            if values.is_empty()
                || line.starts_with("Playback channels")
                || line.starts_with("Capture channels")
            {
                continue;
            }
            let capture = values.starts_with("Capture");
            let switch = if values.contains("[on]") {
                Some(true)
            } else if values.contains("[off]") {
                Some(false)
            } else {
                None
            };
            if let Some(on) = switch {
                if capture {
                    c.capture_on = Some(c.capture_on.unwrap_or(false) || on);
                } else {
                    c.playback_on = Some(c.playback_on.unwrap_or(false) || on);
                }
            }
            if c.value.is_none() {
                c.value = values
                    .split_whitespace()
                    .find(|w| w.parse::<i64>().is_ok())
                    .and_then(|w| w.parse().ok());
                c.db = values
                    .split('[')
                    .find(|p| p.contains("dB"))
                    .map(|p| p.trim_end_matches([']', ' ']).to_string());
            }
        }
    }
    out
}

pub fn controls(card: u32) -> Vec<Control> {
    crate::actions::run(&["amixer", "-c", &card.to_string(), "scontents"])
        .map(|text| parse(&text))
        .unwrap_or_default()
}

/// Boost controls that belong to inputs.
pub fn boosts(controls: &[Control]) -> Vec<&Control> {
    controls.iter().filter(|c| c.is_boost()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Simple mixer control 'Master',0
  Capabilities: pvolume pvolume-joined pswitch pswitch-joined
  Playback channels: Mono
  Limits: Playback 0 - 87
  Mono: Playback 56 [64%] [-23.25dB] [off]
Simple mixer control 'Headphone',0
  Capabilities: pswitch
  Playback channels: Front Left - Front Right
  Mono:
  Front Left: Playback [off]
  Front Right: Playback [off]
Simple mixer control 'Capture',0
  Capabilities: cvolume cswitch
  Capture channels: Front Left - Front Right
  Limits: Capture 0 - 63
  Front Left: Capture 39 [62%] [12.00dB] [off]
  Front Right: Capture 39 [62%] [12.00dB] [off]
Simple mixer control 'Internal Mic Boost',0
  Capabilities: volume
  Playback channels: Front Left - Front Right
  Capture channels: Front Left - Front Right
  Limits: 0 - 3
  Front Left: 1 [33%] [10.00dB]
  Front Right: 1 [33%] [10.00dB]
";

    #[test]
    fn parses_switches_and_boost() {
        let c = parse(SAMPLE);
        assert_eq!(c.len(), 4);
        assert_eq!(c[0].name, "Master");
        assert_eq!(c[0].playback_on, Some(false));
        assert_eq!((c[0].min, c[0].max, c[0].value), (0, 87, Some(56)));
        assert_eq!(c[2].capture_on, Some(false));
        let b = boosts(&c);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].id(), "Internal Mic Boost");
        assert_eq!((b[0].min, b[0].max, b[0].value), (0, 3, Some(1)));
        assert_eq!(b[0].db.as_deref(), Some("10.00dB"));
    }
}
