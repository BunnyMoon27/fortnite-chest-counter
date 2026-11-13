// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod updater;
mod watcher;

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chest_counter::analyzer::{self, MatchRecord};
use chest_counter::store::{now_unix, replays_dir, History, OverlaySettings};
use chrono::{Datelike, Local, NaiveDate, TimeZone};
use eframe::egui::{self, Align2, Color32, FontId, Id, Rect, RichText, Rounding, Sense, Stroke, Vec2};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use watcher::{Cmd, Msg};

// --- palette -------------------------------------------------------------------------------------------------

const BG: Color32 = Color32::from_rgb(0x0C, 0x0E, 0x13);
const CARD: Color32 = Color32::from_rgb(0x15, 0x18, 0x20);
const CARD_HI: Color32 = Color32::from_rgb(0x1C, 0x20, 0x2A);
const BORDER: Color32 = Color32::from_rgb(0x24, 0x29, 0x34);
const TEXT: Color32 = Color32::from_rgb(0xEC, 0xEE, 0xF2);
const MUTED: Color32 = Color32::from_rgb(0x84, 0x8B, 0x99);
const GOLD: Color32 = Color32::from_rgb(0xF0, 0xB8, 0x4E);
const GOLD_DIM: Color32 = Color32::from_rgb(0x5C, 0x47, 0x22);
const GREEN: Color32 = Color32::from_rgb(0x4A, 0xD6, 0x7E);
const RED: Color32 = Color32::from_rgb(0xF0, 0x6A, 0x6A);
const PURPLE: Color32 = Color32::from_rgb(0xB0, 0x7C, 0xF0);
const BLUE: Color32 = Color32::from_rgb(0x5C, 0xA8, 0xF0);

/// How long the "match finished" result stays on the overlay.
const RESULT_ON_OVERLAY: Duration = Duration::from_secs(20);
/// How long the big "finished" banner animates and throws confetti.
const CELEBRATE: f32 = 2.6;

fn kind_color(kind: &str) -> Color32 {
    match kind {
        analyzer::RARE => PURPLE,
        analyzer::EXTRACTION | analyzer::POSTER_CHEST => BLUE,
        analyzer::BURIED | analyzer::OTHER_CHEST => GREEN,
        _ => GOLD,
    }
}

/// Name shown in the UI. The stored kind key stays as it was, so old history still matches.
fn kind_label(kind: &str) -> &'static str {
    match kind {
        analyzer::NORMAL => "Chest",
        analyzer::RARE => "Rare chest",
        analyzer::BURIED => "Buried chest",
        analyzer::EXTRACTION => "Relic chest",
        analyzer::POSTER_CHEST => "Spirit poster chest",
        _ => "Other chest",
    }
}

fn kind_short(kind: &str) -> &'static str {
    match kind {
        analyzer::RARE => "rare",
        analyzer::EXTRACTION => "relic",
        analyzer::POSTER_CHEST => "poster",
        analyzer::BURIED => "buried",
        analyzer::OTHER_CHEST => "other",
        _ => "chest",
    }
}

fn fade(c: Color32, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a.clamp(0.0, 1.0) * 255.0) as u8)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Deterministic pseudo-random value in 0..1 (confetti layout, no RNG state needed).
fn hash01(seed: u32) -> f32 {
    let mut x = seed.wrapping_mul(0x9E37_79B9) ^ 0x85EB_CA6B;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    (x & 0xFFFF) as f32 / 65535.0
}

// --- stats ---------------------------------------------------------------------------------------------------

/// "Ctrl+Shift+O" → hotkey. Letters, digits and F1–F12 only.
fn parse_hotkey(s: &str) -> Option<HotKey> {
    let mut mods = Modifiers::empty();
    let mut key = None;
    for part in s.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "shift" => mods |= Modifiers::SHIFT,
            "alt" => mods |= Modifiers::ALT,
            _ => key = Some(code_of(part)?),
        }
    }
    Some(HotKey::new(Some(mods), key?))
}

fn code_of(name: &str) -> Option<Code> {
    use Code::*;
    const LETTERS: [Code; 26] = [
        KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO, KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ,
    ];
    const DIGITS: [Code; 10] = [Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9];
    const FKEYS: [Code; 12] = [F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12];
    let up = name.to_ascii_uppercase();
    if let [c] = up.as_bytes() {
        return match *c {
            b'A'..=b'Z' => Some(LETTERS[(*c - b'A') as usize]),
            b'0'..=b'9' => Some(DIGITS[(*c - b'0') as usize]),
            _ => None,
        };
    }
    let n: usize = up.strip_prefix('F')?.parse().ok()?;
    FKEYS.get(n.checked_sub(1)?).copied()
}

#[cfg(test)]
mod hotkey_tests {
    use super::parse_hotkey;

    #[test]
    fn parses_keys_and_rejects_junk() {
        assert!(parse_hotkey("Ctrl+Shift+O").is_some());
        assert!(parse_hotkey("f9").is_some());
        assert!(parse_hotkey("Ctrl+Shift").is_none());
        assert!(parse_hotkey("Ctrl+Foo").is_none());
        assert!(parse_hotkey("F13").is_none());
    }
}

fn when(m: &MatchRecord) -> i64 {
    m.match_start_unix.unwrap_or(m.analyzed_at_unix)
}

fn local_date(unix: i64) -> Option<NaiveDate> {
    Local.timestamp_opt(unix, 0).single().map(|d| d.date_naive())
}

struct Stats {
    total: u32,
    matches: usize,
    today: u32,
    session: u32,
    session_matches: u32,
    average: f32,
    best_match: u32,
    best_day: (u32, Option<NaiveDate>),
    last: Option<MatchRecord>,
    last7: Vec<(NaiveDate, u32)>,
    recent: Vec<MatchRecord>,
    /// Chests by kind (only kinds that were opened appear).
    by_kind: BTreeMap<String, u32>,
}

/// Session = matches that finished while the app was open.
fn stats(history: &History, session: &HashSet<String>) -> Stats {
    let counted: Vec<&MatchRecord> = history.counted().collect();
    let total: u32 = counted.iter().map(|m| m.chests_opened).sum();
    let today = Local::now().date_naive();
    let mut per_day: BTreeMap<NaiveDate, u32> = BTreeMap::new();
    for m in &counted {
        if let Some(d) = local_date(when(m)) {
            *per_day.entry(d).or_default() += m.chests_opened;
        }
    }
    let session_matches: Vec<&&MatchRecord> = counted.iter().filter(|m| session.contains(&m.session_id)).collect();
    let best_day = per_day.iter().max_by_key(|(_, &v)| v).map(|(d, &v)| (v, Some(*d))).unwrap_or((0, None));
    let last7 = (0..7).rev().map(|i| today - chrono::Days::new(i)).map(|d| (d, per_day.get(&d).copied().unwrap_or(0))).collect();
    let mut by_kind: BTreeMap<String, u32> = BTreeMap::new();
    for c in counted.iter().flat_map(|m| m.chests.iter()) {
        *by_kind.entry(c.kind.clone()).or_default() += 1;
    }
    let mut recent = counted.clone();
    recent.sort_by_key(|m| std::cmp::Reverse(when(m)));
    Stats {
        total,
        matches: counted.len(),
        today: per_day.get(&today).copied().unwrap_or(0),
        session: session_matches.iter().map(|m| m.chests_opened).sum(),
        session_matches: session_matches.len() as u32,
        average: if counted.is_empty() { 0.0 } else { total as f32 / counted.len() as f32 },
        best_match: counted.iter().map(|m| m.chests_opened).max().unwrap_or(0),
        best_day,
        last: recent.first().map(|m| (*m).clone()),
        last7,
        recent: recent.iter().take(30).map(|m| (*m).clone()).collect(),
        by_kind,
    }
}

/// What the overlay shows, computed once per refresh.
struct LastSummary {
    chests: u32,
    place: Option<i32>,
    elims: Option<u32>,
}

struct OverlayData {
    total: u32,
    session: u32,
    today: u32,
    average: f32,
    last: Option<LastSummary>,
    spark: Vec<u32>,
}

// --- app -----------------------------------------------------------------------------------------------------

enum Scan {
    Reading { done: usize, total: usize },
    Watching,
}

struct App {
    history: History,
    rx: Receiver<Msg>,
    cmd: Sender<Cmd>,
    scan: Scan,
    recording: bool,
    failures: Vec<(String, String)>,
    session: HashSet<String>,
    /// Latest match that finished while the app was open, with when it arrived.
    result: Option<(MatchRecord, Instant)>,
    result_dismissed: bool,
    confirm_reset: bool,
    notice: Option<String>,
    /// Statistics cache: recomputed when history changes or every few seconds (day rollover).
    stats_cache: Option<(Instant, Stats)>,
    stats_dirty: bool,
    /// Click-through state last sent to the overlay (commands trigger repaints, so send on change only).
    passthrough_sent: Option<bool>,
    /// Windows 11 corner/border removed from the current overlay window.
    overlay_unrounded: bool,
    overlay_toggle: Arc<AtomicBool>,
    overlay_lock: Arc<AtomicBool>,
    hotkeys: Option<GlobalHotKeyManager>,
    /// The two hotkeys currently registered, and their ids (read by the event handler).
    bound: Option<(HotKey, HotKey)>,
    hotkey_ids: Arc<Mutex<(u32, u32)>>,
    hotkey_err: Option<String>,
    /// Text in the Hotkeys settings fields: (show/hide, lock/move).
    hotkey_edit: (String, String),
    update_rx: Receiver<updater::Msg>,
    update_tx: Sender<updater::Msg>,
    update_note: Option<String>,
    /// Downloaded and verified update, installed as soon as no match is recording.
    update_ready: Option<PathBuf>,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        apply_theme(&cc.egui_ctx);
        let history = History::load();
        let (rx, cmd) = watcher::spawn(cc.egui_ctx.clone(), history.analyzed_files(), history.reset_at_unix);
        let (update_tx, update_rx) = std::sync::mpsc::channel();
        updater::spawn_periodic(update_tx.clone(), cc.egui_ctx.clone());

        // Global hotkeys (work while the game has focus); the ids follow whatever keys are bound.
        let overlay_toggle = Arc::new(AtomicBool::new(false));
        let overlay_lock = Arc::new(AtomicBool::new(false));
        let hotkey_ids = Arc::new(Mutex::new((0u32, 0u32)));
        {
            let (t, l, ctx, ids) = (overlay_toggle.clone(), overlay_lock.clone(), cc.egui_ctx.clone(), hotkey_ids.clone());
            GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
                if e.state != HotKeyState::Pressed {
                    return;
                }
                let Ok((tid, lid)) = ids.lock().map(|g| *g) else { return };
                if e.id == tid {
                    t.store(true, Ordering::SeqCst);
                } else if e.id == lid {
                    l.store(true, Ordering::SeqCst);
                }
                ctx.request_repaint();
            }));
        }

        // Test switch: show the latest match as if it had just finished (banner + overlay card).
        let result = std::env::var_os("CHESTCOUNTER_DEMO_RESULT")
            .and_then(|_| history.counted().max_by_key(|m| when(m)).cloned())
            .map(|m| (m, Instant::now()));

        let saved = (history.overlay.toggle_key.clone(), history.overlay.lock_key.clone());
        let mut app = App {
            history,
            rx,
            cmd,
            scan: Scan::Reading { done: 0, total: 0 },
            recording: false,
            failures: Vec::new(),
            session: HashSet::new(),
            result,
            result_dismissed: false,
            confirm_reset: false,
            notice: None,
            stats_cache: None,
            stats_dirty: true,
            passthrough_sent: None,
            overlay_unrounded: false,
            overlay_toggle,
            overlay_lock,
            hotkeys: GlobalHotKeyManager::new().ok(),
            bound: None,
            hotkey_ids,
            hotkey_err: None,
            hotkey_edit: (saved.0.clone(), saved.1.clone()),
            update_rx,
            update_tx,
            update_note: None,
            update_ready: None,
        };
        // Saved keys that fail (typo, or taken by another app) fall back to the defaults; the error stays visible.
        if let Err(e) = app.bind_hotkeys(&saved.0, &saved.1) {
            app.hotkey_err = Some(e);
            let d = OverlaySettings::default();
            let _ = app.bind_hotkeys(&d.toggle_key, &d.lock_key);
        }
        app
    }

    fn save(&mut self) {
        if let Err(e) = self.history.save() {
            self.notice = Some(format!("Could not save history: {e}"));
        }
    }

    /// Replaces the registered hotkeys. On failure the previous pair is restored.
    fn bind_hotkeys(&mut self, toggle: &str, lock: &str) -> Result<(), String> {
        let (Some(t), Some(l)) = (parse_hotkey(toggle), parse_hotkey(lock)) else {
            return Err("Unknown key. Use letters, digits or F1–F12, e.g. Ctrl+Shift+O".into());
        };
        if t.id() == l.id() {
            return Err("The two actions need different keys".into());
        }
        let m = self.hotkeys.as_ref().ok_or("Global hotkeys are not available on this PC")?;
        let old = self.bound.take();
        if let Some((ot, ol)) = old {
            let _ = m.unregister(ot);
            let _ = m.unregister(ol);
        }
        match m.register(t).and_then(|_| m.register(l)) {
            Ok(()) => {
                *self.hotkey_ids.lock().unwrap_or_else(|p| p.into_inner()) = (t.id(), l.id());
                self.bound = Some((t, l));
                self.hotkey_err = None;
                Ok(())
            }
            Err(e) => {
                let _ = m.unregister(t);
                let _ = m.unregister(l);
                if let Some((ot, ol)) = old {
                    let _ = m.register(ot);
                    let _ = m.register(ol);
                    self.bound = Some((ot, ol));
                }
                Err(format!("Could not register that key (used by another app?): {e}"))
            }
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let mut changed = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Progress { done, total } => self.scan = Scan::Reading { done, total },
                Msg::Analyzed { record, fresh } => {
                    if fresh {
                        self.session.insert(record.session_id.clone());
                    }
                    if fresh && analyzer::counted(&record) {
                        self.result = Some((*record.clone(), Instant::now()));
                        self.result_dismissed = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
                    }
                    self.history.upsert(*record);
                    changed = true;
                }
                Msg::Failed(file, e) => self.failures.push((file, e)),
                Msg::Idle => self.scan = Scan::Watching,
                Msg::Recording(r) => self.recording = r,
            }
        }
        while let Ok(m) = self.update_rx.try_recv() {
            match m {
                updater::Msg::Note(s) => self.update_note = Some(s),
                updater::Msg::Ready(v, p) => {
                    self.update_note = Some(format!("Version {v} is ready. It installs when no match is recording."));
                    self.update_ready = Some(p);
                }
            }
        }
        // Install as soon as no match is being recorded. apply() only returns if the install failed.
        if !self.recording {
            if let Some(p) = self.update_ready.take() {
                self.update_note = Some(format!("Update failed: {}", updater::apply(&p)));
            }
        }
        if self.overlay_toggle.swap(false, Ordering::SeqCst) {
            self.history.overlay.enabled = !self.history.overlay.enabled;
            changed = true;
        }
        if self.overlay_lock.swap(false, Ordering::SeqCst) {
            self.history.overlay.locked = !self.history.overlay.locked;
            changed = true;
        }
        if changed {
            self.stats_dirty = true;
            self.save();
        }
        // Redraw once when the overlay's "match finished" card expires (nothing else may trigger it).
        if let Some((_, at)) = &self.result {
            if let Some(left) = RESULT_ON_OVERLAY.checked_sub(at.elapsed()) {
                ctx.request_repaint_after(left + Duration::from_millis(50));
            }
        }
    }

    /// Seconds since the latest match arrived, if it arrived recently enough to animate.
    fn result_age(&self) -> Option<f32> {
        self.result.as_ref().map(|(_, at)| at.elapsed().as_secs_f32())
    }
}

fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.window_stroke = Stroke::new(1.0_f32, BORDER);
    v.window_rounding = Rounding::same(12.0);
    v.extreme_bg_color = BG;
    v.override_text_color = Some(TEXT);
    v.widgets.inactive.weak_bg_fill = CARD_HI;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    v.widgets.inactive.rounding = Rounding::same(8.0);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x28, 0x2E, 0x3A);
    v.widgets.hovered.rounding = Rounding::same(8.0);
    v.widgets.active.rounding = Rounding::same(8.0);
    v.selection.bg_fill = GOLD_DIM;
    ctx.set_visuals(v);
    // The overlay (a second window) flips widget rects between passes, which would repaint forever.
    ctx.options_mut(|o| o.repaint_on_widget_change = false);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = Vec2::new(8.0, 8.0);
        s.spacing.button_padding = Vec2::new(12.0, 6.0);
        s.spacing.scroll.bar_width = 6.0;
    });
}

fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::none()
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .rounding(12.0)
        .inner_margin(egui::Margin::same(10.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A filled status dot (the default font has no "●").
fn dot(ui: &mut egui::Ui, color: Color32, halo: f32) {
    let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
    let p = ui.painter();
    if halo > 0.0 {
        p.circle_filled(r.center(), 3.5 + 3.0 * halo, fade(color, 0.25 * (1.0 - halo)));
    }
    p.circle_filled(r.center(), 4.0, color);
}

fn caption(text: &str) -> RichText {
    RichText::new(text.to_uppercase()).size(10.0).color(MUTED).strong()
}

/// A small closed chest drawn with shapes (no image assets needed).
fn chest_icon(painter: &egui::Painter, rect: Rect, color: Color32) {
    let lid = Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.min.y + rect.height() * 0.42));
    let body = Rect::from_min_max(egui::pos2(rect.min.x, lid.max.y + rect.height() * 0.06), rect.max);
    painter.rect_filled(lid, Rounding { nw: 6.0, ne: 6.0, sw: 1.0, se: 1.0 }, color);
    painter.rect_filled(body, Rounding { nw: 1.0, ne: 1.0, sw: 3.0, se: 3.0 }, color);
    let lock = Rect::from_center_size(egui::pos2(rect.center().x, lid.max.y + 1.0), Vec2::new(rect.width() * 0.18, rect.height() * 0.26));
    painter.rect_filled(lock, 2.0, BG);
}

/// Bar growing from the bottom, with the value shown on hover.
fn sparkline(ui: &mut egui::Ui, values: &[u32], height: f32, key: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
    if values.is_empty() {
        return;
    }
    let max = values.iter().copied().max().unwrap_or(1).max(1) as f32;
    let slot = rect.width() / values.len() as f32;
    let pointer = ui.ctx().pointer_hover_pos();
    let last = values.len() - 1;
    for (i, v) in values.iter().enumerate() {
        let x = rect.min.x + slot * i as f32;
        let slot_rect = Rect::from_min_max(egui::pos2(x, rect.min.y), egui::pos2(x + slot, rect.max.y));
        let hovered = pointer.is_some_and(|p| slot_rect.contains(p));
        let grow = ui.ctx().animate_value_with_time(Id::new((key, i)), *v as f32 / max, 0.6);
        let bottom = rect.max.y - 2.0;
        let h = ((rect.height() - 14.0) * grow).max(3.0);
        let bar = Rect::from_min_max(egui::pos2(x + slot * 0.2, bottom - h), egui::pos2(x + slot * 0.8, bottom));
        let color = if hovered || i == last { GOLD } else { fade(GOLD, 0.3) };
        let p = ui.painter();
        p.rect_filled(bar, 3.0, color);
        if hovered {
            p.text(egui::pos2(bar.center().x, bar.min.y - 2.0), Align2::CENTER_BOTTOM, v.to_string(), FontId::proportional(10.0), TEXT);
        }
    }
}

/// Thin progress bar with a moving shine.
fn progress(ui: &mut egui::Ui, frac: f32, t: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 5.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 3.0, CARD_HI);
    let fill = Rect::from_min_size(rect.min, Vec2::new(rect.width() * frac.clamp(0.0, 1.0), rect.height()));
    p.rect_filled(fill, 3.0, GOLD);
    let x = rect.min.x + (t * 0.7).fract() * rect.width();
    let shine = Rect::from_min_size(egui::pos2(x - 14.0, rect.min.y), Vec2::new(28.0, rect.height())).intersect(fill);
    p.rect_filled(shine, 3.0, Color32::from_white_alpha(110));
}

/// Gold burst that falls and fades for a couple of seconds after a match ends.
fn confetti(painter: &egui::Painter, origin: egui::Pos2, age: f32) {
    if age <= 0.0 || age > CELEBRATE {
        return;
    }
    let alpha = 1.0 - age / CELEBRATE;
    for i in 0..40u32 {
        let angle = hash01(i * 3) * std::f32::consts::TAU;
        let speed = 80.0 + hash01(i * 3 + 1) * 120.0;
        let pos = origin + Vec2::new(angle.cos() * speed * age, angle.sin() * speed * age + 260.0 * age * age);
        let color = [GOLD, PURPLE, GREEN, BLUE, RED][i as usize % 5];
        painter.circle_filled(pos, 1.8 + hash01(i * 3 + 2) * 2.2, fade(color, alpha));
    }
}

/// Last 7 days as bars; today is gold, hover shows the day's total.
fn bar_chart(ui: &mut egui::Ui, days: &[(NaiveDate, u32)]) {
    let height = 96.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
    let max = days.iter().map(|d| d.1).max().unwrap_or(0).max(1) as f32;
    let slot = rect.width() / days.len() as f32;
    let today = Local::now().date_naive();
    for (i, (date, value)) in days.iter().enumerate() {
        let x = rect.min.x + slot * i as f32;
        let slot_rect = Rect::from_min_max(egui::pos2(x, rect.min.y), egui::pos2(x + slot, rect.max.y));
        let resp = ui.interact(slot_rect, Id::new(("day", i)), Sense::hover());
        let grow = ui.ctx().animate_value_with_time(Id::new(("bar", i)), *value as f32 / max, 0.7);
        let bar_top = rect.min.y + 14.0;
        let bar_bottom = rect.max.y - 16.0;
        let h = ((bar_bottom - bar_top) * grow).max(2.0);
        let bar = Rect::from_min_max(egui::pos2(x + slot * 0.2, bar_bottom - h), egui::pos2(x + slot * 0.8, bar_bottom));
        let color = if *date == today {
            GOLD
        } else if *value > 0 {
            fade(GOLD, 0.45)
        } else {
            BORDER
        };
        let p = ui.painter();
        p.rect_filled(bar, Rounding { nw: 4.0, ne: 4.0, sw: 1.0, se: 1.0 }, if resp.hovered() { GOLD } else { color });
        if *value > 0 {
            p.text(egui::pos2(bar.center().x, bar.min.y - 3.0), Align2::CENTER_BOTTOM, value.to_string(), FontId::proportional(10.5), TEXT);
        }
        let label = if *date == today { "Today".to_string() } else { date.weekday().to_string() };
        p.text(egui::pos2(x + slot / 2.0, rect.max.y), Align2::CENTER_BOTTOM, label, FontId::proportional(10.0), MUTED);
        resp.on_hover_text(format!("{} · {} chests", date.format("%a %b %-d"), value));
    }
}

fn mode_name(playlist: Option<&str>) -> String {
    let p = playlist.unwrap_or("?");
    let p = p.strip_prefix("Playlist_").unwrap_or(p);
    let no_build = p.contains("NoBuild");
    let size = ["Solo", "Duo", "Trio", "Squad"].into_iter().find(|s| p.contains(s)).unwrap_or("");
    let ranked = if p.contains("Ranked") { "Ranked " } else { "" };
    if size.is_empty() {
        return p.replace('_', " ");
    }
    format!("{ranked}{}{size}s", if no_build { "Zero Build " } else { "Build " }).replace("Solos", "Solo")
}

/// One stat card: lifts and lights up on hover.
fn tile(ui: &mut egui::Ui, key: &str, title: &str, value: String, detail: String, accent: Color32) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 58.0), Sense::hover());
    let hover = ui.ctx().animate_bool_with_time(Id::new(("tile", key)), resp.hovered(), 0.15);
    let rect = rect.translate(Vec2::new(0.0, -2.0 * hover));
    let p = ui.painter();
    p.rect(rect, 10.0, mix(CARD, CARD_HI, hover), Stroke::new(1.0_f32, mix(BORDER, fade(accent, 0.7), hover)));
    p.rect_filled(Rect::from_min_size(rect.min + Vec2::new(0.0, 10.0), Vec2::new(2.5, rect.height() - 20.0)), 2.0, accent);
    p.text(rect.min + Vec2::new(12.0, 8.0), Align2::LEFT_TOP, title.to_uppercase(), FontId::proportional(9.0), MUTED);
    p.text(rect.min + Vec2::new(12.0, 21.0), Align2::LEFT_TOP, value, FontId::proportional(19.0), accent);
    if !detail.is_empty() {
        p.text(rect.right_bottom() + Vec2::new(-10.0, -7.0), Align2::RIGHT_BOTTOM, detail, FontId::proportional(9.5), MUTED);
    }
}

/// One row of the "By type" card: name, count, and a bar that grows to its share.
fn type_row(ui: &mut egui::Ui, name: &str, count: u32, color: Color32, max: f32) {
    ui.horizontal(|ui| {
        dot(ui, color, 0.0);
        ui.label(RichText::new(name).size(11.5));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(count.to_string()).size(12.0).strong().color(color));
        });
    });
    let grow = ui.ctx().animate_value_with_time(Id::new(("type", name)), count as f32 / max, 0.6);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 4.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2.0, CARD_HI);
    p.rect_filled(Rect::from_min_size(rect.min, Vec2::new(rect.width() * grow, rect.height())), 2.0, color);
}

/// "Fri Nov 13 · 4pm", or "4:30pm" when there are minutes.
fn when_label(unix: i64) -> String {
    use chrono::Timelike;
    let Some(d) = Local.timestamp_opt(unix, 0).single() else { return String::new() };
    let time = if d.minute() == 0 { d.format("%-I%P") } else { d.format("%-I:%M%P") };
    format!("{} · {time}", d.format("%a %b %-d"))
}

fn match_row(ui: &mut egui::Ui, m: &MatchRecord) {
    let place = m.placement.unwrap_or(0);
    let accent = if place == 1 {
        GOLD
    } else if (1..=10).contains(&place) {
        GREEN
    } else {
        BORDER
    };
    egui::Frame::none().fill(CARD_HI).rounding(9.0).inner_margin(egui::Margin::symmetric(10.0, 6.0)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(mode_name(m.playlist.as_deref())).strong().size(12.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(m.chests_opened.to_string()).size(16.0).strong().color(GOLD));
                ui.add_space(4.0);
                if let Some(p) = m.placement {
                    let (text, color) = if p == 1 { ("WIN".to_string(), GOLD) } else if p <= 10 { (format!("#{p}"), GREEN) } else { (format!("#{p}"), MUTED) };
                    ui.label(RichText::new(text).size(10.5).strong().color(color));
                }
            });
        });
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let date = when_label(when(m));
            ui.label(RichText::new(date).size(10.0).color(MUTED));
            let mut kinds: BTreeMap<&str, u32> = BTreeMap::new();
            for c in &m.chests {
                *kinds.entry(c.kind.as_str()).or_default() += 1;
            }
            for (k, n) in kinds.iter().filter(|(k, _)| **k != analyzer::NORMAL) {
                ui.label(RichText::new(format!("{n} {}", kind_short(k))).size(10.0).color(kind_color(k)));
            }
            if let Some(e) = m.eliminations.filter(|&e| e > 0) {
                ui.label(RichText::new(format!("{e} elim")).size(10.0).color(MUTED));
            }
        });
        // Accent stripe for placement: drawn last so it sits on top of the frame edge.
        let r = ui.min_rect();
        ui.painter().rect_filled(Rect::from_min_size(r.min + Vec2::new(-10.0, 6.0), Vec2::new(2.5, r.height() - 12.0)), 2.0, accent);
    });
}

impl App {
    fn header(&mut self, ui: &mut egui::Ui, t: f32) {
        let pulse = 0.5 + 0.5 * (t * 2.2).sin();
        ui.horizontal(|ui| {
            let (logo, _) = ui.allocate_exact_size(Vec2::new(30.0, 26.0), Sense::hover());
            ui.painter().circle_filled(logo.center(), 16.0, fade(GOLD, 0.04 + 0.06 * pulse));
            chest_icon(ui.painter(), logo.shrink(3.0), GOLD);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new("Chest Counter").size(15.0).strong());
                ui.label(RichText::new("Fortnite chest tracker").size(9.5).color(MUTED));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (dot_color, text) = match (&self.scan, self.recording) {
                    (Scan::Reading { .. }, _) => (GOLD, "Reading"),
                    (_, true) => (RED, "Recording"),
                    _ => (GREEN, "Watching"),
                };
                let halo = if self.recording { pulse } else { 0.0 };
                dot(ui, dot_color, halo);
                ui.label(RichText::new(text).size(10.5).color(MUTED));
            });
        });
        if let Scan::Reading { done, total } = self.scan {
            ui.add_space(4.0);
            progress(ui, if total > 0 { done as f32 / total as f32 } else { 0.0 }, t);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 16.0) / 3.0;
            let on = self.history.overlay.enabled;
            let (label, tint) = if on { ("Overlay on", GREEN) } else { ("Overlay off", MUTED) };
            let overlay = egui::Button::new(RichText::new(label).color(tint).size(12.0))
                .fill(if on { fade(GREEN, 0.10) } else { CARD_HI })
                .stroke(Stroke::new(1.0_f32, if on { fade(GREEN, 0.45) } else { BORDER }))
                .rounding(8.0);
            let tip = format!("Show the counter over the game ({})", self.history.overlay.toggle_key);
            if ui.add_sized([w, 28.0], overlay).on_hover_text(tip).clicked() {
                self.history.overlay.enabled = !on;
                self.save();
            }
            // Unlocked = drag to move; locked = clicks go through.
            let locked = self.history.overlay.locked;
            let lock = egui::Button::new(RichText::new(if locked { "Move" } else { "Lock" }).size(12.0).color(if locked { TEXT } else { GOLD }))
                .fill(if locked { CARD_HI } else { fade(GOLD, 0.10) })
                .stroke(Stroke::new(1.0_f32, if locked { BORDER } else { fade(GOLD, 0.45) }))
                .rounding(8.0);
            let tip = if locked {
                format!("Unlock so you can drag the overlay ({})", self.history.overlay.lock_key)
            } else {
                "Lock: clicks go through the overlay again".to_string()
            };
            if ui.add_sized([w, 28.0], lock).on_hover_text(tip).clicked() {
                self.history.overlay.locked = !locked;
                self.save();
            }
            let reset = egui::Button::new(RichText::new("Reset").size(12.0).color(TEXT)).fill(CARD_HI).stroke(Stroke::new(1.0_f32, BORDER)).rounding(8.0);
            if ui.add_sized([w, 28.0], reset).on_hover_text("Start counting from zero (a backup is kept)").clicked() {
                self.confirm_reset = true;
            }
        });
    }

    fn result_banner(&mut self, ui: &mut egui::Ui) {
        let Some((m, _)) = self.result.as_ref().filter(|_| !self.result_dismissed) else { return };
        let age = self.result_age().unwrap_or(0.0);
        let a = (age / 0.35).clamp(0.0, 1.0);
        let mut close = false;
        let mut origin = None;
        egui::Frame::none()
            .fill(fade(Color32::from_rgb(0x2E, 0x24, 0x10), a))
            .stroke(Stroke::new(1.5_f32, fade(GOLD, a)))
            .rounding(12.0)
            .inner_margin(egui::Margin::same(10.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    // The chest bounces once when the match lands.
                    let bounce = if age < 0.8 { (age * 9.0).sin().abs() * 5.0 * (1.0 - age / 0.8) } else { 0.0 };
                    let (icon, _) = ui.allocate_exact_size(Vec2::new(30.0, 26.0), Sense::hover());
                    let icon = icon.translate(Vec2::new(0.0, -bounce));
                    chest_icon(ui.painter(), icon, fade(GOLD, a));
                    origin = Some(icon.center());
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new("MATCH FINISHED").size(9.5).color(fade(MUTED, a)).strong());
                        let word = if m.chests_opened == 1 { "chest" } else { "chests" };
                        ui.label(RichText::new(format!("You opened {} {word}", m.chests_opened)).size(15.0).strong().color(fade(GOLD, a)));
                        let mut details = vec![mode_name(m.playlist.as_deref())];
                        if let (Some(p), Some(t)) = (m.placement, m.total_players) {
                            details.push(format!("#{p} of {t}"));
                        }
                        if let Some(e) = m.eliminations {
                            details.push(format!("{e} elim"));
                        }
                        ui.label(RichText::new(details.join("  ·  ")).size(10.5).color(fade(MUTED, a)));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        if ui.add(egui::Button::new(RichText::new("×").size(16.0).color(MUTED)).frame(false)).on_hover_text("Dismiss").clicked() {
                            close = true;
                        }
                    });
                });
            });
        if let Some(o) = origin {
            confetti(ui.painter(), o, age);
        }
        if close {
            self.result_dismissed = true;
        }
        ui.add_space(6.0);
    }

    fn reset_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_reset {
            return;
        }
        egui::Window::new("Reset statistics?")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size([240.0, 0.0])
            .show(ctx, |ui| {
                ui.label("Your chest total and match history go back to zero. Replays already in the Fortnite folder will not be counted again.");
                ui.label(RichText::new("A backup copy of the current history is saved first.").size(11.0).color(MUTED));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let reset = egui::Button::new(RichText::new("Reset").color(Color32::WHITE).strong()).fill(Color32::from_rgb(0xB8, 0x3B, 0x3B));
                    if ui.add(reset).clicked() {
                        match self.history.reset() {
                            Ok(backup) => {
                                let _ = self.cmd.send(Cmd::Reset(self.history.reset_at_unix.unwrap_or_else(now_unix)));
                                self.result = None;
                                self.failures.clear();
                                self.stats_dirty = true;
                                self.notice = Some(format!("Statistics reset. Backup: {}", backup.display()));
                            }
                            Err(e) => self.notice = Some(format!("Reset failed: {e}")),
                        }
                        self.confirm_reset = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_reset = false;
                    }
                });
            });
    }

    fn overlay(&mut self, ctx: &egui::Context, d: &OverlayData) {
        if !self.history.overlay.enabled {
            self.passthrough_sent = None;
            self.overlay_unrounded = false;
            return;
        }
        let locked = self.history.overlay.locked;
        let fresh = self.result.as_ref().filter(|(_, at)| at.elapsed() < RESULT_ON_OVERLAY).map(|(m, _)| m.chests_opened);
        let glow = self.result_age().map_or(0.0, |a| (1.0 - a / 4.0).clamp(0.0, 1.0));
        let recording = self.recording;
        let t = ctx.input(|i| i.time) as f32;
        let rec_pulse = 0.5 + 0.5 * (t * 4.0).sin();
        // Animated values are computed here, outside the viewport closure.
        let total_shown = ctx.animate_value_with_time(Id::new("ov_total"), d.total as f32, 1.0).round() as u32;
        let spark_grow: Vec<f32> = {
            let max = d.spark.iter().copied().max().unwrap_or(1).max(1) as f32;
            d.spark.iter().enumerate().map(|(i, v)| ctx.animate_value_with_time(Id::new(("ov_spark", i)), *v as f32 / max, 0.6)).collect()
        };

        let lock_hint = format!("drag · {} to lock", self.history.overlay.lock_key);
        let mut builder = egui::ViewportBuilder::default()
            .with_title("Chest Counter overlay")
            .with_decorations(false)
            .with_transparent(true)
            .with_window_level(egui::WindowLevel::AlwaysOnTop)
            .with_taskbar(false)
            .with_resizable(false)
            .with_mouse_passthrough(locked)
            .with_inner_size([300.0, 150.0]);
        if let Some([x, y]) = self.history.overlay.pos {
            builder = builder.with_position([x, y]);
        }
        let mut hide = false;
        let mut new_pos = None;
        let id = egui::ViewportId::from_hash_of("overlay");
        let send_passthrough = self.passthrough_sent != Some(locked);
        self.passthrough_sent = Some(locked);
        ctx.show_viewport_immediate(id, builder, |ctx, _class| {
            if send_passthrough {
                ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(locked));
            }
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                let rect = ui.max_rect();
                let response = ui.interact(rect, ui.id().with("overlay-drag"), Sense::click_and_drag());
                if response.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                if response.drag_stopped() {
                    new_pos = ctx.input(|i| i.viewport().outer_rect).map(|r| [r.min.x, r.min.y]);
                }
                if response.secondary_clicked() {
                    hide = true;
                }
                let painter = ui.painter();
                let border = if fresh.is_some() { GOLD } else if locked { Color32::from_rgba_unmultiplied(255, 255, 255, 28) } else { GREEN };
                painter.rect(rect, 10.0, Color32::from_rgba_unmultiplied(12, 14, 20, 232), Stroke::new(1.0_f32, mix(border, GOLD, glow)));
                // Gold accent bar on the left edge.
                let bar = Rect::from_min_max(rect.min + Vec2::new(6.0, 14.0), egui::pos2(rect.min.x + 9.0, rect.max.y - 14.0));
                painter.rect_filled(bar, 2.0, GOLD);
                let inner = Rect::from_min_max(rect.min + Vec2::new(20.0, 10.0), rect.max - Vec2::new(12.0, 8.0));

                // Row 1: chest + last match (big), with its placement and eliminations; REC on the right.
                chest_icon(painter, Rect::from_min_size(inner.min + Vec2::new(0.0, 8.0), Vec2::new(26.0, 22.0)), GOLD);
                let last_text = d.last.as_ref().map_or("-".to_string(), |l| l.chests.to_string());
                let big = painter.text(inner.min + Vec2::new(34.0, -4.0), Align2::LEFT_TOP, last_text, FontId::proportional(42.0), GOLD);
                painter.text(egui::pos2(big.max.x + 6.0, big.max.y - 12.0), Align2::LEFT_BOTTOM, "LAST MATCH", FontId::proportional(9.5), MUTED);
                if let Some(e) = d.last.as_ref().and_then(|l| l.elims).filter(|&e| e > 0) {
                    painter.text(egui::pos2(big.max.x + 6.0, big.max.y - 1.0), Align2::LEFT_BOTTOM, format!("{e} elim"), FontId::proportional(9.5), MUTED);
                }
                let mut right = inner.max.x;
                if recording {
                    let pill = Rect::from_min_size(egui::pos2(right - 50.0, inner.min.y + 2.0), Vec2::new(50.0, 18.0));
                    painter.rect_filled(pill, 9.0, fade(RED, 0.16));
                    painter.circle_filled(egui::pos2(pill.min.x + 11.0, pill.center().y), 3.0 + 1.5 * rec_pulse, fade(RED, 0.6 + 0.4 * rec_pulse));
                    painter.text(egui::pos2(pill.min.x + 19.0, pill.center().y), Align2::LEFT_CENTER, "REC", FontId::proportional(10.0), RED);
                    right = pill.min.x - 6.0;
                }
                if let Some(p) = d.last.as_ref().and_then(|l| l.place) {
                    let (text, color) = if p == 1 { ("WIN".to_string(), GOLD) } else if p <= 10 { (format!("#{p}"), GREEN) } else { (format!("#{p}"), MUTED) };
                    let pill = Rect::from_min_size(egui::pos2(right - 46.0, inner.min.y + 2.0), Vec2::new(46.0, 18.0));
                    painter.rect_filled(pill, 9.0, fade(color, 0.14));
                    painter.text(pill.center(), Align2::CENTER_CENTER, text, FontId::proportional(10.0), color);
                }

                // Row 2: match result for a moment, else total / today / session / average.
                let row2 = inner.min.y + 56.0;
                match fresh {
                    Some(n) => {
                        painter.text(egui::pos2(inner.min.x, row2), Align2::LEFT_TOP, "MATCH OVER", FontId::proportional(9.5), MUTED);
                        painter.text(egui::pos2(inner.min.x, row2 + 12.0), Align2::LEFT_TOP, format!("+{n} chests opened"), FontId::proportional(17.0), GOLD);
                    }
                    None => {
                        let cols = [
                            ("TOTAL", total_shown.to_string(), GOLD),
                            ("TODAY", d.today.to_string(), TEXT),
                            ("SESSION", d.session.to_string(), TEXT),
                            ("AVG", format!("{:.1}", d.average), TEXT),
                        ];
                        let w = inner.width() / 4.0;
                        for (i, (label, value, color)) in cols.iter().enumerate() {
                            let x = inner.min.x + w * i as f32;
                            painter.text(egui::pos2(x, row2), Align2::LEFT_TOP, *label, FontId::proportional(9.0), MUTED);
                            painter.text(egui::pos2(x, row2 + 12.0), Align2::LEFT_TOP, value, FontId::proportional(16.0), *color);
                        }
                    }
                }

                // Row 3: the last 10 matches as bars (newest on the right).
                let spark_top = inner.min.y + 96.0;
                let spark_bottom = inner.max.y - 4.0;
                if d.spark.is_empty() {
                    painter.text(egui::pos2(inner.min.x, spark_bottom), Align2::LEFT_BOTTOM, "No matches yet", FontId::proportional(9.5), MUTED);
                } else {
                    let slot = inner.width() / d.spark.len() as f32;
                    for (i, grow) in spark_grow.iter().enumerate() {
                        let x = inner.min.x + slot * i as f32;
                        let h = ((spark_bottom - spark_top) * grow).max(2.0);
                        let is_last = i + 1 == d.spark.len();
                        let color = if is_last { GOLD } else { fade(GOLD, 0.3) };
                        let b = Rect::from_min_max(egui::pos2(x + slot * 0.2, spark_bottom - h), egui::pos2(x + slot * 0.8, spark_bottom));
                        painter.rect_filled(b, 2.0, color);
                    }
                }
                if !locked {
                    painter.text(egui::pos2(inner.max.x, rect.min.y + 6.0), Align2::RIGHT_TOP, lock_hint.as_str(), FontId::proportional(8.5), GREEN);
                }
            });
        });
        if !self.overlay_unrounded {
            self.overlay_unrounded = win11::unround("Chest Counter overlay");
            if !self.overlay_unrounded {
                ctx.request_repaint_after(Duration::from_millis(100)); // window not created yet
            }
        }
        if let Some(p) = new_pos {
            self.history.overlay.pos = Some(p);
            self.save();
        }
        if hide {
            self.history.overlay.enabled = false;
            self.save();
        }
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // lets the overlay window be see-through
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        self.reset_dialog(ctx);
        // Remember where the main window was left (saved once the mouse is released, not every frame).
        if ctx.input(|i| i.pointer.any_released()) {
            if let Some(r) = ctx.input(|i| i.viewport().outer_rect) {
                let p = [r.min.x, r.min.y];
                if self.history.window_pos != Some(p) {
                    self.history.window_pos = Some(p);
                    self.save();
                }
            }
        }
        let t = ctx.input(|i| i.time) as f32;
        let (at, st) = match self.stats_cache.take() {
            Some((at, s)) if !self.stats_dirty && at.elapsed() < Duration::from_secs(5) => (at, s),
            _ => (Instant::now(), stats(&self.history, &self.session)),
        };
        self.stats_dirty = false;
        let not_counted = self.history.matches.len().saturating_sub(st.matches);

        // Animations that need frames (pulses, progress shine, banner, overlay REC). Idle = no redraws.
        let scanning = matches!(self.scan, Scan::Reading { .. });
        let celebrating = self.result_age().is_some_and(|a| a < CELEBRATE);
        if self.recording || scanning || celebrating || self.history.overlay.enabled {
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        let glow = self.result_age().map_or(0.0, |a| (1.0 - a / 3.0).clamp(0.0, 1.0));
        let spark: Vec<u32> = st.recent.iter().take(10).rev().map(|m| m.chests_opened).collect();
        let overlay_data = OverlayData {
            total: st.total,
            session: st.session,
            today: st.today,
            average: st.average,
            last: st.last.as_ref().map(|m| LastSummary { chests: m.chests_opened, place: m.placement, elims: m.eliminations }),
            spark,
        };

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin::symmetric(12.0, 10.0)))
            .show(ctx, |ui| {
                self.header(ui, t);
                ui.add_space(8.0);
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    self.result_banner(ui);

                    // Hero: total counts up, the card glows gold for a few seconds after a match.
                    let border = mix(BORDER, GOLD, glow);
                    egui::Frame::none()
                        .fill(CARD)
                        .stroke(Stroke::new(1.0_f32, border))
                        .rounding(14.0)
                        .inner_margin(egui::Margin::same(12.0))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    ui.label(caption("Total chests"));
                                    let shown = ui.ctx().animate_value_with_time(Id::new("hero_total"), st.total as f32, 1.0).round() as u32;
                                    ui.label(RichText::new(shown.to_string()).size(38.0).strong().color(GOLD));
                                    ui.label(RichText::new(format!("{} matches · avg {:.1}", st.matches, st.average)).size(10.5).color(MUTED));
                                });
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                                    let (icon, _) = ui.allocate_exact_size(Vec2::new(44.0, 38.0), Sense::hover());
                                    let bob = (t * 1.6).sin() * 2.0;
                                    chest_icon(ui.painter(), icon.translate(Vec2::new(0.0, bob)), GOLD);
                                });
                            });
                            ui.add_space(8.0);
                            let recent: Vec<u32> = st.recent.iter().take(12).rev().map(|m| m.chests_opened).collect();
                            sparkline(ui, &recent, 44.0, "hero_spark");
                            ui.label(RichText::new("Last 12 matches").size(9.0).color(MUTED));
                        });
                    ui.add_space(8.0);

                    // Stat cards
                    let last_place = st.last.as_ref().and_then(|m| m.placement).map_or("-".to_string(), |p| format!("#{p}"));
                    let last_detail = st.last.as_ref().and_then(|m| m.eliminations).filter(|&e| e > 0).map_or(String::new(), |e| format!("{e} elim"));
                    let best_day_detail = st.best_day.1.map(|d| d.format("%b %-d").to_string()).unwrap_or_default();
                    let tiles: [(&str, &str, String, String, Color32); 6] = [
                        ("today", "Today", st.today.to_string(), String::new(), TEXT),
                        ("session", "Session", st.session.to_string(), format!("{} matches", st.session_matches), TEXT),
                        ("avg", "Average", format!("{:.1}", st.average), "per match".into(), TEXT),
                        ("best", "Best match", st.best_match.to_string(), String::new(), GOLD),
                        ("bestday", "Best day", st.best_day.0.to_string(), best_day_detail, GOLD),
                        ("place", "Last place", last_place, last_detail, TEXT),
                    ];
                    for row in tiles.chunks(2) {
                        ui.columns(2, |cols| {
                            for (col, (key, title, value, detail, accent)) in cols.iter_mut().zip(row) {
                                tile(col, key, title, value.clone(), detail.clone(), *accent);
                            }
                        });
                    }
                    ui.add_space(4.0);

                    card(ui, |ui| {
                        ui.label(caption("By type"));
                        ui.add_space(4.0);
                        let kinds = [analyzer::NORMAL, analyzer::RARE, analyzer::BURIED, analyzer::EXTRACTION, analyzer::POSTER_CHEST, analyzer::OTHER_CHEST];
                        let max = st.by_kind.values().copied().max().unwrap_or(0).max(1) as f32;
                        ui.spacing_mut().item_spacing.y = 6.0;
                        for k in kinds {
                            type_row(ui, kind_label(k), st.by_kind.get(k).copied().unwrap_or(0), kind_color(k), max);
                        }
                    });
                    ui.add_space(8.0);

                    card(ui, |ui| {
                        ui.label(caption("Last 7 days"));
                        bar_chart(ui, &st.last7);
                    });
                    ui.add_space(8.0);

                    card(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(caption("Recent matches"));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(RichText::new(format!("{} counted", st.matches)).size(10.0).color(MUTED));
                            if ui.small_button("Open folder").on_hover_text("Open the Fortnite replays folder").clicked() {
                                let _ = std::process::Command::new("explorer").arg(replays_dir()).spawn();
                            }
                            });
                        });
                        if st.recent.is_empty() {
                            ui.add_space(8.0);
                            ui.vertical_centered(|ui| {
                                ui.label(RichText::new("No matches yet").strong());
                                ui.label(
                                    RichText::new("Play a Battle Royale match with Settings → Game → Replays → Record Replays on. It shows up here when the match ends.")
                                        .size(11.0)
                                        .color(MUTED),
                                );
                            });
                            ui.add_space(8.0);
                        }
                        ui.spacing_mut().item_spacing.y = 5.0;
                        for m in st.recent.iter().take(12) {
                            match_row(ui, m);
                        }
                        if st.recent.len() > 12 {
                            ui.label(RichText::new(format!("+ {} older", st.recent.len() - 12)).size(10.0).color(MUTED));
                        }
                    });

                    if not_counted > 0 {
                        ui.add_space(6.0);
                        ui.label(RichText::new(format!("{not_counted} other matches not counted (before v43, other modes, or before your last reset).")).size(10.0).color(MUTED));
                    }
                    egui::CollapsingHeader::new(caption("Updates")).default_open(false).show(ui, |ui| {
                        ui.label(RichText::new(format!("Version {}", updater::CURRENT)).size(11.0).color(MUTED));
                        if ui.button("Check for updates").clicked() {
                            updater::check_now(self.update_tx.clone(), ui.ctx().clone());
                        }
                        if let Some(n) = &self.update_note {
                            ui.label(RichText::new(n).size(10.5).color(MUTED));
                        }
                    });
                    egui::CollapsingHeader::new(caption("Hotkeys")).default_open(false).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Show / hide").size(11.0));
                            ui.add(egui::TextEdit::singleline(&mut self.hotkey_edit.0).desired_width(120.0));
                        });
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Lock / move").size(11.0));
                            ui.add(egui::TextEdit::singleline(&mut self.hotkey_edit.1).desired_width(120.0));
                        });
                        if ui.button("Apply").clicked() {
                            let (t, l) = (self.hotkey_edit.0.trim().to_string(), self.hotkey_edit.1.trim().to_string());
                            match self.bind_hotkeys(&t, &l) {
                                Ok(()) => {
                                    self.history.overlay.toggle_key = t;
                                    self.history.overlay.lock_key = l;
                                    self.save();
                                }
                                Err(e) => self.hotkey_err = Some(e),
                            }
                        }
                        if let Some(e) = &self.hotkey_err {
                            ui.label(RichText::new(e).size(10.5).color(RED));
                        }
                    });
                    if let Some(n) = &self.notice {
                        ui.label(RichText::new(n).size(10.0).color(MUTED));
                    }
                    for (file, e) in &self.failures {
                        ui.label(RichText::new(format!("Could not read {file}: {e}")).size(10.0).color(RED));
                    }
                });
            });

        self.overlay(ctx, &overlay_data);
        self.stats_cache = Some((at, st));
    }
}

fn main() -> eframe::Result<()> {
    updater::cleanup(); // remove the exe left behind by the last update
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([270.0, 410.0])
            .with_min_inner_size([270.0, 410.0])
            .with_max_inner_size([270.0, 410.0])
            .with_resizable(false)
            .with_maximize_button(false)
            .with_title("Chest Counter"),
        ..Default::default()
    };
    if let Some([x, y]) = History::load().window_pos {
        options.viewport = options.viewport.with_position([x, y]);
    }
    eframe::run_native("Chest Counter", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

/// Windows 11 rounds borderless windows with its own 8px corner and grey border, which clash with
/// the overlay card's own rounded edge. Turn the border off and keep the system rounding.
#[cfg(windows)]
mod win11 {
    #[link(name = "user32")]
    extern "system" {
        fn FindWindowW(class: *const u16, title: *const u16) -> isize;
    }
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(hwnd: isize, attr: u32, value: *const u32, size: u32) -> i32;
    }
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWA_BORDER_COLOR: u32 = 34;
    const DWMWCP_ROUND: u32 = 2;
    const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;

    /// True once the window exists and was updated (older Windows ignores the attributes, also fine).
    pub fn unround(title: &str) -> bool {
        let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
        // SAFETY: null-terminated UTF-16 title; values are u32 as the DWM attributes expect.
        unsafe {
            let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
            if hwnd == 0 {
                return false;
            }
            DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &DWMWCP_ROUND, 4);
            DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &DWMWA_COLOR_NONE, 4);
        }
        true
    }
}

#[cfg(not(windows))]
mod win11 {
    pub fn unround(_title: &str) -> bool {
        true
    }
}
