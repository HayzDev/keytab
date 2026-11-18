//! User preferences, saved as `settings.json` next to `app-hints.json`.
//! Platform-neutral; the settings window (Windows) edits them through `main.rs` commands.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::RwLock;

#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Stable,
    Nightly,
}

/// A window kept out of the switcher: every window of `app` or, with a `title`, only the
/// window with exactly that title. Both compare case-insensitively.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hidden {
    /// As the switcher shows it ("Vivaldi").
    pub app: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl Hidden {
    pub fn matches(&self, app: &str, title: &str) -> bool {
        self.app.to_lowercase() == app.to_lowercase()
            && self.title.as_ref().is_none_or(|t| t.to_lowercase() == title.to_lowercase())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// "system", "dark" or "light".
    pub theme: String,
    /// The key that opens the switcher (Windows): "RightAlt", "RightCtrl", "RightWin", "Apps", "CapsLock".
    pub hotkey: String,
    /// Moving the mouse over a row selects it (and previews it).
    pub hover_select: bool,
    /// Del and the row's close button ask before closing a window.
    pub confirm_close: bool,
    /// Previews start pinned on, as if Ctrl+P was pressed.
    pub previews_on: bool,
    pub channel: Channel,
    /// Check for an update when keytab starts and install it.
    pub auto_update: bool,
    /// Windows left out of the switcher (Alt+H or a row's hide button); Settings > Switcher
    /// brings them back.
    pub hidden: Vec<Hidden>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: "system".into(),
            hotkey: "RightAlt".into(),
            hover_select: true,
            confirm_close: true,
            previews_on: false,
            channel: Channel::default(),
            auto_update: true,
            hidden: Vec::new(),
        }
    }
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);

/// %APPDATA%\keytab on Windows, $XDG_CONFIG_HOME/keytab or ~/.config/keytab elsewhere.
pub fn config_dir() -> PathBuf {
    let env = |k| std::env::var_os(k).map(PathBuf::from);
    let base = env("APPDATA")
        .or_else(|| env("XDG_CONFIG_HOME"))
        .or_else(|| env("HOME").map(|h| h.join(".config")))
        .unwrap_or_default();
    base.join("keytab")
}

fn path() -> PathBuf {
    config_dir().join("settings.json")
}

fn load() -> Settings {
    // Tests mustn't depend on the developer's own settings.
    if cfg!(test) {
        return Settings::default();
    }
    let Ok(text) = std::fs::read_to_string(path()) else { return Settings::default() };
    serde_json::from_str(&text).unwrap_or_else(|e| {
        // Keep the unreadable file instead of overwriting it on the next save.
        let _ = std::fs::rename(path(), path().with_extension("json.bad"));
        eprintln!("keytab: ignoring corrupt {}: {e}", path().display());
        Settings::default()
    })
}

pub fn get() -> Settings {
    if let Some(s) = SETTINGS.read().unwrap().as_ref() {
        return s.clone();
    }
    let s = load();
    *SETTINGS.write().unwrap() = Some(s.clone());
    s
}

pub fn set(s: Settings) {
    if cfg!(test) {
        *SETTINGS.write().unwrap() = Some(s);
        return;
    }
    let path = path();
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    if let Err(e) = std::fs::write(&path, serde_json::to_string_pretty(&s).unwrap()) {
        eprintln!("keytab: couldn't save {}: {e}", path.display());
    }
    *SETTINGS.write().unwrap() = Some(s);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_take_defaults() {
        let s: Settings = serde_json::from_str(r#"{"channel":"nightly","hover_select":false}"#).unwrap();
        assert_eq!(s.channel, Channel::Nightly);
        assert!(!s.hover_select);
        assert_eq!(s.hotkey, "RightAlt");
        assert!(s.confirm_close && s.auto_update);
        assert!(s.hidden.is_empty());
    }

    #[test]
    fn hidden_matches_one_window_or_the_whole_app() {
        let app = Hidden { app: "Steam".into(), title: None };
        let one = Hidden { app: "Steam".into(), title: Some("Friends List".into()) };
        assert!(app.matches("steam", "Store") && app.matches("Steam", ""));
        assert!(one.matches("Steam", "friends list") && !one.matches("Steam", "Store"));
        assert!(!app.matches("Steamwebhelper", "Store"));
        assert_eq!(serde_json::to_string(&app).unwrap(), r#"{"app":"Steam"}"#);
    }
}
