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
use tauri::AppHandle;
#[cfg(windows)]
use tauri::Emitter;

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
    /// Either Ctrl: held + p = keep previews on; other Ctrl combos pass through.
    Ctrl,
    /// Left Alt: held + j/k = down/up.
    Alt,
    /// Hold Shift to inspect previews without immediately following a hint.
    Preview,
    /// `\`: hint for this window.
    HintWindow,
    /// `|`: saved hint for every window of the app.
    HintApp,
    /// `/`: hint mode -> search mode.
    Slash,
    /// Lowercase letter/digit, or ' ', '-', '.'.
    Char(char),
    /// A shifted number-row symbol, used as a digit only while previewing hints.
    ShiftDigit(char),
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
            "Control" => Key::Ctrl,
            "Shift" => Key::Preview,
            "\\" => Key::HintWindow,
            "|" => Key::HintApp,
            "/" => Key::Slash,
            _ => {
                let mut cs = key.chars();
                match (cs.next(), cs.next()) {
                    (Some(c), None) if ")!@#$%^&*(".contains(c) => {
                        Key::ShiftDigit((b'0' + ")!@#$%^&*(".find(c).unwrap() as u8) as char)
                    }
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
pub(crate) enum Scope {
    Window,
    App,
}

/// The hint dialog, for one window (`\`) or every window of its app (`|`).
#[derive(Clone, Serialize)]
pub(crate) struct Edit {
    pub(crate) scope: Scope,
    pub(crate) id: WinId,
    pub(crate) app: String,
    pub(crate) title: String,
    pub(crate) current: String,
    pub(crate) kind: Kind,
    pub(crate) keys: String,
    pub(crate) error: Option<String>,
}

struct State {
    open: bool,
    activate_held: bool,
    alt_held: bool,
    alt_swallowed: bool,
    preview_held: bool,
    /// Right Shift is down (so key repeat doesn't keep toggling search).
    search_held: bool,
    ctrl_held: bool,
    /// Ctrl+P: previews stay on without holding Shift, in every mode, until toggled off.
    previews_pinned: bool,
    typed: String,
    selected: Option<WinId>,
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
    preview_held: false,
    search_held: false,
    ctrl_held: false,
    previews_pinned: false,
    typed: String::new(),
    selected: None,
    wins: Vec::new(),
    search: None,
    edit: None,
    custom: BTreeMap::new(),
    app_hints: BTreeMap::new(),
});
static APP: OnceLock<AppHandle> = OnceLock::new();
static OVERLAY: OnceLock<platform::Overlay> = OnceLock::new();

pub fn init(app: AppHandle, overlay: platform::Overlay) {
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
pub(crate) struct Row<'a> {
    #[serde(flatten)]
    pub(crate) win: &'a Win,
    pub(crate) hits: &'a [usize],
}

#[derive(Clone, Serialize)]
pub(crate) struct SearchView<'a> {
    pub(crate) query: &'a str,
    pub(crate) sel: usize,
}

#[derive(Clone, Serialize)]
pub(crate) struct View<'a> {
    pub(crate) rows: Vec<Row<'a>>,
    pub(crate) typed: &'a str,
    pub(crate) search: Option<SearchView<'a>>,
    pub(crate) edit: Option<&'a Edit>,
    /// True when the overlay has keyboard focus and the webview must forward keys.
    pub(crate) dom_input: bool,
    /// Shift is held (hints wait for its release).
    pub(crate) preview: bool,
    /// Ctrl+P has previews on regardless of Shift.
    pub(crate) pinned: bool,
    pub(crate) selected: Option<WinId>,
}

fn emit(s: &State) {
    let Some(_app) = APP.get() else { return };
    let (rows, search) = match &s.search {
        Some(q) => (
            q.hits.iter().map(|(i, h)| Row { win: &s.wins[*i], hits: h }).collect(),
            Some(SearchView { query: &q.query, sel: q.sel }),
        ),
        None => (s.wins.iter().filter(|w| !s.preview_held || w.key.starts_with(&s.typed)).map(|w| Row { win: w, hits: &[] }).collect(), None),
    };
    let selected = s.search.as_ref().map_or_else(|| selected_hint(s), |q| q.hits.get(q.sel).map(|(i, _)| s.wins[*i].id));
    let view = View { rows, typed: &s.typed, search, edit: s.edit.as_ref(), dom_input: !platform::GLOBAL_INPUT, preview: s.preview_held, pinned: s.previews_pinned, selected };
    #[cfg(windows)]
    let _ = _app.emit("view", view);
    #[cfg(target_os = "linux")]
    platform::render(&view);
}

/// Replay state once the webview has installed its listener (including cold starts).
pub fn refresh() {
    emit(&STATE.lock().unwrap());
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
    s.selected = None;
    s.search = None;
    s.edit = None;
    s.open = true;
    emit(s);
    if let Some(w) = OVERLAY.get() {
        platform::show(w);
    }
}

/// An empty query lists every window in recency order, so start on the previous one like
/// hint mode does (selected_hint); a real query starts on its best match.
fn first_sel(query: &str, hits: usize) -> usize {
    usize::from(query.is_empty() && hits > 1)
}

fn open_search(s: &mut State) {
    if !s.open {
        open(s);
    }
    let hits = rank(&s.wins, "");
    s.search = Some(Search { query: String::new(), sel: first_sel("", hits.len()), hits });
    emit(s);
}

fn close(s: &mut State) {
    s.open = false;
    s.preview_held = false;
    if !platform::GLOBAL_INPUT {
        s.alt_held = false;
        s.alt_swallowed = false;
        s.ctrl_held = false;
    }
    s.typed.clear();
    s.selected = None;
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
    let mut s = STATE.lock().unwrap();
    if s.open { close(&mut s) }
}
pub fn activate(id: WinId) {
    switch_to(&mut STATE.lock().unwrap(), id);
}

// ---- keys ------------------------------------------------------------------------

/// Feed one key event. Returns true if keytab used it (a global hook should swallow it).
pub fn on_key(key: Key, down: bool) -> bool {
    let mut s = STATE.lock().unwrap();
    match key {
        Key::Preview => {
            if !s.open { return false; }
            let was_held = std::mem::replace(&mut s.preview_held, down);
            if was_held && !down && s.search.is_none() && s.edit.is_none() {
                if let Some(id) = unique_hint(&s.wins, &s.typed) {
                    switch_to(&mut s, id);
                    return true;
                }
            }
            if was_held != down { emit(&s); }
            return true;
        }
        Key::Activate => {
            if down && !s.activate_held {
                if s.open { close(&mut s) } else { open(&mut s) }
            }
            s.activate_held = down;
            return true;
        }
        // With Activate held, Right Shift opens straight into search. On its own, while
        // open, it flips between hint mode and search mode.
        Key::Search => {
            let was_held = std::mem::replace(&mut s.search_held, down);
            let pressed = down && !was_held;
            if !pressed {
                return s.open;
            }
            if s.activate_held || (s.open && s.search.is_none() && s.edit.is_none()) {
                open_search(&mut s);
            } else if s.open && s.edit.is_none() {
                s.search = None;
                s.typed.clear();
                s.selected = None;
                emit(&s);
            }
            return s.open;
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
        Key::Ctrl => {
            s.ctrl_held = down;
            return false;
        }
        _ => {}
    }
    if !s.open || !down {
        return false;
    }
    if s.ctrl_held {
        if key == Key::Char('p') && s.edit.is_none() {
            s.previews_pinned = !s.previews_pinned;
            emit(&s);
            return true;
        }
        return false;
    }
    let key = match key {
        Key::ShiftDigit(c) if s.preview_held && s.search.is_none() => Key::Char(c),
        Key::ShiftDigit(_) => Key::Other,
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
        Key::Up | Key::Down => {
            let matches: Vec<_> = s.wins.iter().filter(|w| w.key.starts_with(&s.typed)).map(|w| w.id).collect();
            let current = selected_hint(&s).and_then(|id| matches.iter().position(|m| *m == id)).unwrap_or(0);
            let next = if key == Key::Up { current.saturating_sub(1) } else { (current + 1).min(matches.len().saturating_sub(1)) };
            s.selected = matches.get(next).copied();
            emit(&s);
        }
        Key::Enter => {
            if let Some(id) = selected_hint(&s) { switch_to(&mut s, id); }
        }
        Key::Back => {
            s.typed.pop();
            s.selected = None;
            emit(&s);
        }
        Key::Delete => {
            if let Some(id) = selected_hint(&s) {
                platform::close(id);
                s.wins.retain(|w| w.id != id);
                s.typed.clear();
                s.selected = None;
                if s.wins.is_empty() { close(&mut s) } else { emit(&s) }
            }
        }
        Key::Char(c) if c.is_ascii_alphanumeric() => {
            let typed = format!("{}{c}", s.typed);
            let hits: Vec<_> = s.wins.iter().filter(|w| w.key.starts_with(&typed)).collect();
            match hits.as_slice() {
                [] => {} // no window has this key: ignore the press
                [w] if w.key == typed && !s.preview_held => {
                    let id = w.id;
                    switch_to(&mut s, id);
                }
                _ => {
                    s.typed = typed;
                    s.selected = None;
                    emit(&s);
                }
            }
        }
        // Edit the selected window among those still matching the hint prefix.
        Key::Slash => open_search(&mut s),
        k if k.hint_scope().is_some() => {
            if let Some(i) = selected_hint(&s).and_then(|id| s.wins.iter().position(|w| w.id == id)) {
                start_edit(&mut s, i, k.hint_scope().unwrap());
                emit(&s);
            }
        }
        _ => return false, // let other keys through untouched
    }
    true
}

fn selected_hint(s: &State) -> Option<WinId> {
    let matches: Vec<WinId> = s.wins.iter().filter(|w| w.key.starts_with(&s.typed)).map(|w| w.id).collect();
    if let Some(id) = s.selected.filter(|id| matches.contains(id)) {
        return Some(id);
    }
    // Nothing typed: the previous window, like Alt-Tab (the first row is the current one).
    matches.get(if s.typed.is_empty() { 1 } else { 0 }).or(matches.first()).copied()
}

fn unique_hint(wins: &[Win], typed: &str) -> Option<WinId> {
    let mut hits = wins.iter().filter(|w| w.key.starts_with(typed));
    let id = hits.next()?.id;
    hits.next().is_none().then_some(id)
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
            q.sel = first_sel("", q.hits.len());
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
            q.sel = first_sel(&q.query, q.hits.len());
        }
        Key::Char(c) => {
            q.query.push(c);
            q.hits = rank(&s.wins, &q.query);
            q.sel = 0;
        }
        Key::Slash => return true,
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
    static STATE_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
        assert_eq!(Key::from_dom("/"), Key::Slash);
        assert_eq!(Key::from_dom("\\"), Key::HintWindow);
        assert_eq!(Key::from_dom("Shift"), Key::Preview);
        assert_eq!(Key::from_dom("é"), Key::Other);
        for (symbol, digit) in ")!@#$%^&*(".chars().zip('0'..='9') {
            assert_eq!(Key::from_dom(&symbol.to_string()), Key::ShiftDigit(digit));
        }
    }

    #[test]
    fn normal_arrows_select_only_matching_hints_and_clamp_at_the_ends() {
        let _guard = STATE_TEST.lock().unwrap();
        let mut s = super::STATE.lock().unwrap();
        super::close(&mut s);
        s.open = true;
        s.wins = vec![Win::new(1, "Code".into(), "Editor".into()), Win::new(2, "Console".into(), "Shell".into()), Win::new(3, "Discord".into(), "Chat".into())];
        for (win, hint) in s.wins.iter_mut().zip(["ca", "co", "d"]) { win.key = hint.into(); }
        assert_eq!(super::selected_hint(&s), Some(2), "nothing typed selects the previous window");
        drop(s);
        super::on_key(Key::Down, true);
        assert_eq!(super::selected_hint(&super::STATE.lock().unwrap()), Some(3));
        super::on_key(Key::Char('c'), true);
        assert_eq!(super::selected_hint(&super::STATE.lock().unwrap()), Some(1));
        super::on_key(Key::Down, true);
        super::on_key(Key::Down, true);
        assert_eq!(super::selected_hint(&super::STATE.lock().unwrap()), Some(2));
        super::on_key(Key::Up, true);
        super::on_key(Key::Up, true);
        assert_eq!(super::selected_hint(&super::STATE.lock().unwrap()), Some(1));
        super::on_key(Key::Back, true);
        let mut s = super::STATE.lock().unwrap();
        assert_eq!(s.typed, "");
        assert_eq!(super::selected_hint(&s), Some(2));
        s.wins.clear();
        drop(s);
        super::on_key(Key::Down, true);
        let mut s = super::STATE.lock().unwrap();
        assert_eq!(super::selected_hint(&s), None);
        super::close(&mut s);
    }

    #[test]
    fn shift_previews_defer_hints_and_backspace_restores_the_prefix() {
        let _guard = STATE_TEST.lock().unwrap();
        let mut s = super::STATE.lock().unwrap();
        super::close(&mut s);
        s.open = true;
        s.wins = vec![Win::new(1, "Code".into(), "Editor".into()), Win::new(2, "Console".into(), "Terminal".into())];
        s.wins[0].key = "c2".into();
        s.wins[1].key = "co".into();
        drop(s);
        super::on_key(Key::Preview, true);
        super::on_key(Key::Char('c'), true);
        super::on_key(Key::Char('c'), false);
        assert!(super::STATE.lock().unwrap().preview_held);
        assert_eq!(super::unique_hint(&super::STATE.lock().unwrap().wins, "c"), None);
        super::on_key(Key::from_dom("@"), true);
        {
            let s = super::STATE.lock().unwrap();
            assert!(s.open);
            assert_eq!(s.typed, "c2");
            assert_eq!(super::unique_hint(&s.wins, &s.typed), Some(1));
        }
        super::on_key(Key::Back, true);
        assert_eq!(super::STATE.lock().unwrap().typed, "c");
        super::on_key(Key::Char('o'), true);
        {
            let s = super::STATE.lock().unwrap();
            assert!(s.open && s.preview_held);
            assert_eq!(s.typed, "co", "further letters must keep narrowing while Shift is held");
        }
        super::on_key(Key::Back, true);
        super::on_key(Key::Preview, false);
        let mut s = super::STATE.lock().unwrap();
        assert!(s.open, "releasing Shift must not focus an ambiguous hint");
        assert!(!s.preview_held);
        super::open_search(&mut s);
        drop(s);
        super::on_key(Key::Preview, true);
        super::on_key(Key::from_dom("@"), true);
        let mut s = super::STATE.lock().unwrap();
        assert_eq!(s.search.as_ref().unwrap().query, "", "shifted digits must not change search input");
        super::close(&mut s);
    }

    #[test]
    fn slash_opens_search_from_hint_mode() {
        let _guard = STATE_TEST.lock().unwrap();
        let mut s = super::STATE.lock().unwrap();
        super::close(&mut s);
        s.open = true;
        drop(s);
        assert!(super::on_key(Key::Slash, true));
        assert!(super::STATE.lock().unwrap().search.is_some());
        assert!(super::on_key(Key::Slash, true), "swallowed in search too");
        let mut s = super::STATE.lock().unwrap();
        assert_eq!(s.search.as_ref().unwrap().query, "");
        super::close(&mut s);
    }

    #[test]
    fn ctrl_p_pins_previews_and_other_ctrl_keys_pass_through() {
        let _guard = STATE_TEST.lock().unwrap();
        let mut s = super::STATE.lock().unwrap();
        super::close(&mut s);
        s.open = true;
        s.wins = vec![Win::new(1, "Code".into(), "Editor".into()), Win::new(2, "Paint".into(), "Art".into())];
        s.wins[0].key = "c".into();
        s.wins[1].key = "p".into();
        drop(s);
        super::on_key(Key::Ctrl, true);
        assert!(super::on_key(Key::Char('p'), true), "Ctrl+P is ours");
        assert!(!super::on_key(Key::Char('c'), true), "Ctrl+C goes to the app");
        super::on_key(Key::Ctrl, false);
        let mut s = super::STATE.lock().unwrap();
        assert!(s.open && s.previews_pinned, "pinning previews doesn't jump to the P hint");
        s.previews_pinned = false;
        super::close(&mut s);
    }

    #[test]
    fn right_shift_toggles_search_mode() {
        let _guard = STATE_TEST.lock().unwrap();
        let mut s = super::STATE.lock().unwrap();
        super::close(&mut s);
        s.open = true;
        drop(s);
        super::on_key(Key::Search, true);
        super::on_key(Key::Search, true); // key repeat must not toggle back
        assert!(super::STATE.lock().unwrap().search.is_some(), "Right Shift enters search");
        super::on_key(Key::Search, false);
        super::on_key(Key::Search, true);
        super::on_key(Key::Search, false);
        let mut s = super::STATE.lock().unwrap();
        assert!(s.open && s.search.is_none(), "pressing it again goes back to hints");
        assert!(!s.preview_held);
        super::close(&mut s);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn hiding_resets_modifiers_without_a_global_hook() {
        let _guard = STATE_TEST.lock().unwrap();
        let mut s = super::STATE.lock().unwrap();
        s.alt_held = true;
        s.alt_swallowed = true;
        super::close(&mut s);
        assert!(!s.alt_held && !s.alt_swallowed);
    }
}
