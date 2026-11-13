//! Colours of the active NIRUX theme, read the way NIRUFETCH and NIRUWM do:
//! the `theme` id in `~/.config/niruwm/config.toml`, the built-in catalog, and a
//! user theme file of the same id in `~/.config/niruwm/themes` on top.

use ratatui::style::Color;
use std::path::{Path, PathBuf};

pub type Rgb = (u8, u8, u8);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub accent: Rgb,
    pub foreground: Rgb,
    pub muted: Rgb,
    pub border: Rgb,
    pub background: Rgb,
    /// Light themes (Carl Larsson) need darker warning colours.
    pub light: bool,
}

/// id, accent (focus), foreground, muted, border, background
const BUILTIN: &[(&str, Rgb, Rgb, Rgb, Rgb, Rgb)] = &[
    (
        "niru-noir",
        (0x5a, 0x5a, 0x60),
        (0xe5, 0xe5, 0xe5),
        (0xa8, 0xa8, 0xa8),
        (0x2a, 0x2a, 0x2d),
        (0x0f, 0x0f, 0x10),
    ),
    (
        "niru-nord",
        (0x88, 0xc0, 0xd0),
        (0xec, 0xef, 0xf4),
        (0x9a, 0xa5, 0xb1),
        (0x3b, 0x42, 0x52),
        (0x2e, 0x34, 0x40),
    ),
    (
        "niru-satie",
        (0xb0, 0x8d, 0x7d),
        (0xde, 0xda, 0xd4),
        (0x8f, 0x8b, 0x86),
        (0x2a, 0x27, 0x24),
        (0x14, 0x14, 0x14),
    ),
    (
        "niru-carl-larsson",
        (0xa6, 0x3d, 0x2f),
        (0x3b, 0x32, 0x28),
        (0x7d, 0x6f, 0x5f),
        (0xd8, 0xcd, 0xbb),
        (0xf3, 0xec, 0xe0),
    ),
    (
        "niru-forest-vigil",
        (0x8a, 0xa8, 0x7e),
        (0xd8, 0xde, 0xd2),
        (0x96, 0xa2, 0x92),
        (0x2b, 0x36, 0x2c),
        (0x0b, 0x10, 0x0c),
    ),
    (
        "niru-blood-of-urdr",
        (0xc9, 0x60, 0x50),
        (0xe8, 0xd6, 0xcf),
        (0xb0, 0x9a, 0x92),
        (0x3a, 0x1d, 0x1b),
        (0x12, 0x08, 0x08),
    ),
    (
        "niru-root-and-stone",
        (0xc1, 0x97, 0x68),
        (0xea, 0xdf, 0xd2),
        (0xb5, 0xa0, 0x8c),
        (0x3a, 0x2a, 0x1e),
        (0x17, 0x10, 0x0b),
    ),
    (
        "niru-blue-horizon",
        (0x86, 0xae, 0xc8),
        (0xdc, 0xe6, 0xeb),
        (0x93, 0xa6, 0xb2),
        (0x24, 0x38, 0x46),
        (0x0b, 0x18, 0x21),
    ),
    (
        "niru-the-norns",
        (0xc9, 0xc2, 0xb9),
        (0xec, 0xe9, 0xe4),
        (0xa6, 0xa1, 0x9b),
        (0x3b, 0x39, 0x36),
        (0x11, 0x11, 0x10),
    ),
    (
        "niru-winter-nights",
        (0xd8, 0xc4, 0xaa),
        (0xea, 0xd9, 0xc3),
        (0xb9, 0xaa, 0x9a),
        (0x4e, 0x51, 0x57),
        (0x1f, 0x23, 0x2b),
    ),
];

impl Default for Theme {
    fn default() -> Self {
        let (_, accent, foreground, muted, border, background) = BUILTIN[0];
        Theme {
            accent,
            foreground,
            muted,
            border,
            background,
            light: false,
        }
    }
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
}

/// A top-level `key = value` of a small TOML file: lines before the first
/// table header, the value unquoted and a trailing comment removed.
pub fn toml_top_level(text: &str, wanted: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            return None;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != wanted {
            continue;
        }
        let value = value.trim();
        let value = if let Some(rest) = value.strip_prefix('"') {
            rest.split('"').next().unwrap_or_default()
        } else {
            value.split('#').next().unwrap_or_default().trim()
        };
        return Some(value.to_string());
    }
    None
}

pub fn hex_rgb(value: &str) -> Option<Rgb> {
    let value = value.trim().trim_start_matches('#');
    if value.len() != 6 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(&value[at..at + 2], 16).ok();
    Some((byte(0)?, byte(2)?, byte(4)?))
}

fn luminance((r, g, b): Rgb) -> f32 {
    0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32
}

/// The theme in `niruwm_dir` (normally `~/.config/niruwm`).
pub fn theme_in(niruwm_dir: &Path) -> Theme {
    let read = |path: PathBuf| std::fs::read_to_string(path).unwrap_or_default();
    let id = toml_top_level(&read(niruwm_dir.join("config.toml")), "theme")
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| "niru-noir".into());
    let mut theme = Theme::default();
    if let Some(&(_, accent, foreground, muted, border, background)) =
        BUILTIN.iter().find(|entry| entry.0 == id)
    {
        theme = Theme {
            accent,
            foreground,
            muted,
            border,
            background,
            light: false,
        };
    }
    // Only plain ids are looked up, so a config value never leaves the folder.
    if id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        let text = read(niruwm_dir.join("themes").join(format!("{id}.toml")));
        let colour = |key: &str| toml_top_level(&text, key).and_then(|v| hex_rgb(&v));
        if let Some(rgb) = colour("focus") {
            theme.accent = rgb;
        }
        if let Some(rgb) = colour("foreground") {
            theme.foreground = rgb;
        }
        if let Some(rgb) = colour("muted") {
            theme.muted = rgb;
        }
        if let Some(rgb) = colour("border") {
            theme.border = rgb;
        }
        if let Some(rgb) = colour("background") {
            theme.background = rgb;
        }
    }
    // Noir's grey accent is too dark for text on its own background.
    if luminance(theme.accent) < 110.0 && luminance(theme.background) < 60.0 {
        theme.accent = theme.muted;
    }
    theme.light = luminance(theme.background) > 150.0;
    theme
}

pub fn load() -> Theme {
    theme_in(&config_dir().join("niruwm"))
}

pub fn rgb(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

impl Theme {
    pub fn accent(&self) -> Color {
        rgb(self.accent)
    }
    pub fn fg(&self) -> Color {
        rgb(self.foreground)
    }
    pub fn muted(&self) -> Color {
        rgb(self.muted)
    }
    pub fn border(&self) -> Color {
        rgb(self.border)
    }
    pub fn bg(&self) -> Color {
        rgb(self.background)
    }
    pub fn good(&self) -> Color {
        if self.light {
            Color::Rgb(0x3d, 0x7a, 0x3a)
        } else {
            Color::Rgb(0x8f, 0xc0, 0x7e)
        }
    }
    pub fn warn(&self) -> Color {
        if self.light {
            Color::Rgb(0x9a, 0x6a, 0x10)
        } else {
            Color::Rgb(0xe0, 0xb8, 0x60)
        }
    }
    pub fn bad(&self) -> Color {
        if self.light {
            Color::Rgb(0xa8, 0x2a, 0x20)
        } else {
            Color::Rgb(0xe0, 0x68, 0x58)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_and_user_files() {
        let dir = std::env::temp_dir().join(format!("niruio-theme-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("themes")).unwrap();
        assert_eq!(theme_in(&dir).foreground, (0xe5, 0xe5, 0xe5));
        std::fs::write(dir.join("config.toml"), "theme = \"niru-forest-vigil\"\n").unwrap();
        assert_eq!(theme_in(&dir).accent, (0x8a, 0xa8, 0x7e));
        std::fs::write(
            dir.join("themes/niru-forest-vigil.toml"),
            "focus = \"102030\"\nbackground = \"f0f0f0\"\n",
        )
        .unwrap();
        let t = theme_in(&dir);
        assert_eq!(t.accent, (0x10, 0x20, 0x30));
        assert!(t.light);
        std::fs::write(dir.join("config.toml"), "theme = \"../evil\"\n").unwrap();
        assert_eq!(theme_in(&dir).foreground, (0xe5, 0xe5, 0xe5));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn carl_larsson_is_light() {
        let dir = std::env::temp_dir().join(format!("niruio-theme-cl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), "theme = \"niru-carl-larsson\"\n").unwrap();
        assert!(theme_in(&dir).light);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
