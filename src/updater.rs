//! Checks GitHub for a newer release, downloads it, verifies its SHA-256, and swaps the running exe.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

use eframe::egui;
use sha2::{Digest, Sha256};

const REPO: &str = "BunnyMoon27/fortnite-chest-counter";
const EXE_NAME: &str = "ChestCounter.exe";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

pub enum Msg {
    /// Status line for the Updates section.
    Note(String),
    /// New version downloaded and verified; the file is ready to install.
    Ready(String, PathBuf),
}

struct Release {
    version: String,
    exe_url: String,
    sums_url: String,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(Duration::from_secs(60)).build()
}

/// True when `a` is a higher dotted version than `b` (e.g. "0.1.1" > "0.1.0").
fn newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| s.split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parse(a) > parse(b)
}

/// The latest GitHub release, if it is newer than the running version.
fn check() -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = agent()
        .get(&url)
        .set("User-Agent", "chest-counter-updater")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let version = json["tag_name"].as_str().ok_or("release has no tag")?.trim_start_matches('v').to_string();
    if !newer(&version, CURRENT) {
        return Ok(None);
    }
    let asset = |name: &str| -> Option<String> {
        json["assets"].as_array()?.iter().find(|a| a["name"] == name)?["browser_download_url"].as_str().map(String::from)
    };
    Ok(Some(Release {
        exe_url: asset(EXE_NAME).ok_or("release has no ChestCounter.exe")?,
        sums_url: asset("SHA256SUMS.txt").ok_or("release has no SHA256SUMS.txt")?,
        version,
    }))
}

/// Downloads the new exe to a temp file. Refuses it unless its SHA-256 matches SHA256SUMS.txt.
fn download(rel: &Release) -> Result<PathBuf, String> {
    let sums = agent().get(&rel.sums_url).call().map_err(|e| e.to_string())?.into_string().map_err(|e| e.to_string())?;
    let want = sums
        .lines()
        .find(|l| l.ends_with(EXE_NAME))
        .and_then(|l| l.split_whitespace().next())
        .ok_or("checksum for ChestCounter.exe not found")?
        .to_lowercase();
    let mut bytes = Vec::new();
    agent().get(&rel.exe_url).call().map_err(|e| e.to_string())?.into_reader().read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let got: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if got != want {
        return Err("checksum does not match, download refused".into());
    }
    let path = std::env::temp_dir().join("ChestCounter-update.exe");
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

/// One check-and-download pass. Reports through `tx`.
fn run_once(tx: &Sender<Msg>, ctx: &egui::Context) {
    let send = |m: Msg| {
        let _ = tx.send(m);
        ctx.request_repaint();
    };
    match check() {
        Ok(None) => send(Msg::Note(format!("You have the latest version ({CURRENT})."))),
        Ok(Some(rel)) => {
            send(Msg::Note(format!("Downloading version {}...", rel.version)));
            match download(&rel) {
                Ok(path) => send(Msg::Ready(rel.version, path)),
                Err(e) => send(Msg::Note(format!("Update {} failed: {e}", rel.version))),
            }
        }
        Err(e) => send(Msg::Note(format!("Could not check for updates: {e}"))),
    }
}

/// Checks in the background now (the "Check for updates" button).
pub fn check_now(tx: Sender<Msg>, ctx: egui::Context) {
    std::thread::spawn(move || run_once(&tx, &ctx));
}

/// Checks shortly after start, then every 6 hours.
pub fn spawn_periodic(tx: Sender<Msg>, ctx: egui::Context) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(10));
        run_once(&tx, &ctx);
        std::thread::sleep(Duration::from_secs(6 * 3600));
    });
}

/// Replaces the running exe with `new` and starts it. Only returns with the error text if that failed.
pub fn apply(new: &Path) -> String {
    let Ok(me) = std::env::current_exe() else { return "cannot find the running program".into() };
    let old = me.with_file_name("ChestCounter.old.exe");
    let _ = std::fs::remove_file(&old);
    // A running exe can be renamed but not overwritten, so move it aside first.
    if let Err(e) = std::fs::rename(&me, &old) {
        return e.to_string();
    }
    if let Err(e) = std::fs::copy(new, &me) {
        let _ = std::fs::rename(&old, &me);
        return e.to_string();
    }
    match std::process::Command::new(&me).spawn() {
        Ok(_) => std::process::exit(0),
        Err(e) => e.to_string(),
    }
}

/// Deletes the exe left behind by the last update. Call at start.
pub fn cleanup() {
    if let Ok(me) = std::env::current_exe() {
        let _ = std::fs::remove_file(me.with_file_name("ChestCounter.old.exe"));
    }
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn compares_versions() {
        assert!(newer("0.1.1", "0.1.0"));
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("1.0", "0.9.9"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.0.9", "0.1.0"));
    }
}
