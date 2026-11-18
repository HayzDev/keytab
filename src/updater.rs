//! Updates from GitHub Releases (Windows). Two channels:
//!
//! - stable: the newest `v*` release that isn't a prerelease, newer than this build.
//! - nightly: the rolling `nightly` prerelease, rebuilt from every push to main. Its notes
//!   carry `Commit: <sha>`; it's an update when that isn't the commit this build came from.
//!
//! Downloads use Windows' own `curl.exe` (Windows 10 1803+), so there's no TLS stack in
//! the binary. Installing runs `keytab-setup.exe /S` once keytab has exited, then restarts it.
use crate::settings::Channel;
use serde::{Deserialize, Serialize};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

const REPO: &str = "HayzDev/keytab";
const ASSET: &str = "keytab-setup.exe";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DETACHED_PROCESS: u32 = 0x0000_0008;

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

fn curl() -> Command {
    let system = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
    let mut c = Command::new(system.join("System32").join("curl.exe"));
    c.args(["-fsSL", "--proto", "=https", "--max-time", "120", "-A", "keytab-updater"]);
    c.creation_flags(CREATE_NO_WINDOW);
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
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn()
        .map_err(|e| format!("couldn't start the installer: {e}"))?;
    Ok(())
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

    #[test]
    fn nightly_offers_a_different_commit() {
        let rs = [rel("v0.1.0", false, ""), rel("nightly", true, "Built from main.\n\nCommit: `abcdef1234`\n")];
        assert!(pick(&rs, Channel::Nightly, "nightly", "abcdef1234", "0.1.0").is_none());
        assert_eq!(pick(&rs, Channel::Nightly, "nightly", "0000000", "0.1.0").unwrap().name, "nightly (abcdef1)");
        assert!(pick(&rs, Channel::Nightly, "stable", "abcdef1234", "0.1.0").is_some(), "stable -> nightly");
        assert!(pick(&rs[..1], Channel::Nightly, "stable", "", "0.1.0").is_none());
    }
}
