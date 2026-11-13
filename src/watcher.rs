//! Background thread that watches the Fortnite replay folder and analyzes each finished replay once.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::{Duration, SystemTime};

use chest_counter::analyzer::{analyze_file, MatchRecord};
use chest_counter::replay::{NoHandler, Parser};
use chest_counter::store::{list_replays, replays_dir};
use eframe::egui;

/// A replay still flagged "live" but untouched for this long is from a crashed game, not a match in progress.
const STALE_LIVE: Duration = Duration::from_secs(600);
/// Fortnite rewrites the header last; give it a moment after the final write.
const SETTLE: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_secs(5);

pub enum Msg {
    Progress { done: usize, total: usize },
    /// `fresh` is true for a match that finished while the app was open.
    Analyzed { record: Box<MatchRecord>, fresh: bool },
    Failed(String, String),
    Idle,
    Recording(bool),
}

pub enum Cmd {
    /// Ignore replays older than this moment (statistics reset).
    Reset(i64),
}

pub fn spawn(ctx: egui::Context, already_done: Vec<String>, reset_at: Option<i64>) -> (Receiver<Msg>, Sender<Cmd>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<Cmd>();
    std::thread::spawn(move || {
        let mut done: HashSet<String> = already_done.into_iter().collect();
        // Unreadable or unfinished replays: retried only once the file changes again.
        let mut failed: HashMap<String, SystemTime> = HashMap::new();
        let mut unfinished: HashMap<String, SystemTime> = HashMap::new();
        let mut reset_at = reset_at;
        let mut first_pass = true;
        let mut recording = false;
        let send = |m: Msg| {
            let ok = tx.send(m).is_ok();
            ctx.request_repaint();
            ok
        };
        loop {
            loop {
                match cmd_rx.try_recv() {
                    Ok(Cmd::Reset(t)) => reset_at = Some(t),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }

            let now = SystemTime::now();
            let reset = reset_at.map(|t| SystemTime::UNIX_EPOCH + Duration::from_secs(t.max(0) as u64));
            let mut now_recording = false;
            let todo: Vec<_> = list_replays(&replays_dir())
                .into_iter()
                .filter(|(_, _, modified)| reset.is_none_or(|r| *modified >= r))
                .filter(|(path, _, modified)| {
                    let name = file_name(path);
                    if done.contains(&name) || failed.get(&name) == Some(modified) || unfinished.get(&name) == Some(modified) {
                        return false;
                    }
                    let age = now.duration_since(*modified).unwrap_or_default();
                    // The header's live flag is cleared by Fortnite when the match ends.
                    if Parser::<NoHandler>::peek_info(path).is_ok_and(|i| i.is_live) {
                        now_recording |= age < STALE_LIVE;
                        return false;
                    }
                    age >= SETTLE
                })
                .collect();

            if now_recording != recording {
                recording = now_recording;
                if !send(Msg::Recording(recording)) {
                    return;
                }
            }

            let total = todo.len();
            for (i, (path, _, modified)) in todo.into_iter().enumerate() {
                if first_pass && !send(Msg::Progress { done: i, total }) {
                    return;
                }
                let name = file_name(&path);
                let msg = match analyze_file(&path) {
                    Ok(m) if m.is_partial => {
                        unfinished.insert(name, modified);
                        continue;
                    }
                    Ok(m) => {
                        done.insert(name);
                        Msg::Analyzed { record: Box::new(m), fresh: !first_pass }
                    }
                    Err(e) if e.contains("not yet marked as completed") => {
                        unfinished.insert(name, modified);
                        continue;
                    }
                    Err(e) => {
                        // A recent file is usually still being written (often empty at match start): keep retrying.
                        if now.duration_since(modified).unwrap_or_default() < STALE_LIVE {
                            continue;
                        }
                        failed.insert(name.clone(), modified);
                        Msg::Failed(name, e)
                    }
                };
                if !send(msg) {
                    return;
                }
            }
            if first_pass || total > 0 {
                first_pass = false;
                if !send(Msg::Idle) {
                    return;
                }
            }
            std::thread::sleep(POLL);
        }
    });
    (rx, cmd_tx)
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().unwrap_or_default().to_string_lossy().to_string()
}
