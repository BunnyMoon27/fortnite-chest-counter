//! Saved match history and settings, so replays Fortnite deletes later stay counted.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::analyzer::{counted, MatchRecord, ANALYZER_VERSION};

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct OverlaySettings {
    pub enabled: bool,
    /// Locked: clicks go through and it cannot be moved.
    pub locked: bool,
    pub pos: Option<[f32; 2]>,
    /// Hotkey for show/hide, e.g. "Ctrl+Shift+O".
    pub toggle_key: String,
    /// Hotkey for lock/unlock (move), e.g. "Ctrl+Shift+L".
    pub lock_key: String,
}

impl Default for OverlaySettings {
    fn default() -> Self {
        OverlaySettings {
            enabled: false,
            locked: true,
            pos: None,
            toggle_key: "Ctrl+Shift+O".into(),
            lock_key: "Ctrl+Shift+L".into(),
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct History {
    pub matches: Vec<MatchRecord>,
    /// Statistics reset: matches and replays from before this moment are ignored.
    pub reset_at_unix: Option<i64>,
    pub overlay: OverlaySettings,
    /// Where the main window was last left (restored on start).
    pub window_pos: Option<[f32; 2]>,
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("ChestCounterRs")
}

pub fn replays_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("FortniteGame").join("Saved").join("Demos")
}

pub fn now_unix() -> i64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl History {
    pub fn path() -> PathBuf {
        data_dir().join("history.json")
    }

    /// A history that cannot be read is moved aside (never overwritten) and a new one starts.
    pub fn load() -> History {
        let path = Self::path();
        let Ok(bytes) = std::fs::read(&path) else { return History::default() };
        let json = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes); // tolerate a UTF-8 BOM
        match serde_json::from_slice(json) {
            Ok(h) => h,
            Err(_) => {
                let _ = std::fs::rename(&path, data_dir().join(format!("history-unreadable-{}.json", now_unix())));
                History::default()
            }
        }
    }

    /// Writes to a temp file first so a crash never leaves a half-written history.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        std::fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)
    }

    /// Copies the current history next to it, then clears the statistics from now on.
    pub fn reset(&mut self) -> std::io::Result<PathBuf> {
        let now = now_unix();
        let backup = data_dir().join(format!("history-backup-{now}.json"));
        if Self::path().exists() {
            std::fs::copy(Self::path(), &backup)?;
        }
        self.matches.clear();
        self.reset_at_unix = Some(now);
        self.save()?;
        Ok(backup)
    }

    pub fn analyzed_files(&self) -> Vec<String> {
        self.matches.iter().filter(|m| m.analyzer_version == ANALYZER_VERSION).map(|m| m.replay_file.clone()).collect()
    }

    /// Adds or replaces a match (same replay file or same game session).
    pub fn upsert(&mut self, m: MatchRecord) {
        self.matches.retain(|x| x.replay_file != m.replay_file && x.session_id != m.session_id);
        self.matches.push(m);
        self.matches.sort_by_key(|m| m.match_start_unix.unwrap_or(m.analyzed_at_unix));
    }

    pub fn counted(&self) -> impl Iterator<Item = &MatchRecord> {
        let from = self.reset_at_unix.unwrap_or(i64::MIN);
        // A match counts after a reset if it ended after it (one in progress during the reset still counts).
        self.matches.iter().filter(move |m| counted(m) && match_end(m) >= from)
    }
}

pub fn match_end(m: &MatchRecord) -> i64 {
    match m.match_start_unix {
        Some(start) => start + m.duration_seconds.unwrap_or(0) as i64,
        None => m.analyzed_at_unix,
    }
}

/// Replays in the Fortnite folder with their size and modification time, oldest first.
pub fn list_replays(dir: &Path) -> Vec<(PathBuf, u64, SystemTime)> {
    let mut files: Vec<(PathBuf, u64, SystemTime)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("replay")))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((e.path(), m.len(), m.modified().unwrap_or(SystemTime::UNIX_EPOCH)))
        })
        .collect();
    files.sort_by_key(|f| f.2);
    files
}
