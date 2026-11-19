//! Configuration: parsing, validation and key-name resolution.
//!
//! A config is a set of named layers. One layer is shown by default, another
//! (optionally) while Fn is held. A button can `open` another layer, which
//! expands it over the current one with a back button; `switch` replaces the
//! current layer instead.

use anyhow::{Context, Result, anyhow, bail};
use evdev::KeyCode;
use serde::Deserialize;
use std::{collections::HashMap, fs, path::Path, sync::OnceLock};

pub const DEFAULT_PATH: &str = "/etc/touchbar-daemon/config.toml";

/// Used when no config file exists; also installed as the example config.
pub const DEFAULT_CONFIG: &str = include_str!("../data/config.toml");

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub general: General,
    #[serde(rename = "layer")]
    pub layers: Vec<Layer>,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct General {
    /// Layer shown when nothing else is active.
    pub default_layer: String,
    /// Layer shown while Fn is held; empty disables it.
    pub fn_layer: String,
    /// Draw a background behind each button.
    pub outlines: bool,
    /// Pango font family and style, e.g. "Sans Bold".
    pub font: String,
    /// Text size in pixels.
    pub font_size: f64,
    /// Icon size in pixels.
    pub icon_size: f64,
    /// Seconds without a touch before dimming; 0 never dims.
    pub dim_after: u64,
    /// Seconds without a touch before turning off; 0 never turns off.
    pub off_after: u64,
    /// Seconds without a touch before an opened layer collapses; 0 never.
    pub collapse_after: u64,
    /// Add a virtual Esc key on the left, for models without a physical one.
    pub esc_button: bool,
    /// Mirror touch input along the bar, if presses land on the wrong side.
    pub flip_touch: bool,
    /// Rotate the picture the other way, if it appears upside down.
    pub flip_display: bool,
}

impl Default for General {
    fn default() -> Self {
        General {
            default_layer: "media".into(),
            fn_layer: "function".into(),
            outlines: true,
            font: "Sans Bold".into(),
            font_size: 20.0,
            icon_size: 30.0,
            dim_after: 30,
            off_after: 60,
            collapse_after: 15,
            esc_button: false,
            flip_touch: false,
            flip_display: false,
        }
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub name: String,
    pub buttons: Vec<Button>,
}

#[derive(Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Button {
    pub label: Option<String>,
    pub icon: Option<String>,
    /// "12h", "24h", or a strftime format.
    pub clock: Option<String>,
    /// "percent", "icon" or "both".
    pub battery: Option<String>,
    /// A key name, or a list of them pressed together.
    #[serde(default, deserialize_with = "one_or_many")]
    pub key: Vec<String>,
    /// Expand this layer over the current one.
    pub open: Option<String>,
    /// Replace the current layer with this one.
    pub switch: Option<String>,
    /// How many button spaces this button takes up.
    pub width: Option<u32>,
    /// Draw the icon in white (true) or in its own colours (false).
    pub tint: Option<bool>,
}

impl Button {
    pub fn width(&self) -> u32 {
        self.width.unwrap_or(1).max(1)
    }

    pub fn is_spacer(&self) -> bool {
        self.label.is_none()
            && self.icon.is_none()
            && self.clock.is_none()
            && self.battery.is_none()
            && self.key.is_empty()
            && self.open.is_none()
            && self.switch.is_none()
    }

    /// The resolved key codes; empty when the button sends no keys.
    pub fn keys(&self) -> Vec<KeyCode> {
        self.key.iter().filter_map(|k| parse_key(k)).collect()
    }
}

fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    })
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => DEFAULT_CONFIG.to_string(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Config> {
        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    pub fn layer(&self, name: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.name == name)
    }

    fn validate(&self) -> Result<()> {
        let g = &self.general;
        let mut names = HashMap::new();
        for layer in &self.layers {
            if names.insert(layer.name.as_str(), ()).is_some() {
                bail!("layer \"{}\" is defined twice", layer.name);
            }
        }
        let exists = |name: &str| names.contains_key(name);
        if !exists(&g.default_layer) {
            bail!("default_layer \"{}\" does not exist", g.default_layer);
        }
        if !g.fn_layer.is_empty() && !exists(&g.fn_layer) {
            bail!("fn_layer \"{}\" does not exist", g.fn_layer);
        }
        if !(4.0..=60.0).contains(&g.font_size) || !(4.0..=60.0).contains(&g.icon_size) {
            bail!("font_size and icon_size must be between 4 and 60");
        }
        for layer in &self.layers {
            if layer.buttons.is_empty() {
                bail!("layer \"{}\" has no buttons", layer.name);
            }
            for (i, b) in layer.buttons.iter().enumerate() {
                let at = || format!("layer \"{}\", button {}", layer.name, i + 1);
                for key in &b.key {
                    if parse_key(key).is_none() {
                        bail!("{}: unknown key \"{key}\"", at());
                    }
                }
                let actions = [!b.key.is_empty(), b.open.is_some(), b.switch.is_some()];
                if actions.iter().filter(|a| **a).count() > 1 {
                    bail!("{}: use only one of key, open and switch", at());
                }
                for target in [&b.open, &b.switch].into_iter().flatten() {
                    if !exists(target) {
                        bail!("{}: layer \"{target}\" does not exist", at());
                    }
                }
                if let Some(style) = &b.battery {
                    if !matches!(style.as_str(), "percent" | "icon" | "both") {
                        return Err(anyhow!(
                            "{}: battery must be \"percent\", \"icon\" or \"both\"",
                            at()
                        ));
                    }
                }
                if let Some(spec) = &b.clock {
                    if crate::render::clock_text(spec).is_none() {
                        bail!("{}: invalid clock format \"{spec}\"", at());
                    }
                }
                if b.width == Some(0) {
                    bail!("{}: width must be at least 1", at());
                }
            }
        }
        Ok(())
    }
}

impl Config {
    /// Checks the config without loading it from a file.
    pub fn check(&self) -> Result<()> {
        self.validate()
    }

    /// Writes the config as TOML, one inline table per button.
    pub fn to_toml(&self) -> String {
        let g = &self.general;
        let d = General::default();
        let mut out = String::from(
            "# Oma Touchbar configuration, written by the settings app.\n\
             # Every option is documented in `touchbar-daemon --print-default-config`.\n\n\
             [general]\n",
        );
        let mut line = |k: &str, v: String| out += &format!("{k} = {v}\n");
        line("default_layer", quote(&g.default_layer));
        line("fn_layer", quote(&g.fn_layer));
        line("outlines", g.outlines.to_string());
        line("font", quote(&g.font));
        line("font_size", g.font_size.to_string());
        line("icon_size", g.icon_size.to_string());
        line("dim_after", g.dim_after.to_string());
        line("off_after", g.off_after.to_string());
        line("collapse_after", g.collapse_after.to_string());
        // Only write the hardware fixes when they are needed.
        if g.esc_button != d.esc_button {
            line("esc_button", g.esc_button.to_string());
        }
        if g.flip_touch != d.flip_touch {
            line("flip_touch", g.flip_touch.to_string());
        }
        if g.flip_display != d.flip_display {
            line("flip_display", g.flip_display.to_string());
        }
        for layer in &self.layers {
            out += &format!("\n[[layer]]\nname = {}\nbuttons = [\n", quote(&layer.name));
            for b in &layer.buttons {
                out += &format!("    {},\n", b.to_toml());
            }
            out += "]\n";
        }
        out
    }

    /// A layer name not used yet, starting from `base`.
    pub fn unused_layer_name(&self, base: &str) -> String {
        (1..)
            .map(|n| if n == 1 { base.to_string() } else { format!("{base}-{n}") })
            .find(|name| self.layer(name).is_none())
            .unwrap()
    }

    /// Whether any button opens or switches to `layer`.
    pub fn is_referenced(&self, layer: &str) -> bool {
        self.layers.iter().flat_map(|l| &l.buttons).any(|b| {
            b.open.as_deref() == Some(layer) || b.switch.as_deref() == Some(layer)
        })
    }
}

impl Button {
    fn to_toml(&self) -> String {
        let mut parts = Vec::new();
        let text = |parts: &mut Vec<String>, k: &str, v: &Option<String>| {
            if let Some(v) = v {
                parts.push(format!("{k} = {}", quote(v)));
            }
        };
        text(&mut parts, "label", &self.label);
        text(&mut parts, "icon", &self.icon);
        text(&mut parts, "clock", &self.clock);
        text(&mut parts, "battery", &self.battery);
        match self.key.as_slice() {
            [] => {}
            [one] => parts.push(format!("key = {}", quote(one))),
            many => parts.push(format!(
                "key = [{}]",
                many.iter().map(|k| quote(k)).collect::<Vec<_>>().join(", ")
            )),
        }
        text(&mut parts, "open", &self.open);
        text(&mut parts, "switch", &self.switch);
        if let Some(w) = self.width {
            parts.push(format!("width = {w}"));
        }
        if let Some(t) = self.tint {
            parts.push(format!("tint = {t}"));
        }
        if parts.is_empty() {
            "{}".into()
        } else {
            format!("{{ {} }}", parts.join(", "))
        }
    }
}

fn quote(s: &str) -> String {
    toml::Value::String(s.to_owned()).to_string()
}

/// Every key name the daemon accepts, in a friendly spelling (`VolumeUp`).
pub fn key_names() -> Vec<String> {
    (1..0x100u16)
        .chain(0x160..0x2ff)
        .filter_map(|code| {
            let debug = format!("{:?}", KeyCode::new(code));
            let short = debug.strip_prefix("KEY_")?;
            Some(friendly(short))
        })
        .collect()
}

/// Spells a kernel key name the way the example config does:
/// VOLUMEUP -> VolumeUp, KBDILLUMDOWN -> KbdIllumDown, LEFTCTRL -> LeftCtrl.
fn friendly(upper: &str) -> String {
    const WORDS: &[&str] = &[
        "BRIGHTNESS", "BACKSPACE", "PREVIOUS", "FORWARD", "CONTROL", "DISPLAY", "PROGRAM",
        "CONFIG", "VOLUME", "SEARCH", "SCREEN", "REWIND", "RECORD", "INSERT", "DELETE",
        "SCROLL", "SWITCH", "CAMERA", "BATTERY", "TOGGLE", "PAUSE", "PRINT", "SPACE", "ENTER",
        "ILLUM", "RIGHT", "SLEEP", "POWER", "CLOSE", "CYCLE", "SHIFT", "MEDIA", "SOUND", "PHONE",
        "LEFT", "CTRL", "META", "PLAY", "NEXT", "SONG", "MUTE", "DOWN", "PAGE", "HOME", "LOCK",
        "STOP", "MENU", "BACK", "FIND", "COPY", "UNDO", "REDO", "HELP", "ZOOM", "FILE", "MAIL",
        "CALC", "TIME", "AUTO", "FAST", "EJECT", "WAKE", "OPEN", "SEND", "CUT", "ALT", "TAB",
        "END", "KBD", "MIC", "OFF", "WWW", "OUT", "UP", "IN", "CD", "ON",
    ];
    let mut out = String::new();
    let mut rest = upper;
    while !rest.is_empty() {
        if let Some(word) = WORDS.iter().find(|w| rest.starts_with(**w)) {
            out += &word[..1];
            out += &word[1..].to_ascii_lowercase();
            rest = &rest[word.len()..];
        } else {
            // Digits and unknown letters: keep the first capital, lower the rest.
            let c = rest.chars().next().unwrap();
            let starts_word = out.chars().last().is_none_or(|p| p.is_ascii_digit() || p == '_');
            if starts_word || c.is_ascii_digit() {
                out.push(c);
            } else {
                out.push(c.to_ascii_lowercase());
            }
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// Resolves a key name. Accepts kernel names with or without the `KEY_`
/// prefix, in any case and with or without underscores: `VolumeUp`,
/// `volume_up` and `KEY_VOLUMEUP` are the same key.
pub fn parse_key(name: &str) -> Option<KeyCode> {
    static NAMES: OnceLock<HashMap<String, KeyCode>> = OnceLock::new();
    let names = NAMES.get_or_init(|| {
        // Keyboard keys live below BTN_MISC and from KEY_OK upwards.
        (1..0x100u16)
            .chain(0x160..0x2ff)
            .map(KeyCode::new)
            .filter_map(|k| {
                let debug = format!("{k:?}");
                let short = debug.strip_prefix("KEY_")?;
                Some((normalize(short), k))
            })
            .collect()
    });
    let trimmed = name.trim();
    let short = trimmed
        .strip_prefix("KEY_")
        .or_else(|| trimmed.strip_prefix("key_"))
        .unwrap_or(trimmed);
    names.get(&normalize(short)).copied()
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        Config::parse(DEFAULT_CONFIG).unwrap();
    }

    #[test]
    fn round_trips_through_toml() {
        let mut config = Config::parse(DEFAULT_CONFIG).unwrap();
        config.general.flip_touch = true;
        config.layers[0].buttons.push(Button {
            label: Some("say \"hi\"".into()),
            key: vec!["LeftCtrl".into(), "C".into()],
            width: Some(2),
            tint: Some(false),
            ..Default::default()
        });
        config.layers[0].buttons.push(Button::default());
        let text = config.to_toml();
        assert_eq!(Config::parse(&text).unwrap(), config);
    }

    #[test]
    fn listed_key_names_parse() {
        let names = super::key_names();
        assert!(names.len() > 200);
        assert!(names.iter().all(|n| parse_key(n).is_some()));
        for want in ["VolumeUp", "KbdIllumDown", "LeftCtrl", "PlayPause", "PreviousSong", "BrightnessDown", "F12", "Esc"] {
            assert!(names.iter().any(|n| n == want), "{want} missing");
        }
    }

    #[test]
    fn key_names() {
        assert_eq!(parse_key("VolumeUp"), Some(KeyCode::KEY_VOLUMEUP));
        assert_eq!(parse_key("volume_up"), Some(KeyCode::KEY_VOLUMEUP));
        assert_eq!(parse_key("KEY_VOLUMEUP"), Some(KeyCode::KEY_VOLUMEUP));
        assert_eq!(parse_key("F12"), Some(KeyCode::KEY_F12));
        assert_eq!(parse_key("LeftCtrl"), Some(KeyCode::KEY_LEFTCTRL));
        assert_eq!(parse_key("KbdIllumUp"), Some(KeyCode::KEY_KBDILLUMUP));
        assert_eq!(parse_key("BrightnessDown"), Some(KeyCode::KEY_BRIGHTNESSDOWN));
        assert_eq!(parse_key("NotAKey"), None);
        assert_eq!(parse_key("BTN_LEFT"), None);
    }

    #[test]
    fn rejects_bad_configs() {
        let base = "[[layer]]\nname = \"media\"\nbuttons = [";
        let bad = [
            "{ key = \"Nope\" }]",
            "{ open = \"missing\" }]",
            "{ key = \"A\", open = \"media\" }]",
            "{ battery = \"full\" }]",
            "{ width = 0, label = \"x\" }]",
            "]",
        ];
        for b in bad {
            assert!(Config::parse(&format!("{base}{b}")).is_err(), "{b}");
        }
        let ok = format!("[general]\nfn_layer = \"\"\n{base}{{ label = \"x\", key = [\"LeftCtrl\", \"C\"] }}]");
        Config::parse(&ok).unwrap();
    }
}
