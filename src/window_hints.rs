//! Niri window hints belong to one compositor instance, not to a Keytab process.
use crate::keys::WinId;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io::{self, Write}, path::Path};

#[derive(Deserialize, Serialize)]
struct SavedHints {
    session: String,
    hints: BTreeMap<WinId, String>,
}

pub fn load(path: &Path, session: &str) -> io::Result<BTreeMap<WinId, String>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e),
    };
    let mut saved: SavedHints = serde_json::from_slice(&bytes).map_err(|e| {
        let _ = fs::rename(path, path.with_extension("json.bad"));
        io::Error::new(io::ErrorKind::InvalidData, e)
    })?;
    if saved.session != session { return Ok(BTreeMap::new()); }
    saved.hints.retain(|_, hint| !hint.is_empty() && hint.len() <= 4
        && hint.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
    Ok(saved.hints)
}

pub fn save(path: &Path, session: &str, hints: &BTreeMap<WinId, String>) -> io::Result<()> {
    fs::create_dir_all(path.parent().unwrap())?;
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(&SavedHints { session: session.into(), hints: hints.clone() })?;
    let mut file = fs::File::create(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    // A stopped/replaced Keytab process cannot leave the previous cache half-written.
    fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory() -> std::path::PathBuf {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("keytab-window-hints-{}-{unique}", std::process::id()));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn restarts_restore_hints_only_for_the_same_compositor_instance() {
        let dir = directory();
        let path = dir.join("hints.json");
        let mut hints = BTreeMap::from([(7, "z9".into())]);
        save(&path, "boot:niri:start", &hints).unwrap();
        assert_eq!(load(&path, "boot:niri:start").unwrap(), hints);
        assert!(load(&path, "new-boot:niri:start").unwrap().is_empty());
        assert!(load(&path, "boot:niri:new-start").unwrap().is_empty());
        hints.clear();
        save(&path, "boot:niri:start", &hints).unwrap();
        assert!(load(&path, "boot:niri:start").unwrap().is_empty());
        assert!(!path.with_extension("json.tmp").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn invalid_cached_hints_are_not_applied() {
        let dir = directory();
        let path = dir.join("hints.json");
        let hints = BTreeMap::from([(1, "ok1".into()), (2, "".into()), (3, "longer".into()), (4, "a!".into())]);
        save(&path, "session", &hints).unwrap();
        assert_eq!(load(&path, "session").unwrap(), BTreeMap::from([(1, "ok1".into())]));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_damaged_cache_is_kept_as_a_backup() {
        let dir = directory();
        let path = dir.join("hints.json");
        fs::write(&path, b"unfinished json").unwrap();
        assert!(load(&path, "session").is_err());
        assert_eq!(fs::read(path.with_extension("json.bad")).unwrap(), b"unfinished json");
        fs::remove_dir_all(dir).unwrap();
    }
}
