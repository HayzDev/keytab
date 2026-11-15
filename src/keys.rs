//! Platform-neutral window model and hint assignment.
use serde::Serialize;
use std::collections::BTreeMap;

/// Opaque per-platform window id (HWND on Windows, niri window id on Linux).
pub type WinId = i64;

#[derive(Clone, Serialize)]
pub struct Win {
    pub id: WinId,
    pub key: String,
    pub app: String,
    pub title: String,
    pub kind: Kind,
}

impl Win {
    pub fn new(id: WinId, app: String, title: String) -> Self {
        Win { id, key: String::new(), app, title, kind: Kind::Auto }
    }
}

/// Where a window's key came from.
#[derive(Clone, Copy, Default, PartialEq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Auto,
    /// `\` hint for this one window (in memory, dies with the window).
    Window,
    /// `|` hint for every window of this app (saved to disk).
    App,
}

pub fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// Fill in `key` for every window. Precedence: per-window hint (`custom`, by id), then
/// per-app hint (`apps`, by lowercase app name), then auto keys around them.
/// An app hint shared by several windows becomes hint+1, hint+2, ... in stable order.
pub fn assign(wins: &mut [Win], custom: &BTreeMap<WinId, String>, apps: &BTreeMap<String, String>) {
    // Keys are assigned in a stable order (app name, then id) so they don't reshuffle
    // every time focus order changes. Display order stays MRU.
    let mut order: Vec<usize> = (0..wins.len()).collect();
    order.sort_by_key(|&i| (wins[i].app.to_lowercase(), wins[i].id));
    let names: Vec<String> = order.iter().map(|&i| wins[i].app.to_lowercase()).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let shares = |app: &str| {
        (0..names.len()).filter(|&p| names[p] == app && !custom.contains_key(&wins[order[p]].id)).count()
    };
    let mut nth: BTreeMap<&str, usize> = BTreeMap::new();
    let fixed: Vec<Option<(String, Kind)>> = order
        .iter()
        .zip(&names)
        .map(|(&i, &app)| {
            if let Some(k) = custom.get(&wins[i].id) {
                return Some((k.clone(), Kind::Window));
            }
            let hint = apps.get(app)?;
            let n = nth.entry(app).or_default();
            *n += 1;
            Some((if shares(app) == 1 { hint.clone() } else { format!("{hint}{n}") }, Kind::App))
        })
        .collect();
    let keys: Vec<Option<&str>> = fixed.iter().map(|f| f.as_ref().map(|(k, _)| k.as_str())).collect();
    for ((key, &i), f) in assign_keys(&names, &keys).into_iter().zip(&order).zip(&fixed) {
        wins[i].key = key;
        wins[i].kind = f.as_ref().map_or(Kind::Auto, |(_, kind)| *kind);
    }
}

/// `fixed` keys (user hints) are kept as-is. Auto keys: one letter if the first letter is
/// unique; otherwise first letter + next letter of the app's own name (chrome+code ->
/// "ch","co") so they stay guessable. The result is prefix-free, so typing a full key is
/// never ambiguous. `names` must be lowercase and in the stable assignment order.
pub fn assign_keys(names: &[&str], fixed: &[Option<&str>]) -> Vec<String> {
    const ALNUM: &str = "abcdefghijklmnopqrstuvwxyz0123456789";
    let first = |n: &str| n.chars().find(|c| c.is_ascii_alphanumeric());
    let clash = |used: &[String], k: &str| used.iter().any(|u| u.starts_with(k) || k.starts_with(u.as_str()));

    let mut keys: Vec<String> = fixed.iter().map(|f| f.unwrap_or_default().to_string()).collect();
    let mut used: Vec<String> = keys.iter().filter(|k| !k.is_empty()).cloned().collect();
    let auto: Vec<usize> = (0..names.len()).filter(|&i| fixed[i].is_none()).collect();

    for &i in &auto {
        let Some(f) = first(names[i]) else { continue };
        let k = f.to_string();
        if auto.iter().filter(|&&j| first(names[j]) == Some(f)).count() == 1 && !clash(&used, &k) {
            used.push(k.clone());
            keys[i] = k;
        }
    }
    for &i in &auto {
        let Some(f) = first(names[i]).filter(|_| keys[i].is_empty()) else { continue };
        let own: Vec<char> =
            names[i].chars().skip_while(|&c| c != f).skip(1).filter(|c| c.is_ascii_alphanumeric()).collect();
        let pairs = own.iter().copied().chain(ALNUM.chars()).map(|c| format!("{f}{c}"));
        let singles = own.iter().copied().chain(ALNUM.chars()).map(String::from);
        let any = ALNUM.chars().flat_map(|a| ALNUM.chars().map(move |b| format!("{a}{b}")));
        if let Some(k) = pairs.chain(singles).chain(any).find(|k| !clash(&used, k)) {
            used.push(k.clone());
            keys[i] = k;
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::{assign, assign_keys, Kind, Win};
    use std::collections::BTreeMap;

    fn auto(names: &[&str]) -> Vec<String> {
        assign_keys(names, &vec![None; names.len()])
    }

    #[test]
    fn keys() {
        assert_eq!(auto(&["slack"]), ["s"]);
        assert_eq!(auto(&["chrome", "chrome", "code", "slack"]), ["ch", "cr", "co", "s"]);
        assert_eq!(auto(&["aa", "aa", "aa"]), ["aa", "ab", "ac"]);
        assert_eq!(auto(&["", "-x"]), ["", "x"]);
    }

    #[test]
    fn custom_keys() {
        // Custom hint frees the "c" group: chrome drops back to a single letter.
        assert_eq!(assign_keys(&["chrome", "code", "slack"], &[None, Some("x"), None]), ["c", "x", "s"]);
        // Custom "s" blocks "s" and every "s?" pair, so slack falls back to a letter of its name.
        assert_eq!(assign_keys(&["slack", "spotify"], &[None, Some("s")]), ["l", "s"]);
        // Custom "ch" forces the other c-window off the bare "c" (prefix-free).
        assert_eq!(assign_keys(&["chrome", "code"], &[Some("ch"), None]), ["ch", "co"]);
    }

    #[test]
    fn app_hints() {
        let w = |id, app: &str| Win::new(id, app.into(), String::new());
        let apps = BTreeMap::from([("discord".to_string(), "x".to_string())]);
        let keys = |ws: &[Win]| ws.iter().map(|w| (w.key.clone(), w.kind)).collect::<Vec<_>>();

        let mut ws = [w(1, "Discord"), w(2, "Discord"), w(3, "Chrome")];
        assign(&mut ws, &BTreeMap::new(), &apps);
        assert_eq!(keys(&ws), [("x1".into(), Kind::App), ("x2".into(), Kind::App), ("c".into(), Kind::Auto)]);

        // A per-window hint wins over the app hint; the one remaining Discord gets plain "x".
        assign(&mut ws, &BTreeMap::from([(2, "dd".to_string())]), &apps);
        assert_eq!(keys(&ws), [("x".into(), Kind::App), ("dd".into(), Kind::Window), ("c".into(), Kind::Auto)]);
    }
}
