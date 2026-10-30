//! rnote-pad's settings, read from ~/.config/rnote-pad/config.toml.

use std::ffi::{CString, c_char, c_int};
use std::fmt;
use std::path::{Path, PathBuf};
use std::{env, fs};

use serde::Deserialize;

/// Used when there is no config file; install.sh copies it to ~/.config.
const DEFAULT: &str = include_str!("../config.toml");
const MODIFIERS: [&str; 5] = ["SHIFT", "CTRL", "CONTROL", "ALT", "SUPER"];

#[link(name = "xkbcommon")]
unsafe extern "C" {
    fn xkb_keysym_from_name(name: *const c_char, flags: c_int) -> u32;
}
const XKB_KEYSYM_CASE_INSENSITIVE: c_int = 1;

/// Hyprland resolves key names with xkb and quietly does nothing for unknown
/// ones, so catch typos here instead.
fn is_key_name(key: &str) -> bool {
    let Ok(name) = CString::new(key) else {
        return false;
    };
    // SAFETY: `name` is NUL-terminated and outlives the call.
    unsafe { xkb_keysym_from_name(name.as_ptr(), XKB_KEYSYM_CASE_INSENSITIVE) != 0 }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub window_class: String,
    /// Map the tablet onto the window of `window_class` while it is on screen.
    #[serde(default = "enabled")]
    pub follow_window: bool,
    /// Pads with a touch ring (Intuos Pro 2nd gen).
    #[serde(default)]
    pub buttons: Buttons,
    #[serde(default)]
    pub ring: RingConfig,
    /// Pads with a dial (Intuos Pro 3rd gen), and that tablet's pen.
    #[serde(default)]
    pub dial: DialConfig,
    #[serde(default)]
    pub pen: PenConfig,
    #[serde(skip)]
    pub source: String,
}

/// ExpressKeys are numbered top to bottom; the kernel reports the ring
/// centre after them, as the last pad button.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Buttons {
    key1: Option<Shortcut>,
    key2: Option<Shortcut>,
    key3: Option<Shortcut>,
    key4: Option<Shortcut>,
    key5: Option<Shortcut>,
    key6: Option<Shortcut>,
    ring_button: Option<Shortcut>,
}

impl Buttons {
    /// `index` is the pad button number, i.e. the evdev code minus BTN_0.
    pub fn get(&self, index: u16) -> Option<&Shortcut> {
        match index {
            0 => self.key1.as_ref(),
            1 => self.key2.as_ref(),
            2 => self.key3.as_ref(),
            3 => self.key4.as_ref(),
            4 => self.key5.as_ref(),
            5 => self.key6.as_ref(),
            6 => self.ring_button.as_ref(),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RingConfig {
    pub clockwise: Option<Shortcut>,
    pub counterclockwise: Option<Shortcut>,
    /// Ring positions (72 per turn) per shortcut.
    pub step: u8,
}

impl Default for RingConfig {
    fn default() -> Self {
        Self {
            clockwise: None,
            counterclockwise: None,
            step: 4,
        }
    }
}

/// ExpressKeys round the dial. The kernel numbers them top, right, bottom,
/// left, then the dial's centre button.
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DialConfig {
    top: Option<Shortcut>,
    right: Option<Shortcut>,
    bottom: Option<Shortcut>,
    left: Option<Shortcut>,
    centre: Option<Shortcut>,
    pub clockwise: Option<Shortcut>,
    pub counterclockwise: Option<Shortcut>,
    /// Dial clicks per shortcut.
    pub step: u8,
}

impl DialConfig {
    /// `index` is the pad button number, i.e. the evdev code minus BTN_0.
    pub fn button(&self, index: u16) -> Option<&Shortcut> {
        match index {
            0 => self.top.as_ref(),
            1 => self.right.as_ref(),
            2 => self.bottom.as_ref(),
            3 => self.left.as_ref(),
            4 => self.centre.as_ref(),
            _ => None,
        }
    }
}

impl Default for DialConfig {
    fn default() -> Self {
        Self {
            top: None,
            right: None,
            bottom: None,
            left: None,
            centre: None,
            clockwise: None,
            counterclockwise: None,
            step: 1,
        }
    }
}

/// Pro Pen 3 side buttons.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PenConfig {
    /// The button nearest the tip.
    pub lower: Option<Action>,
    /// The middle and top buttons. Over Bluetooth the tablet reports both
    /// as the eraser end coming into range, so they can't be told apart.
    pub upper: Option<Action>,
}

/// What a pen button sends: one shortcut, or a pair to switch between.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(try_from = "OneOrPair")]
pub enum Action {
    Send(Shortcut),
    /// Sends the second shortcut if the first is the last tool picked
    /// (see `Shortcut::picks_tool`), otherwise the first.
    Switch(Shortcut, Shortcut),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrPair {
    One(String),
    Pair(Vec<String>),
}

impl TryFrom<OneOrPair> for Action {
    type Error = String;

    fn try_from(raw: OneOrPair) -> Result<Self, String> {
        match raw {
            OneOrPair::One(text) => Ok(Self::Send(text.try_into()?)),
            OneOrPair::Pair(texts) => match <[String; 2]>::try_from(texts) {
                Ok([first, second]) => Ok(Self::Switch(first.try_into()?, second.try_into()?)),
                Err(texts) => Err(format!(
                    "{texts:?}: expected one shortcut, or two to switch between"
                )),
            },
        }
    }
}

/// A chord in Hyprland's terms: space-separated modifiers plus a key name.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "String")]
pub struct Shortcut {
    pub mods: String,
    pub key: String,
}

impl TryFrom<String> for Shortcut {
    type Error = String;

    /// Parses "CTRL+SHIFT+Z" style strings.
    fn try_from(text: String) -> Result<Self, String> {
        let mut parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let key = parts.pop().unwrap_or_default();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!(
                "{text:?}: expected MODS+KEY with a key name such as Z, 1, equal or minus"
            ));
        }
        if !is_key_name(key) {
            return Err(format!(
                "{text:?}: {key:?} is not an xkb key name such as Z, equal, minus or Escape"
            ));
        }
        let mut mods = Vec::new();
        for part in parts {
            let modifier = part.to_ascii_uppercase();
            if !MODIFIERS.contains(&modifier.as_str()) {
                return Err(format!(
                    "{text:?}: unknown modifier {part:?}, expected one of {}",
                    MODIFIERS.join(", ")
                ));
            }
            // One spelling, so equal chords compare equal.
            mods.push(if modifier == "CONTROL" {
                "CTRL".to_owned()
            } else {
                modifier
            });
        }
        Ok(Self {
            mods: mods.join(" "),
            key: key.to_owned(),
        })
    }
}

impl Shortcut {
    /// Whether this is one of Rnote's tool keys, Ctrl+1 (Brush) to Ctrl+6 (Tools).
    pub fn picks_tool(&self) -> bool {
        self.mods == "CTRL" && matches!(self.key.as_str(), "1" | "2" | "3" | "4" | "5" | "6")
    }
}

impl fmt::Display for Shortcut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for modifier in self.mods.split_whitespace() {
            write!(f, "{modifier}+")?;
        }
        f.write_str(&self.key)
    }
}

impl Config {
    /// An explicit path must exist; the default one falls back to the built-in config.
    pub fn load(explicit: Option<&Path>) -> Result<Self, String> {
        let path = match explicit {
            Some(path) => Some(path.to_owned()),
            None => default_path().filter(|path| path.exists()),
        };
        let (text, source) = match &path {
            Some(path) => {
                let text =
                    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
                (text, path.display().to_string())
            }
            None => (DEFAULT.to_owned(), "built-in defaults".to_owned()),
        };
        Self::parse(&text, source)
    }

    fn parse(text: &str, source: String) -> Result<Self, String> {
        let mut config: Self = toml::from_str(text).map_err(|e| format!("{source}: {e}"))?;
        if !(1..=36).contains(&config.ring.step) {
            return Err(format!("{source}: ring.step must be between 1 and 36"));
        }
        if config.dial.step == 0 {
            return Err(format!("{source}: dial.step must be at least 1"));
        }
        config.source = source;
        Ok(config)
    }
}

fn enabled() -> bool {
    true
}

fn default_path() -> Option<PathBuf> {
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("rnote-pad/config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shortcut(text: &str) -> Result<Shortcut, String> {
        Shortcut::try_from(text.to_owned())
    }

    #[test]
    fn default_config_parses() {
        let config = Config::parse(DEFAULT, "default".into()).unwrap();
        assert_eq!(config.window_class, "com.github.flxzt.rnote");
        assert_eq!(
            config.buttons.get(4),
            Some(&shortcut("CTRL+SHIFT+Z").unwrap())
        );
        assert!(config.buttons.get(7).is_none());
        assert_eq!(config.dial.button(3), Some(&shortcut("CTRL+Z").unwrap()));
        assert!(config.dial.button(5).is_none());
        assert_eq!(
            config.pen.lower,
            Some(Action::Switch(
                shortcut("CTRL+4").unwrap(),
                shortcut("CTRL+1").unwrap()
            ))
        );
        assert_eq!(
            config.pen.upper,
            Some(Action::Send(shortcut("CTRL+5").unwrap()))
        );
    }

    #[test]
    fn pen_actions() {
        let pen = |line: &str| {
            Config::parse(
                &format!("window_class = \"x\"\n[pen]\n{line}"),
                "test".into(),
            )
        };
        assert!(pen("lower = [\"CTRL+4\"]").is_err());
        assert!(pen("lower = [\"CTRL+4\", \"CTRL+1\", \"CTRL+2\"]").is_err());
        assert!(pen("lower = [\"CTRL+4\", \"CTRL+NotAKey\"]").is_err());
        assert!(pen("lower = 4").is_err());
        assert!(pen("middle = \"CTRL+4\"").is_err());
    }

    #[test]
    fn tool_keys() {
        assert!(shortcut("CTRL+4").unwrap().picks_tool());
        assert!(shortcut("control+6").unwrap().picks_tool());
        assert!(!shortcut("CTRL+7").unwrap().picks_tool());
        assert!(!shortcut("CTRL+SHIFT+1").unwrap().picks_tool());
        assert!(!shortcut("CTRL+Z").unwrap().picks_tool());
    }

    #[test]
    fn shortcuts() {
        assert_eq!(
            shortcut("ctrl + shift+Z").unwrap(),
            Shortcut {
                mods: "CTRL SHIFT".into(),
                key: "Z".into()
            }
        );
        assert_eq!(shortcut("Escape").unwrap().mods, "");
        assert_eq!(shortcut("CTRL+equal").unwrap().to_string(), "CTRL+equal");
        assert!(shortcut("CTRL+").is_err());
        assert!(shortcut("CTRL+NotAKey").is_err());
        assert!(shortcut("HYPER+Z").is_err());
        assert!(shortcut("CTRL+\"").is_err());
    }

    #[test]
    fn rejects_unknown_buttons() {
        assert!(
            Config::parse(
                "window_class = \"x\"\n[buttons]\nkey7 = \"Z\"",
                "test".into()
            )
            .is_err()
        );
    }
}
