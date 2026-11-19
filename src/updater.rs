//! Updates from GitHub Releases (Windows and Linux). Two channels:
//!
//! - stable: the newest `v*` release that isn't a prerelease, newer than this build.
//! - nightly: the rolling `nightly` prerelease, rebuilt from every push to main. Its notes
//!   carry `Commit: <sha>`; it's an update when that isn't the commit this build came from.
//!
//! Downloads use the system `curl` (Windows' own `curl.exe` on Windows 10 1803+), so there's
//! no TLS stack in the binary.
//!
//! - Windows: runs `keytab-setup.exe /S` once keytab has exited, then restarts it.
//! - Linux: unpacks `keytab-linux-x86_64.tar.gz` and replaces only the `keytab` binary and
//!   the `keytabctl` helper next to the running executable, then restarts keytab. It never
//!   touches the niri config (`keytab.kdl`, keybinds, layer rules): those are the user's.
use crate::settings::Channel;
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use std::path::PathBuf;
use std::process::Command;

const REPO: &str = "HayzDev/keytab";
#[cfg(windows)]
const ASSET: &str = "keytab-setup.exe";
#[cfg(not(windows))]
const ASSET: &str = "keytab-linux-x86_64.tar.gz";
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The channel and commit this binary was built from (set by the release workflow).
/// Local builds are "dev".
pub fn built_channel() -> &'static str {
    option_env!("KEYTAB_CHANNEL").unwrap_or("dev")
}
pub fn built_commit() -> &'static str {
    env!("KEYTAB_COMMIT")
}

#[derive(Clone, Serialize)]
pub struct Info {
    /// "0.1.0", "0.1.0 nightly (abc1234)", "0.1.0 dev".
    pub current: String,
    pub available: Option<Update>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Update {
    /// "v0.2.0" or "nightly (abc1234)".
    pub name: String,
    pub url: String,
    pub notes_url: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    prerelease: bool,
    draft: bool,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

pub fn current() -> String {
    let v = env!("CARGO_PKG_VERSION");
    match built_channel() {
        "stable" => v.into(),
        "nightly" => format!("{v} nightly ({})", short(built_commit())),
        other => format!("{v} {other}"),
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

#[cfg(windows)]
fn curl() -> Command {
    let system = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
    let mut c = Command::new(system.join("System32").join("curl.exe"));
    c.args(["-fsSL", "--proto", "=https", "--max-time", "120", "-A", "keytab-updater"]);
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

#[cfg(not(windows))]
fn curl() -> Command {
    let mut c = Command::new("curl");
    c.args(["-fsSL", "--proto", "=https", "--max-time", "120", "-A", "keytab-updater"]);
    c
}

fn releases() -> Result<Vec<Release>, String> {
    let out = curl()
        .args(["-H", "Accept: application/vnd.github+json"])
        .arg(format!("https://api.github.com/repos/{REPO}/releases?per_page=30"))
        .output()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("couldn't reach GitHub ({})", String::from_utf8_lossy(&out.stderr).trim()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("unexpected answer from GitHub: {e}"))
}

/// "0.10.2" -> [0, 10, 2], for comparing versions.
fn version(s: &str) -> Vec<u64> {
    s.trim_start_matches('v').split(['.', '-', '+']).map_while(|p| p.parse().ok()).collect()
}

/// The release to install on `channel`, if it differs from this build in the right direction.
fn pick(releases: &[Release], channel: Channel, built: &str, commit: &str, ours: &str) -> Option<Update> {
    let asset = |r: &Release| r.assets.iter().find(|a| a.name == ASSET).map(|a| a.browser_download_url.clone());
    match channel {
        Channel::Stable => {
            let r = releases.iter().filter(|r| !r.prerelease && !r.draft && r.tag_name.starts_with('v'))
                .max_by(|a, b| version(&a.tag_name).cmp(&version(&b.tag_name)))?;
            let (theirs, ours) = (version(&r.tag_name), version(ours));
            // Leaving nightly for stable may reinstall the same version number.
            let wanted = theirs > ours || (built == "nightly" && theirs >= ours);
            if !wanted { return None; }
            Some(Update { name: r.tag_name.clone(), url: asset(r)?, notes_url: r.html_url.clone() })
        }
        Channel::Nightly => {
            let r = releases.iter().find(|r| r.tag_name == "nightly" && !r.draft)?;
            let sha = r.body.as_deref().and_then(|b| b.split("Commit: ").nth(1))
                .map(|s| s.split_whitespace().next().unwrap_or("").trim_matches('`').to_string())
                .unwrap_or_default();
            if built == "nightly" && !sha.is_empty() && sha == commit { return None; }
            Some(Update {
                name: if sha.is_empty() { "nightly".into() } else { format!("nightly ({})", short(&sha)) },
                url: asset(r)?,
                notes_url: r.html_url.clone(),
            })
        }
    }
}

/// Ask GitHub whether `channel` has something newer than this build. Blocking.
pub fn check(channel: Channel) -> Result<Info, String> {
    let all = releases()?;
    // "Up to date" would be misleading when the channel has nothing published at all.
    let published = all.iter().any(|r| !r.draft && match channel {
        Channel::Stable => !r.prerelease && r.tag_name.starts_with('v'),
        Channel::Nightly => r.tag_name == "nightly",
    });
    if !published {
        return Err(match channel {
            Channel::Stable => "No stable release has been published yet".into(),
            Channel::Nightly => "No nightly build has been published yet. One appears after the next push to main".into(),
        });
    }
    let available = pick(&all, channel, built_channel(), built_commit(), env!("CARGO_PKG_VERSION"));
    Ok(Info { current: current(), available })
}

/// Download the installer, then hand off to a hidden PowerShell that waits for keytab to
/// exit, installs silently and starts the new keytab. The caller must exit the app.
#[cfg(windows)]
pub fn install(update: &Update) -> Result<(), String> {
    if !update.url.starts_with("https://github.com/") {
        return Err("refusing to download from outside github.com".into());
    }
    let dir = std::env::temp_dir().join("keytab-update");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let setup = dir.join(ASSET);
    let out = curl().arg("-o").arg(&setup).arg(&update.url).output().map_err(|e| format!("couldn't run curl: {e}"))?;
    let size = std::fs::metadata(&setup).map(|m| m.len()).unwrap_or(0);
    if !out.status.success() || size < 100_000 {
        return Err(format!("download failed ({})", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let quote = |p: &std::path::Path| format!("'{}'", p.display().to_string().replace('\'', "''"));
    let script = format!(
        "Wait-Process -Id {pid} -Timeout 20 -ErrorAction SilentlyContinue; \
         Start-Process -FilePath {setup} -ArgumentList '/S' -Wait; \
         Remove-Item -Recurse -Force {dir} -ErrorAction SilentlyContinue; \
         Start-Process -FilePath {exe}",
        pid = std::process::id(),
        setup = quote(&setup),
        dir = quote(&dir),
        exe = quote(&crate::platform::installed_exe()),
    );
    Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-Command", &script])
        // Not DETACHED_PROCESS: PowerShell 5.1 exits without running anything when it has no
        // console, so keytab would quit and never come back. A hidden console is enough.
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("couldn't start the installer: {e}"))?;
    Ok(())
}

/// Linux: the running executable, which the update replaces in place.
#[cfg(not(windows))]
fn running_exe() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("couldn't find keytab's executable: {e}"))?;
    // /proc/self/exe after the file was replaced on disk.
    let text = exe.to_string_lossy();
    Ok(text.strip_suffix(" (deleted)").map_or(exe.clone(), std::path::PathBuf::from))
}

/// Why this executable can't update itself from a release archive, if it can't.
#[cfg(not(windows))]
fn updatable(exe: &std::path::Path) -> Result<(), String> {
    if std::env::consts::ARCH != "x86_64" {
        return Err("Prebuilt Linux releases are x86_64 only. Build keytab from source to update it".into());
    }
    if exe.components().any(|c| c.as_os_str() == "target") {
        return Err(format!("{} is a cargo build. Rebuild it from source to update it", exe.display()));
    }
    let dir = exe.parent().ok_or("keytab's executable has no folder")?;
    let probe = dir.join(format!(".keytab-update-probe-{}", std::process::id()));
    std::fs::write(&probe, b"").map_err(|_| format!(
        "{} isn't writable by you. Update keytab the way you installed it (package manager or install.sh)",
        dir.display()))?;
    let _ = std::fs::remove_file(probe);
    Ok(())
}

/// Download the release archive, put the new `keytab` and `keytabctl` next to the running
/// executable (atomically, so a failure leaves the old ones), then start a detached shell
/// that waits for this keytab to exit and starts the new one. The caller must exit the app.
/// The niri config is never touched, so custom keybinds (F13, Mod+Tab, ...) survive.
#[cfg(not(windows))]
pub fn install(update: &Update) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    if !update.url.starts_with("https://github.com/") {
        return Err("refusing to download from outside github.com".into());
    }
    let exe = running_exe()?;
    updatable(&exe)?;
    let dir = exe.parent().unwrap().to_path_buf();
    let name = exe.file_name().unwrap().to_owned();
    let work = std::env::temp_dir().join(format!("keytab-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let archive = work.join(ASSET);
    let result = curl().arg("-o").arg(&archive).arg(&update.url).output()
        .map_err(|e| format!("couldn't run curl: {e}"))
        .and_then(|out| {
            let size = std::fs::metadata(&archive).map(|m| m.len()).unwrap_or(0);
            if out.status.success() && size >= 100_000 { Ok(()) }
            else { Err(format!("download failed ({})", String::from_utf8_lossy(&out.stderr).trim())) }
        })
        .and_then(|()| replace_from_archive(&archive, &work.join("unpacked"), &dir, &name));
    let _ = std::fs::remove_dir_all(&work);
    result?;
    // Wait (up to 20s) until this process is gone or a zombie: the new keytab needs the
    // single-instance D-Bus name.
    let script = r#"i=0
while [ $i -lt 200 ] && kill -0 "$1" 2>/dev/null; do
    case $(sed 's/.*) //' "/proc/$1/stat" 2>/dev/null | cut -c1) in Z|"") break ;; esac
    sleep 0.1; i=$((i + 1))
done
exec "$2""#;
    Command::new("sh").arg("-c").arg(script).arg("keytab-update")
        .arg(std::process::id().to_string()).arg(dir.join(&name))
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("updated, but couldn't restart keytab ({e}); start it again yourself"))?;
    Ok(())
}

/// Unpack a release archive into `unpacked` and replace `dir/name` (keytab) and
/// `dir/keytabctl` with its files. Both are staged first, so a failure changes nothing.
#[cfg(not(windows))]
fn replace_from_archive(archive: &std::path::Path, unpacked: &std::path::Path, dir: &std::path::Path, name: &std::ffi::OsStr) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(unpacked).map_err(|e| e.to_string())?;
    let out = Command::new("tar").arg("-xzf").arg(archive).arg("-C").arg(unpacked)
        .args(["--strip-components=1", "--no-same-owner"]).output()
        .map_err(|e| format!("couldn't run tar: {e}"))?;
    if !out.status.success() {
        return Err(format!("couldn't unpack the update ({})", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let new_exe = unpacked.join("keytab");
    let new_ctl = unpacked.join("keytabctl");
    let starts = |p: &std::path::Path, magic: &[u8]| std::fs::read(p).is_ok_and(|b| b.starts_with(magic));
    if !starts(&new_exe, b"\x7fELF") || !starts(&new_ctl, b"#!") {
        return Err("the update archive doesn't contain keytab and keytabctl".into());
    }
    // A running binary can be replaced by a rename, which never leaves a half-written file.
    let mut staged = Vec::new();
    for (from, to) in [(new_exe, dir.join(name)), (new_ctl, dir.join("keytabctl"))] {
        let tmp = dir.join(format!(".{}.new", to.file_name().unwrap().to_string_lossy()));
        if let Err(e) = std::fs::copy(&from, &tmp)
            .and_then(|_| std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))) {
            let _ = std::fs::remove_file(&tmp);
            for (t, _) in &staged { let _ = std::fs::remove_file(t); }
            return Err(format!("couldn't write {}: {e}", tmp.display()));
        }
        staged.push((tmp, to));
    }
    for (tmp, to) in &staged {
        std::fs::rename(tmp, to).map_err(|e| format!("couldn't replace {}: {e}", to.display()))?;
    }
    Ok(())
}

/// Whether the startup check may install updates by itself. Debug builds never do; on Linux
/// neither do builds from source ("dev"), so a local build isn't swapped for a release.
pub fn auto_allowed() -> bool {
    !cfg!(debug_assertions) && (cfg!(windows) || built_channel() != "dev")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(tag: &str, pre: bool, body: &str) -> Release {
        Release {
            tag_name: tag.into(),
            prerelease: pre,
            draft: false,
            body: Some(body.into()),
            html_url: format!("https://github.com/{REPO}/releases/tag/{tag}"),
            assets: vec![Asset { name: ASSET.into(), browser_download_url: format!("https://github.com/x/{tag}/{ASSET}") }],
        }
    }

    #[test]
    fn stable_offers_only_newer_versions() {
        let rs = [rel("nightly", true, "Commit: abcdef1234"), rel("v0.10.0", false, ""), rel("v0.9.0", false, ""), rel("v0.11.0", true, "")];
        assert_eq!(pick(&rs, Channel::Stable, "stable", "", "0.9.0").unwrap().name, "v0.10.0");
        assert!(pick(&rs, Channel::Stable, "stable", "", "0.10.0").is_none());
        assert!(pick(&rs, Channel::Stable, "dev", "", "0.10.0").is_none());
        assert_eq!(pick(&rs, Channel::Stable, "nightly", "x", "0.10.0").unwrap().name, "v0.10.0", "nightly -> stable at the same version");
    }

    #[cfg(not(windows))]
    #[test]
    fn linux_update_replaces_only_the_binaries_and_keeps_other_files() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("keytab-updater-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let pkg = root.join("src/keytab-linux-x86_64");
        let bin = root.join("bin");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(pkg.join("keytab"), b"\x7fELFnew").unwrap();
        std::fs::write(pkg.join("keytabctl"), b"#!/bin/sh\nnew").unwrap();
        std::fs::write(pkg.join("niri.kdl"), b"binds { Alt+Tab {} }").unwrap();
        std::fs::write(bin.join("keytab"), b"old").unwrap();
        std::fs::write(bin.join("keytabctl"), b"old").unwrap();
        std::fs::write(bin.join("keytab.kdl"), b"binds { F13 {} }").unwrap();
        let archive = root.join("a.tar.gz");
        assert!(Command::new("tar").arg("-czf").arg(&archive).arg("-C").arg(root.join("src"))
            .arg("keytab-linux-x86_64").status().unwrap().success());
        replace_from_archive(&archive, &root.join("unpacked"), &bin, std::ffi::OsStr::new("keytab")).unwrap();
        assert_eq!(std::fs::read(bin.join("keytab")).unwrap(), b"\x7fELFnew");
        assert_eq!(std::fs::read(bin.join("keytabctl")).unwrap(), b"#!/bin/sh\nnew");
        assert_eq!(std::fs::metadata(bin.join("keytab")).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(std::fs::read(bin.join("keytab.kdl")).unwrap(), b"binds { F13 {} }", "config is never touched");
        assert!(!bin.join("niri.kdl").exists());
        // A broken archive changes nothing.
        std::fs::write(pkg.join("keytab"), b"not a binary").unwrap();
        assert!(Command::new("tar").arg("-czf").arg(&archive).arg("-C").arg(root.join("src"))
            .arg("keytab-linux-x86_64").status().unwrap().success());
        assert!(replace_from_archive(&archive, &root.join("unpacked2"), &bin, std::ffi::OsStr::new("keytab")).is_err());
        assert_eq!(std::fs::read(bin.join("keytab")).unwrap(), b"\x7fELFnew");
        assert!(std::fs::read_dir(&bin).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".new")));
        assert!(updatable(std::path::Path::new("/home/x/dev/keytab/target/release/keytab")).is_err(), "cargo builds update from source");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nightly_offers_a_different_commit() {
        let rs = [rel("v0.1.0", false, ""), rel("nightly", true, "Built from main.\n\nCommit: `abcdef1234`\n")];
        assert!(pick(&rs, Channel::Nightly, "nightly", "abcdef1234", "0.1.0").is_none());
        assert_eq!(pick(&rs, Channel::Nightly, "nightly", "0000000", "0.1.0").unwrap().name, "nightly (abcdef1)");
        assert!(pick(&rs, Channel::Nightly, "stable", "abcdef1234", "0.1.0").is_some(), "stable -> nightly");
        assert!(pick(&rs[..1], Channel::Nightly, "stable", "", "0.1.0").is_none());
    }
}
