//! 設定ファイル(`$XDG_CONFIG_HOME/splamouse/config.toml`)の読み込み。
//!
//! ボタンは書いたものだけに操作を割り当て、書かなかったボタンは何もしない。
//! 感度は書かなければ 0.0(標準)。

use anyhow::{Context, Result};
use enigo::{Button, Key};
use serde::{Deserialize, Deserializer, de};
use std::{env, fs, io, path::PathBuf, str::FromStr};

#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub sensitivity: Sensitivity,
    pub buttons: Buttons,
}

/// 感度。-5.0 〜 5.0 で、0.0 が標準。
#[derive(Debug, Default, Clone, Copy, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sensitivity {
    #[serde(deserialize_with = "level")]
    pub gyro: f64,
    #[serde(deserialize_with = "level")]
    pub stick: f64,
}

impl Sensitivity {
    /// 感度を倍率にする。
    pub fn gyro_factor(&self) -> f64 {
        2.0 + self.gyro * 0.2
    }

    pub fn stick_factor(&self) -> f64 {
        2.0 + self.stick * 0.2
    }
}

fn level<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
    let value = f64::deserialize(deserializer)?;
    // NaN もここで弾かれる。
    if (-5.0..=5.0).contains(&value) {
        Ok(value)
    } else {
        Err(de::Error::custom(format!(
            "must be between -5.0 and 5.0, got {value}"
        )))
    }
}

/// 各ボタンの割り当て。書かなかったボタンは何もしない。
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Buttons {
    pub a: Action,
    pub b: Action,
    pub x: Action,
    pub y: Action,
    pub l: Action,
    pub r: Action,
    pub zl: Action,
    pub zr: Action,
    pub minus: Action,
    pub plus: Action,
    pub left: Action,
    pub right: Action,
    pub down: Action,
    pub up: Action,
    pub lstick: Action,
    pub rstick: Action,
    pub capture: Action,
    pub home: Action,
}

/// ボタンに割り当てる振る舞い。
#[derive(Debug, Default, Clone, PartialEq)]
pub enum Action {
    /// 何もしない。
    #[default]
    None,
    /// 押している間キーを押す。押す時は先頭から、離す時は末尾から。
    Keys(Vec<Key>),
    /// 押している間マウスボタンを押す(ドラッグ)。
    Hold(Button),
    /// 離した時にクリックする。
    ClickOnRelease(Button),
    /// ポインタを画面中央へ滑らかに動かす。
    Center,
}

impl Action {
    /// 押している間、`key` を押したままにするか。
    pub fn holds_key(&self, key: Key) -> bool {
        matches!(self, Self::Keys(keys) if keys.contains(&key))
    }

    /// 押している間、`button` を押したままにするか。
    pub fn holds_button(&self, button: Button) -> bool {
        matches!(self, Self::Hold(held) if *held == button)
    }
}

impl FromStr for Action {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim().to_ascii_lowercase();
        Ok(match s.as_str() {
            "" => return Err(r#"empty action (use "none" to disable the button)"#.into()),
            "none" => Action::None,
            "center" => Action::Center,
            _ => {
                if let Some(button) = s.strip_prefix("hold:") {
                    Action::Hold(parse_mouse_button(button)?)
                } else if let Some(button) = s.strip_prefix("click:") {
                    Action::ClickOnRelease(parse_mouse_button(button)?)
                } else {
                    Action::Keys(parse_keys(&s)?)
                }
            }
        })
    }
}

impl<'de> Deserialize<'de> for Action {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

fn parse_mouse_button(name: &str) -> Result<Button, String> {
    Ok(match name.trim() {
        "left" => Button::Left,
        "right" => Button::Right,
        "back" => Button::Back,
        "forward" => Button::Forward,
        _ => {
            return Err(format!(
                "unknown mouse button `{name}` (expected left, right, back or forward)"
            ));
        }
    })
}

fn parse_keys(combo: &str) -> Result<Vec<Key>, String> {
    let mut keys = Vec::new();
    for name in combo.split('+').map(str::trim) {
        let key = parse_key(name).ok_or_else(|| format!("unknown key `{name}` in `{combo}`"))?;
        if keys.contains(&key) {
            return Err(format!("duplicate key `{name}` in `{combo}`"));
        }
        keys.push(key);
    }
    Ok(keys)
}

const FUNCTION_KEYS: [Key; 20] = [
    Key::F1,
    Key::F2,
    Key::F3,
    Key::F4,
    Key::F5,
    Key::F6,
    Key::F7,
    Key::F8,
    Key::F9,
    Key::F10,
    Key::F11,
    Key::F12,
    Key::F13,
    Key::F14,
    Key::F15,
    Key::F16,
    Key::F17,
    Key::F18,
    Key::F19,
    Key::F20,
];

fn parse_key(name: &str) -> Option<Key> {
    Some(match name {
        "cmd" | "command" => Key::Meta,
        "shift" => Key::Shift,
        "ctrl" | "control" => Key::Control,
        "alt" | "opt" | "option" => Key::Alt,
        "left" => Key::LeftArrow,
        "right" => Key::RightArrow,
        "up" => Key::UpArrow,
        "down" => Key::DownArrow,
        "enter" | "return" => Key::Return,
        "tab" => Key::Tab,
        "space" => Key::Space,
        "esc" | "escape" => Key::Escape,
        "backspace" => Key::Backspace,
        "delete" => Key::Delete,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        _ => {
            if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<usize>().ok()) {
                return FUNCTION_KEYS.get(n.checked_sub(1)?).copied();
            }
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Key::Other(ansi_keycode(c)?),
                _ => return None,
            }
        }
    })
}

/// 文字に対応する macOS の仮想キーコード(US 配列での物理位置)。
/// `Key::Unicode` は入力ソースによって解決できないことがあるため、物理キーで送る。
fn ansi_keycode(c: char) -> Option<u32> {
    Some(match c {
        'a' => 0x00,
        's' => 0x01,
        'd' => 0x02,
        'f' => 0x03,
        'h' => 0x04,
        'g' => 0x05,
        'z' => 0x06,
        'x' => 0x07,
        'c' => 0x08,
        'v' => 0x09,
        'b' => 0x0B,
        'q' => 0x0C,
        'w' => 0x0D,
        'e' => 0x0E,
        'r' => 0x0F,
        'y' => 0x10,
        't' => 0x11,
        '1' => 0x12,
        '2' => 0x13,
        '3' => 0x14,
        '4' => 0x15,
        '6' => 0x16,
        '5' => 0x17,
        '=' => 0x18,
        '9' => 0x19,
        '7' => 0x1A,
        '-' => 0x1B,
        '8' => 0x1C,
        '0' => 0x1D,
        ']' => 0x1E,
        'o' => 0x1F,
        'u' => 0x20,
        '[' => 0x21,
        'i' => 0x22,
        'p' => 0x23,
        'l' => 0x25,
        'j' => 0x26,
        '\'' => 0x27,
        'k' => 0x28,
        ';' => 0x29,
        '\\' => 0x2A,
        ',' => 0x2B,
        '/' => 0x2C,
        'n' => 0x2D,
        'm' => 0x2E,
        '.' => 0x2F,
        '`' => 0x32,
        _ => return None,
    })
}

/// 設定されている `XDG_CONFIG_HOME`。XDG Base Directory の仕様どおり、相対パスは無視する。
pub fn xdg_config_home() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
}

/// 設定ファイルのパス。`XDG_CONFIG_HOME` が無い場合は `~/.config` を使う。
pub fn path() -> Option<PathBuf> {
    let base = xdg_config_home().or_else(|| env::home_dir().map(|home| home.join(".config")))?;
    Some(base.join("splamouse").join("config.toml"))
}

pub fn load() -> Result<Config> {
    let Some(path) = path() else {
        eprintln!("No home directory found, so the buttons do nothing");
        return Ok(Config::default());
    };
    match fs::read_to_string(&path) {
        Ok(text) => {
            let config = toml::from_str(&text)
                .with_context(|| format!("error parsing {}", path.display()))?;
            eprintln!("Loaded config from {}", path.display());
            Ok(config)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            eprintln!("No config at {}, so the buttons do nothing", path.display());
            Ok(Config::default())
        }
        Err(e) => Err(e).with_context(|| format!("error reading {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Config, toml::de::Error> {
        toml::from_str(text)
    }

    #[test]
    fn empty_file_is_default() {
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn overrides_only_given_fields() {
        let config = parse(
            r#"
            [sensitivity]
            gyro = -3
            [buttons]
            home = "center"
            a = "none"
            "#,
        )
        .unwrap();
        assert_eq!(config.sensitivity.gyro, -3.0);
        assert_eq!(config.sensitivity.stick, 0.0);
        assert_eq!(config.buttons.home, Action::Center);
        assert_eq!(config.buttons.a, Action::None);
        // 書かなかったボタンは何もしない
        assert_eq!(config.buttons.b, Action::None);
    }

    #[test]
    fn parses_actions() {
        assert_eq!(
            "Cmd + Shift + Z".parse(),
            Ok(Action::Keys(vec![Key::Meta, Key::Shift, Key::Other(0x06)]))
        );
        assert_eq!(
            "cmd+w".parse(),
            Ok(Action::Keys(vec![Key::Meta, Key::Other(0x0D)]))
        );
        assert_eq!(
            "ctrl+tab".parse(),
            Ok(Action::Keys(vec![Key::Control, Key::Tab]))
        );
        assert_eq!("f12".parse(), Ok(Action::Keys(vec![Key::F12])));
        assert_eq!("hold:left".parse(), Ok(Action::Hold(Button::Left)));
        assert_eq!(
            "click:right".parse(),
            Ok(Action::ClickOnRelease(Button::Right))
        );
        assert_eq!("center".parse(), Ok(Action::Center));
    }

    #[test]
    fn rejects_invalid_actions() {
        for s in [
            "", "cmd+", "cmd+foo", "cmd+cmd", "f0", "f21", "hold:foo", "click:",
        ] {
            assert!(s.parse::<Action>().is_err(), "{s:?} should be rejected");
        }
    }

    #[test]
    fn rejects_invalid_config() {
        assert!(parse("[sensitivity]\ngyro = 5.1").is_err());
        assert!(parse("[sensitivity]\nstick = nan").is_err());
        assert!(parse("[buttons]\nsl = \"cmd\"").is_err());
        assert!(parse("[button]\na = \"cmd\"").is_err());
    }

    #[test]
    fn readme_examples_are_valid() {
        for readme in [
            include_str!("../README.md"),
            include_str!("../README-ja.md"),
        ] {
            let blocks: Vec<_> = readme
                .split("```toml\n")
                .skip(1)
                .map(|rest| rest.split("```").next().unwrap())
                .collect();
            assert!(!blocks.is_empty());
            for block in blocks {
                parse(block).unwrap();
                // 例ではすべてのボタンに割り当てている(名前の誤りは parse が弾く)
                let table: toml::Table = toml::from_str(block).unwrap();
                assert_eq!(table["buttons"].as_table().unwrap().len(), 18, "{block}");
            }
        }
    }

    #[test]
    fn error_points_to_the_line() {
        let err = parse("[buttons]\na = \"cmd\"\nb = \"cmd+foo\"").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("line 3"), "{msg}");
        assert!(msg.contains("unknown key `foo`"), "{msg}");
    }
}
