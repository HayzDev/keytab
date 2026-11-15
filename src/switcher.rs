//! Platform-neutral core: the key state machine, fuzzy search, hint dialogs and the
//! saved app hints. Input arrives as abstract [`Key`]s, from a global hook (Windows) or
//! from the focused webview (Wayland/niri). All OS calls go through `crate::platform`.
use crate::keys::{self, Kind, Win, WinId};
use crate::platform;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter, WebviewWindow};

const MAX_HINT: usize = 4;

/// Keyboard input, already translated from the platform's key codes.
#[derive(Clone, Copy, PartialEq, Debug)]
#[cfg_attr(not(windows), allow(dead_code))] // Activate/Search come only from global hooks
pub enum Key {
    /// Open/close the switcher (Right Alt on Windows).
    Activate,
    /// Pressed while Activate is held: open in search mode (Right Shift on Windows).
    Search,
    /// Esc or CapsLock.
    Esc,
    Back,
    Enter,
    Up,
    Down,
    Delete,
    /// Left Alt: held + j/k = down/up.
    Alt,
    /// `\`: hint for this window.
    HintWindow,
    /// `|`: saved hint for every window of the app.
    HintApp,
    /// Lowercase letter/digit, or ' ', '-', '.'.
    Char(char),
    Other,
}

impl Key {
    /// From a DOM `KeyboardEvent.key`, for platforms where the overlay has real focus.
    pub fn from_dom(key: &str) -> Key {
        match key {
            "Escape" | "CapsLock" => Key::Esc,
            "Backspace" => Key::Back,
            "Enter" => Key::Enter,
            "ArrowUp" => Key::Up,
            "ArrowDown" => Key::Down,
            "Delete" => Key::Delete,
            "Alt" => Key::Alt,
            "\\" => Key::HintWindow,
            "|" => Key::HintApp,
            _ => {
                let mut cs = key.chars();
                match (cs.next(), cs.next()) {
                    (Some(c), None) if c.is_ascii_alphanumeric() || " -.".contains(c) => {
                        Key::Char(c.to_ascii_lowercase())
                    }
                    _ => Key::Other,
                }
            }
        }
    }

    fn hint_scope(self) -> Option<Scope> {
        match self {
            Key::HintWindow => Some(Scope::Window),
            Key::HintApp => Some(Scope::App),
            _ => None,
        }
    }
}

struct Search {
    query: String,
    sel: usize,
    /// (index into State::wins, matched char indices in `haystack`), best first.
    hits: Vec<(usize, Vec<usize>)>,
}

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Scope {
    Window,
    App,
}

/// The hint dialog, for one window (`\`) or every window of its app (`|`).
#[derive(Clone, Serialize)]
struct Edit {
    scope: Scope,
    id: WinId,
    app: String,
    title: String,
    current: String,
    kind: Kind,
    keys: String,
    error: Option<String>,
}

struct State {
    open: bool,
    activate_held: bool,
    alt_held: bool,
    alt_swallowed: bool,
    typed: String,
    wins: Vec<Win>,
    search: Option<Search>,
    edit: Option<Edit>,
    /// User-chosen hints per window. Lives as long as the window does.
    custom: BTreeMap<WinId, String>,
    /// User-chosen hints per app (lowercase app name). Saved to disk.
    app_hints: BTreeMap<String, String>,
}

static STATE: Mutex<State> = Mutex::new(State {
    open: false,
    activate_held: false,
    alt_held: false,
    alt_swallowed: false,
    typed: String::new(),
    wins: Vec::new(),
    search: None,
    edit: None,
    custom: BTreeMap::new(),
    app_hints: BTreeMap::new(),
});
static APP: OnceLock<AppHandle> = OnceLock::new();
static OVERLAY: OnceLock<WebviewWindow> = OnceLock::new();

pub fn init(app: AppHandle, overlay: WebviewWindow) {
    STATE.lock().unwrap().app_hints = load_app_hints();
    let _ = APP.set(app);
    let _ = OVERLAY.set(overlay);
}

// ---- saved app hints -------------------------------------------------------------

/// %APPDATA%\keytab on Windows, $XDG_CONFIG_HOME/keytab or ~/.config/keytab elsewhere.
fn hints_path() -> PathBuf {
    let env = |k| std::env::var_os(k).map(PathBuf::from);
    let base = env("APPDATA")
        .or_else(|| env("XDG_CONFIG_HOME"))
        .or_else(|| env("HOME").map(|h| h.join(".config")))
        .unwrap_or_default();
    base.join("keytab").join("app-hints.json")
}

fn load_app_hints() -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(hints_path()) else { return BTreeMap::new() };
    serde_json::from_str(&text).unwrap_or_else(|e| {
        // Keep the unreadable file instead of overwriting it on the next save.
        let _ = std::fs::rename(hints_path(), hints_path().with_extension("json.bad"));
        eprintln!("keytab: ignoring corrupt {}: {e}", hints_path().display());
        BTreeMap::new()
    })
}

fn save_app_hints(hints: &BTreeMap<String, String>) {
    let path = hints_path();
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    if let Err(e) = std::fs::write(&path, serde_json::to_string_pretty(hints).unwrap()) {
        eprintln!("keytab: couldn't save {}: {e}", path.display());
    }
}

// ---- view ------------------------------------------------------------------------

#[derive(Clone, Serialize)]
struct Row<'a> {
    #[serde(flatten)]
    win: &'a Win,
    hits: &'a [usize],
}

#[derive(Clone, Serialize)]
struct SearchView<'a> {
    query: &'a str,
    sel: usize,
}

#[derive(Clone, Serialize)]
struct View<'a> {
    rows: Vec<Row<'a>>,
    typed: &'a str,
    search: Option<SearchView<'a>>,
    edit: Option<&'a Edit>,
    /// True when the overlay has keyboard focus and the webview must forward keys.
    dom_input: bool,
}

fn emit(s: &State) {
    let Some(app) = APP.get() else { return };
    let (rows, search) = match &s.search {
        Some(q) => (
            q.hits.iter().map(|(i, h)| Row { win: &s.wins[*i], hits: h }).collect(),
            Some(SearchView { query: &q.query, sel: q.sel }),
        ),
        None => (s.wins.iter().map(|w| Row { win: w, hits: &[] }).collect(), None),
    };
    let view = View { rows, typed: &s.typed, search, edit: s.edit.as_ref(), dom_input: !platform::GLOBAL_INPUT };
    let _ = app.emit("view", view);
}

// ---- search ----------------------------------------------------------------------

/// What search matches against. The UI splits it back into app / title for highlighting.
fn haystack(w: &Win) -> String {
    if w.title == w.app { w.app.clone() } else { format!("{} {}", w.app, w.title) }
}

/// fzf-style: space-separated terms must all fuzzy-match; scores add up.
/// Ties keep MRU order (stable sort). Empty query lists everything.
fn rank(wins: &[Win], query: &str) -> Vec<(usize, Vec<usize>)> {
    let m = SkimMatcherV2::default();
    let mut hits: Vec<(i64, usize, Vec<usize>)> = wins
        .iter()
        .enumerate()
        .filter_map(|(i, w)| {
            let hay = haystack(w);
            let (mut score, mut idx) = (0, Vec::new());
            for term in query.split_whitespace() {
                let (s, ix) = m.fuzzy_indices(&hay, term)?;
                score += s;
                idx.extend(ix);
            }
            Some((score, i, idx))
        })
        .collect();
    hits.sort_by_key(|h| std::cmp::Reverse(h.0));
    hits.into_iter().map(|(_, i, ix)| (i, ix)).collect()
}

// ---- open / close / switch -------------------------------------------------------

fn open(s: &mut State) {
    s.custom.retain(|&id, _| platform::alive(id));
    s.wins = platform::list();
    keys::assign(&mut s.wins, &s.custom, &s.app_hints);
    s.typed.clear();
    s.search = None;
    s.edit = None;
    s.open = true;
    emit(s);
    if let Some(w) = OVERLAY.get() {
        platform::show(w);
    }
}

fn open_search(s: &mut State) {
    if !s.open {
        open(s);
    }
    let hits = rank(&s.wins, "");
    s.search = Some(Search { query: String::new(), sel: 0, hits });
    emit(s);
}

fn close(s: &mut State) {
    s.open = false;
    s.typed.clear();
    s.search = None;
    s.edit = None;
    s.wins.clear();
    if let Some(w) = OVERLAY.get() {
        platform::hide(w);
    }
    emit(s);
}

fn switch_to(s: &mut State, id: WinId) {
    close(s);
    // Off the input thread: a Windows hook callback must return fast.
    std::thread::spawn(move || platform::focus(id));
}

/// Entry points for the UI and command line.
pub fn toggle() {
    let mut s = STATE.lock().unwrap();
    if s.open { close(&mut s) } else { open(&mut s) }
}
pub fn search() {
    open_search(&mut STATE.lock().unwrap());
}
pub fn hide() {
    close(&mut STATE.lock().unwrap());
}
pub fn activate(id: WinId) {
    switch_to(&mut STATE.lock().unwrap(), id);
}

// ---- keys ------------------------------------------------------------------------

/// Feed one key event. Returns true if keytab used it (a global hook should swallow it).
pub fn on_key(key: Key, down: bool) -> bool {
    let mut s = STATE.lock().unwrap();
    match key {
        Key::Activate => {
            if down && !s.activate_held {
                if s.open { close(&mut s) } else { open(&mut s) }
            }
            s.activate_held = down;
            return true;
        }
        Key::Search if down && s.activate_held => {
            open_search(&mut s);
            return true;
        }
        // Alt is swallowed while open, else the app behind sees a bare Alt tap (the J/K
        // never reach it) and opens its menu bar. Swallow the release only if we swallowed
        // the press, so an Alt held from before opening isn't left stuck down.
        Key::Alt => {
            if !down {
                s.alt_held = false;
                return std::mem::take(&mut s.alt_swallowed);
            }
            if !s.alt_held {
                s.alt_held = true;
                s.alt_swallowed = s.open;
            }
            return s.alt_swallowed;
        }
        _ => {}
    }
    if !s.open || !down {
        return false;
    }
    let key = match key {
        Key::Char('j') if s.alt_held => Key::Down,
        Key::Char('k') if s.alt_held => Key::Up,
        k => k,
    };
    if s.edit.is_some() {
        return on_edit_key(&mut s, key);
    }
    if s.search.is_some() {
        return on_search_key(&mut s, key);
    }
    match key {
        Key::Esc => close(&mut s),
        Key::Up | Key::Down => {} // no selection in letter mode; swallow so Alt+J/K don't leak
        Key::Back => {
            s.typed.pop();
            emit(&s);
        }
        Key::Char(c) if c.is_ascii_alphanumeric() => {
            let typed = format!("{}{c}", s.typed);
            let hits: Vec<_> = s.wins.iter().filter(|w| w.key.starts_with(&typed)).collect();
            match hits.as_slice() {
                [] => {} // no window has this key: ignore the press
                [w] if w.key == typed => {
                    let id = w.id;
                    switch_to(&mut s, id);
                }
                _ => {
                    s.typed = typed;
                    emit(&s);
                }
            }
        }
        // Top window still matching what's typed so far (first row when nothing typed).
        k if k.hint_scope().is_some() => {
            if let Some(i) = s.wins.iter().position(|w| w.key.starts_with(&s.typed)) {
                start_edit(&mut s, i, k.hint_scope().unwrap());
                emit(&s);
            }
        }
        _ => return false, // let other keys through untouched
    }
    true
}

fn start_edit(s: &mut State, i: usize, scope: Scope) {
    let w = &s.wins[i];
    s.edit = Some(Edit {
        scope,
        id: w.id,
        app: w.app.clone(),
        title: w.title.clone(),
        current: w.key.clone(),
        kind: w.kind,
        keys: String::new(),
        error: None,
    });
}

fn on_search_key(s: &mut State, key: Key) -> bool {
    let q = s.search.as_mut().unwrap();
    match key {
        Key::Esc => {
            close(s);
            return true;
        }
        Key::Enter => {
            if let Some(id) = q.hits.get(q.sel).map(|(i, _)| s.wins[*i].id) {
                switch_to(s, id);
            }
            return true;
        }
        Key::Delete => {
            let Some(&(i, _)) = q.hits.get(q.sel) else { return true };
            platform::close(s.wins[i].id);
            s.wins.remove(i);
            q.query.clear();
            q.hits = rank(&s.wins, "");
            q.sel = 0;
        }
        k if k.hint_scope().is_some() => {
            if let Some(&(i, _)) = q.hits.get(q.sel) {
                start_edit(s, i, k.hint_scope().unwrap());
            }
        }
        Key::Up => q.sel = q.sel.saturating_sub(1),
        Key::Down => q.sel = (q.sel + 1).min(q.hits.len().saturating_sub(1)),
        Key::Back => {
            q.query.pop();
            q.hits = rank(&s.wins, &q.query);
            q.sel = 0;
        }
        Key::Char(c) => {
            q.query.push(c);
            q.hits = rank(&s.wins, &q.query);
            q.sel = 0;
        }
        _ => return false,
    }
    emit(s);
    true
}

/// Hint dialog: type 1-4 letters/digits, Enter saves, Delete resets to the automatic
/// hint, Esc goes back. Saving drops to letter mode so the new hint is visible.
fn on_edit_key(s: &mut State, key: Key) -> bool {
    let e = s.edit.as_mut().unwrap();
    let app = e.app.to_lowercase();
    match key {
        Key::Esc => s.edit = None,
        Key::Up | Key::Down => {}
        Key::Delete => {
            match e.scope {
                Scope::Window => {
                    s.custom.remove(&e.id);
                }
                Scope::App => {
                    s.app_hints.remove(&app);
                    save_app_hints(&s.app_hints);
                }
            }
            finish_edit(s);
        }
        Key::Enter if e.keys.is_empty() => s.edit = None,
        Key::Enter => {
            // User hints must stay prefix-free against each other; auto keys re-flow.
            let wins = &s.wins;
            let name = |id: WinId| wins.iter().find(|w| w.id == id).map_or("another window".into(), |w| w.app.clone());
            let windows = s.custom.iter().filter(|(id, _)| !(e.scope == Scope::Window && **id == e.id));
            let apps = s.app_hints.iter().filter(|(a, _)| !(e.scope == Scope::App && **a == app));
            let clash = windows
                .map(|(id, k)| (name(*id), k.clone()))
                .chain(apps.map(|(a, k)| (format!("every {a} window"), k.clone())))
                .find(|(_, k)| k.starts_with(e.keys.as_str()) || e.keys.starts_with(k.as_str()));
            match (clash, e.scope) {
                (Some((who, k)), _) => e.error = Some(format!("Conflicts with {who} ({})", k.to_uppercase())),
                (None, Scope::Window) => {
                    s.custom.insert(e.id, e.keys.clone());
                    finish_edit(s);
                }
                (None, Scope::App) => {
                    s.app_hints.insert(app, e.keys.clone());
                    save_app_hints(&s.app_hints);
                    finish_edit(s);
                }
            }
        }
        Key::Back => {
            e.keys.pop();
            e.error = None;
        }
        Key::Char(c) if c.is_ascii_alphanumeric() => {
            if e.keys.len() < MAX_HINT {
                e.keys.push(c);
            }
            e.error = None;
        }
        _ => return false,
    }
    emit(s);
    true
}

fn finish_edit(s: &mut State) {
    s.edit = None;
    s.search = None;
    s.typed.clear();
    keys::assign(&mut s.wins, &s.custom, &s.app_hints);
}

#[cfg(test)]
mod tests {
    use super::{rank, Key};
    use crate::keys::Win;

    #[test]
    fn fuzzy() {
        let w = |app: &str, title: &str| Win::new(0, app.into(), title.into());
        let ws = [
            w("Vivaldi", "Adam Basis - YouTube - Vivaldi"),
            w("Discord", "@splep - Discord"),
            w("Spotify", "Spotify Premium"),
            w("Steamwebhelper", "Steam"),
        ];
        let top = |q: &str| rank(&ws, q).first().map(|h| ws[h.0].app.as_str());
        assert_eq!(rank(&ws, "").len(), 4); // empty query = everything, MRU order
        assert_eq!(top("spt"), Some("Spotify"));
        assert_eq!(top("ytube"), Some("Vivaldi")); // matches the title too
        assert_eq!(top("disc splep"), Some("Discord")); // multi-term
        assert!(rank(&ws, "zzz").is_empty());
    }

    #[test]
    fn dom_keys() {
        assert_eq!(Key::from_dom("A"), Key::Char('a'));
        assert_eq!(Key::from_dom("CapsLock"), Key::Esc);
        assert_eq!(Key::from_dom("|"), Key::HintApp);
        assert_eq!(Key::from_dom("\\"), Key::HintWindow);
        assert_eq!(Key::from_dom("Shift"), Key::Other);
        assert_eq!(Key::from_dom("é"), Key::Other);
    }
}
