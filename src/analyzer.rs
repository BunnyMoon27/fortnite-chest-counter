//! Chest counting for one replay: port of ChestCounter's ReplayAnalyzer (chest part) and the bits of
//! FortniteReplayBuilder it relies on (players, pawns, game state).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::replay::bytes::Bytes;
use crate::replay::{Abort, Ctx, Export, Handler, Parser, PlayerStateExport};

/// Bump when the analysis changes so stored matches are analyzed again.
pub const ANALYZER_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ChestRecord {
    pub time_seconds: f32,
    pub kind: String,
    pub blueprint: String,
    pub destroyed: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct TeammateRecord {
    pub name: String,
    pub epic_id: Option<String>,
    pub chests_seen: u32,
    pub kinds: BTreeMap<String, u32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct MatchRecord {
    pub session_id: String,
    pub replay_file: String,
    pub analyzed_at_unix: i64,
    pub match_start_unix: Option<i64>,
    pub duration_seconds: Option<u32>,
    pub playlist: Option<String>,
    pub game_version: Option<String>,
    pub player_name: Option<String>,
    pub epic_id: Option<String>,
    pub placement: Option<i32>,
    pub total_players: Option<u32>,
    pub is_replay_owner: bool,
    pub eliminations: Option<u32>,
    pub chests_opened: u32,
    pub ammo_boxes_opened: u32,
    pub other_containers_opened: u32,
    pub other_interactions: BTreeMap<String, u32>,
    pub analyzer_version: u32,
    pub is_battle_royale: bool,
    pub is_partial: bool,
    pub chests: Vec<ChestRecord>,
    pub teammates: Vec<TeammateRecord>,
}

// --- chest kinds (ChestKinds) ---------------------------------------------------------------------------------

pub const NORMAL: &str = "Chest";
pub const RARE: &str = "Rare chest";
pub const BURIED: &str = "Buried chest";
pub const EXTRACTION: &str = "Extraction / relic chest";
pub const POSTER_CHEST: &str = "Spirit poster chest";
pub const OTHER_CHEST: &str = "Other chest";
pub const CANNON: &str = "Loot cannon";
pub const AMMO_BOX: &str = "Ammo box";
pub const FOOD_BOX: &str = "Food box";
pub const OTHER_CONTAINER: &str = "Other container";
pub const POSTER: &str = "Spirit wanted poster";
pub fn is_chest(kind: &str) -> bool {
    [NORMAL, RARE, BURIED, EXTRACTION, POSTER_CHEST, OTHER_CHEST].contains(&kind)
}

pub fn classify(blueprint: &str) -> &'static str {
    let b = blueprint.to_lowercase();
    let has = |s: &str| b.contains(s);
    if has("lootcannon") || has("cannon") {
        CANNON
    } else if has("ammo") {
        AMMO_BOX
    } else if has("foodbox") || has("food_box") {
        FOOD_BOX
    } else if has("rarechest") || has("legendary") || has("rare_chest") {
        RARE
    } else if has("buried") {
        BURIED
    } else if has("spriteposter") && !has("chest") {
        POSTER
    } else if has("spriteposter") || has("booty") {
        POSTER_CHEST
    } else if has("relic") || has("extraction") {
        EXTRACTION
    } else if has("tiered_chest") || has("normalchest") || has("standardchest") {
        NORMAL
    } else if has("chest") {
        OTHER_CHEST
    } else {
        OTHER_CONTAINER
    }
}

/// Last path segment, after the last '.', without the "_UAID_..." suffix.
pub fn clean_name(path: &str) -> String {
    let name = &path[path.rfind('/').map_or(0, |i| i + 1)..];
    let name = &name[name.rfind('.').map_or(0, |i| i + 1)..];
    match name.find("_UAID_") {
        Some(i) => name[..i].to_string(),
        None => name.to_string(),
    }
}

// --- playlist / version rules (Profiles) -----------------------------------------------------------------------

/// Explicit standard Battle Royale playlists; unknown modes do not count.
pub fn is_battle_royale_playlist(playlist: Option<&str>) -> bool {
    let p = playlist.unwrap_or("");
    let last = p.rsplit('/').next().unwrap_or("");
    let name = last.rsplit('.').next().unwrap_or("").to_lowercase();
    let Some(rest) = name.strip_prefix("playlist_") else { return false };
    let sizes = ["solo", "duo", "trio", "squad"];
    let with_size = |prefix: &str| sizes.iter().any(|s| rest == format!("{prefix}{s}"));
    rest == "trios"
        || with_size("default")
        || with_size("habanero")
        || with_size("habanerobr_")
        || with_size("habanero_nobuildbr_")
        || with_size("nobuildbr_")
        || with_size("ranked_")
        || with_size("rankednobuildbr_")
        || with_size("nobuildbr_ranked_")
}

pub const MIN_FORTNITE_MAJOR_VERSION: u32 = 43;

/// Major version from a branch like "++Fortnite+Release-43.00" (regex `(?:Release-|v|\b)(\d+)\.(\d+)`).
pub fn fortnite_major_version(version: Option<&str>) -> Option<u32> {
    let s = version?.as_bytes();
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    for start in 0..s.len() {
        if !s[start].is_ascii_digit() {
            continue;
        }
        let ok_start = start == 0 || !is_word(s[start - 1]) || (s[start - 1] | 0x20) == b'v';
        if !ok_start {
            continue;
        }
        let mut end = start;
        while end < s.len() && s[end].is_ascii_digit() {
            end += 1;
        }
        if end + 1 < s.len() && s[end] == b'.' && s[end + 1].is_ascii_digit() {
            return std::str::from_utf8(&s[start..end]).ok()?.parse().ok();
        }
    }
    None
}

/// Season gate of the original: only v43+ matches count (unknown version: matches from Nov 2026 on).
pub fn is_supported_version(m: &MatchRecord) -> bool {
    if let Some(major) = fortnite_major_version(m.game_version.as_deref()) {
        return major >= MIN_FORTNITE_MAJOR_VERSION;
    }
    const NOV_1_2026_UTC: i64 = 1_793_491_200;
    m.match_start_unix.unwrap_or(m.analyzed_at_unix) >= NOV_1_2026_UTC
}

pub fn counted(m: &MatchRecord) -> bool {
    m.is_battle_royale && is_supported_version(m)
}

// --- dates -------------------------------------------------------------------------------------------------------

/// .NET ticks (100 ns since 0001-01-01) to Unix seconds; the two DateTimeKind bits are ignored.
pub fn ticks_to_unix(ticks: i64) -> Option<i64> {
    let t = ticks & 0x3FFF_FFFF_FFFF_FFFF;
    (t > 0).then(|| (t - 621_355_968_000_000_000).div_euclid(10_000_000))
}

// --- replay handler ----------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct PlayerData {
    id: Option<i32>,
    epic_id: Option<String>,
    is_bot: bool,
    team_index: Option<i32>,
    placement: Option<i32>,
    name: Option<String>,
}

struct SearchEvent {
    time: f32,
    label: String,
    searcher: Option<u32>,
    destroyed: bool,
}

#[derive(Default)]
struct Analyzer {
    // FortniteReplayBuilder
    actor_to_channel: HashMap<u32, u32>,
    channel_to_actor: HashMap<u32, u32>,
    queued_pawns: HashMap<u32, Vec<(u32, Option<u32>)>>,
    spectators: HashSet<u32>,
    players: Vec<(u32, PlayerData)>, // keyed by channel, insertion order
    recorder_id: Option<u32>,
    session_id: Option<String>,
    utc_started_ticks: Option<u64>,
    playlist: Option<String>,
    // ReplayAnalyzer
    channel_key: HashMap<u32, String>,
    channel_label: HashMap<u32, String>,
    last_searcher: HashMap<u32, u32>,
    containers_observed: HashSet<u32>,
    searched: HashSet<String>,
    searches: Vec<SearchEvent>,
    pawn_to_player_state: HashMap<u32, u32>,
    player_state_to_player_id: HashMap<u32, i32>,
}

fn player_id(s: &PlayerStateExport) -> Option<i32> {
    s.player_id_uint.map(|v| v as i32).or(s.player_id_int)
}

impl Analyzer {
    fn player_index(&self, ch: u32) -> Option<usize> {
        self.players.iter().position(|(c, _)| *c == ch)
    }

    fn update_player_state(&mut self, ch: u32, s: &PlayerStateExport) -> Result<(), Abort> {
        if s.only_spectator == Some(true) {
            self.spectators.insert(ch);
            return Ok(());
        }
        if self.spectators.contains(&ch) {
            return Ok(());
        }
        let is_new = self.player_index(ch).is_none();
        if is_new {
            let p = PlayerData {
                id: player_id(s),
                epic_id: s.unique_id.clone().or_else(|| s.unique_id_alt.clone()),
                is_bot: s.is_bot == Some(true),
                team_index: s.team_index,
                ..Default::default()
            };
            self.players.push((ch, p));
        }
        let i = self.player_index(ch).expect("just inserted");
        let p = &mut self.players[i].1;
        if s.team_index.is_some_and(|t| t > 0) {
            p.team_index = s.team_index;
        }
        if p.placement.is_none() {
            p.placement = s.place;
        }
        if is_new {
            if let Some(actor) = self.channel_to_actor.get(&ch).copied() {
                for (pawn_ch, ps) in self.queued_pawns.remove(&actor).unwrap_or_default() {
                    self.update_player_pawn(pawn_ch, ps)?;
                }
            }
        }
        Ok(())
    }

    fn update_player_pawn(&mut self, ch: u32, player_state: Option<u32>) -> Result<(), Abort> {
        let Some(actor) = player_state else { return Ok(()) };
        match self.actor_to_channel.get(&actor) {
            Some(&state_ch) => {
                // C#: `_players[stateChannelIndex]` throws when that channel holds no player.
                if self.player_index(state_ch).is_none() {
                    return Err(Abort("pawn's player state is not a player".into()));
                }
            }
            None => self.queued_pawns.entry(actor).or_default().push((ch, player_state)),
        }
        Ok(())
    }
}

impl Handler for Analyzer {
    fn on_channel_opened(&mut self, ctx: &Ctx, ch: u32) {
        let actor_id = ctx.actor_id(ch);
        if let Some(a) = actor_id {
            self.actor_to_channel.insert(a, ch);
            self.channel_to_actor.insert(ch, a);
        }
        let archetype = ctx.path_of(ctx.archetype_id(ch));
        let actor_path = ctx.path_of(actor_id);
        // Level-placed actors have no archetype; their actor path is unique per chest.
        let key = if !actor_path.is_empty() { actor_path.clone() } else { format!("guid:{}", actor_id.map(|a| a.to_string()).unwrap_or_default()) };
        self.channel_key.insert(ch, key);
        self.channel_label.insert(ch, clean_name(if !archetype.is_empty() { &archetype } else { &actor_path }));
        self.last_searcher.remove(&ch);
        self.containers_observed.remove(&ch);
    }

    fn on_export(&mut self, ctx: &Ctx, ch: u32, export: &Export) -> Result<(), Abort> {
        // FortniteReplayReader.OnExportRead (builder) runs first; its exception skips the analyzer part.
        match export {
            Export::GameState { recorder, session_id, utc_started_ticks } => {
                self.session_id = self.session_id.take().or_else(|| session_id.clone());
                self.utc_started_ticks = self.utc_started_ticks.or(*utc_started_ticks);
                self.recorder_id = self.recorder_id.or(*recorder);
            }
            Export::Playlist { name } => self.playlist = self.playlist.take().or_else(|| name.clone()),
            Export::PlayerState(s) => self.update_player_state(ch, s)?,
            Export::PlayerPawn { player_state } => self.update_player_pawn(ch, *player_state)?,
            _ => {}
        }

        match export {
            Export::Container { searching_pawn, already_searched, destroyed } => {
                self.containers_observed.insert(ch);
                if let Some(s) = searching_pawn.filter(|&s| s != 0) {
                    self.last_searcher.insert(ch, s);
                }
                let key = self.channel_key.get(&ch).cloned().unwrap_or_else(|| format!("ch:{ch}"));
                let label = self.channel_label.get(&ch).cloned().unwrap_or_else(|| "?".into());
                if *already_searched {
                    if self.searched.insert(key) {
                        self.searches.push(SearchEvent { time: ctx.time, label, searcher: self.last_searcher.get(&ch).copied(), destroyed: false });
                    }
                } else if let Some(destroyer) = searching_pawn.filter(|&s| s != 0 && *destroyed) {
                    // Broken instead of opened: the loot spills out and SearchingPawn is whoever destroyed it.
                    if self.searched.insert(key) {
                        self.searches.push(SearchEvent { time: ctx.time, label, searcher: Some(destroyer), destroyed: true });
                    }
                }
            }
            Export::PlayerPawn { player_state: Some(ps) } if *ps != 0 => {
                if let Some(pawn) = ctx.actor_id(ch) {
                    self.pawn_to_player_state.insert(pawn, *ps);
                }
            }
            Export::PlayerState(s) => {
                if let (Some(ps), Some(pid)) = (ctx.actor_id(ch), player_id(s)) {
                    self.player_state_to_player_id.insert(ps, pid);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn on_external_data(&mut self, _ctx: &Ctx, ch: u32, data: &[u8]) -> Result<(), Abort> {
        let name = decode_player_name(data).map_err(Abort)?;
        if let Some(i) = self.player_index(ch) {
            self.players[i].1.name = Some(name);
        }
        Ok(())
    }
}

/// PlayerNameData: Fortnite obfuscates names in the replay with a position-dependent shift.
fn decode_player_name(data: &[u8]) -> Result<String, String> {
    let mut ar = Bytes::new(data.to_vec());
    ar.u8()?; // handle
    ar.u8()?; // unknown
    let is_player = ar.bool()?;
    let encoded = ar.fstring()?;
    if !is_player {
        return Ok(encoded);
    }
    let units: Vec<u16> = encoded.encode_utf16().collect();
    let len = units.len();
    let decoded: Vec<u16> = units.iter().enumerate().map(|(i, &c)| c.wrapping_add((((len % 4 * 3 % 8) + 1 + i) * 3 % 8) as u16)).collect();
    Ok(String::from_utf16_lossy(&decoded))
}

fn round1(x: f32) -> f32 {
    (x * 10.0).round_ties_even() / 10.0
}

/// Parses `path` and returns what the replay owner did with chests.
pub fn analyze_file(path: &Path) -> Result<MatchRecord, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let file = path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
    analyze_bytes(data, &file)
}

pub fn analyze_bytes(data: Vec<u8>, file: &str) -> Result<MatchRecord, String> {
    let (meta, a) = Parser::new(Analyzer::default()).parse(data)?;

    // --- Who are we tracking? The replay owner (recording player).
    let players: Vec<&PlayerData> = a.players.iter().map(|(_, p)| p).collect();
    let recorder_ps = a.recorder_id;
    let target = recorder_ps
        .and_then(|ps| a.player_state_to_player_id.get(&ps))
        .and_then(|pid| players.iter().copied().find(|p| p.id == Some(*pid)));

    let mut target_states: HashSet<u32> = HashSet::new();
    if let Some(tid) = target.and_then(|t| t.id) {
        target_states.extend(a.player_state_to_player_id.iter().filter(|(_, &v)| v == tid).map(|(&k, _)| k));
    }
    if target_states.is_empty() {
        if let Some(ps) = recorder_ps {
            target_states.insert(ps);
        }
    }
    let is_recorder = recorder_ps.is_some_and(|ps| target_states.contains(&ps));
    let pawns_of = |states: &HashSet<u32>| -> HashSet<u32> {
        a.pawn_to_player_state.iter().filter(|(_, ps)| states.contains(ps)).map(|(&pawn, _)| pawn).collect()
    };
    let target_pawns = pawns_of(&target_states);

    // --- Everything the target searched.
    let mut interactions: Vec<&SearchEvent> = a.searches.iter().filter(|s| s.searcher.is_some_and(|p| target_pawns.contains(&p))).collect();
    interactions.sort_by(|x, y| x.time.total_cmp(&y.time));

    let mut chests = Vec::new();
    let mut others: BTreeMap<String, u32> = BTreeMap::new();
    for s in &interactions {
        let kind = classify(&s.label);
        if !is_chest(kind) {
            // Keep the blueprint for unknown containers so new kinds (vaults) can be recognised.
            let key = if kind == OTHER_CONTAINER { format!("{kind}: {}", s.label) } else { kind.to_string() };
            *others.entry(key).or_default() += 1;
            continue;
        }
        chests.push(ChestRecord { time_seconds: round1(s.time), kind: kind.into(), blueprint: s.label.clone(), destroyed: s.destroyed });
    }

    // --- Teammates: same team, with what they were seen searching (only what happened near the recorder).
    let mut teammates = Vec::new();
    if let (Some(t), Some(team)) = (target, target.and_then(|t| t.team_index)) {
        let mut seen_keys = HashSet::new();
        for mate in players.iter().filter(|p| p.team_index == Some(team) && p.id != t.id && !p.is_bot) {
            let key = mate.epic_id.clone().or_else(|| mate.name.clone()).unwrap_or_else(|| format!("pid{}", mate.id.map(|i| i.to_string()).unwrap_or_default()));
            if !seen_keys.insert(key) {
                continue;
            }
            let states: HashSet<u32> = a.player_state_to_player_id.iter().filter(|(_, &v)| Some(v) == mate.id).map(|(&k, _)| k).collect();
            let pawns = pawns_of(&states);
            let mut kinds: BTreeMap<String, u32> = BTreeMap::new();
            for s in a.searches.iter().filter(|s| s.searcher.is_some_and(|p| pawns.contains(&p))) {
                *kinds.entry(classify(&s.label).to_string()).or_default() += 1;
            }
            let chests_seen = kinds.iter().filter(|(k, _)| is_chest(k)).map(|(_, v)| v).sum();
            let name = mate.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Teammate").to_string();
            teammates.push(TeammateRecord { name, epic_id: mate.epic_id.clone(), chests_seen, kinds });
        }
        teammates.sort_by(|x, y| y.chests_seen.cmp(&x.chests_seen));
    }

    let info_start = ticks_to_unix(meta.info.timestamp_ticks);
    let match_start = a.utc_started_ticks.and_then(|t| ticks_to_unix(t as i64)).or(info_start);
    let session_id = a.session_id.clone().unwrap_or_else(|| format!("{}-{file}", info_start.unwrap_or(0)));
    let placement = target.and_then(|t| t.placement).or(if is_recorder { meta.team_stats.map(|t| t.0 as i32) } else { None });
    let ammo = others.get(AMMO_BOX).copied().unwrap_or(0);
    let other_containers = others.iter().filter(|(k, _)| k.as_str() != AMMO_BOX).map(|(_, v)| v).sum();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);

    Ok(MatchRecord {
        session_id,
        replay_file: file.to_string(),
        analyzed_at_unix: now,
        match_start_unix: match_start,
        duration_seconds: Some(meta.info.length_ms / 1000),
        is_battle_royale: is_battle_royale_playlist(a.playlist.as_deref()),
        playlist: a.playlist.clone(),
        game_version: Some(meta.header.branch.clone()).filter(|b| !b.is_empty()),
        player_name: target.and_then(|t| t.name.clone()),
        epic_id: target.and_then(|t| t.epic_id.clone()),
        placement,
        total_players: meta.team_stats.map(|t| t.1),
        is_replay_owner: is_recorder,
        eliminations: if is_recorder { meta.eliminations } else { None },
        chests_opened: chests.len() as u32,
        ammo_boxes_opened: ammo,
        other_containers_opened: other_containers,
        other_interactions: others,
        analyzer_version: ANALYZER_VERSION,
        // A replay still being recorded (live flag) is not final yet either.
        is_partial: meta.is_partial || meta.info.is_live,
        chests,
        teammates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(classify("Tiered_Chest_Athena_C"), NORMAL);
        assert_eq!(classify("Tiered_Ammo_Athena_C"), AMMO_BOX);        assert_eq!(classify("RareChest_Athena_C"), RARE);
        assert_eq!(classify("B_SpritePoster_C"), POSTER);
        assert_eq!(classify("Barrel_FishingRod"), OTHER_CONTAINER);
        assert_eq!(clean_name("/Game/X/Tiered_Chest_Athena.Tiered_Chest_Athena_C"), "Tiered_Chest_Athena_C");
        assert_eq!(clean_name("Tiered_Chest_6_UAID_0123"), "Tiered_Chest_6");
    }

    #[test]
    fn playlist_and_version() {
        assert!(is_battle_royale_playlist(Some("/Game/Athena/Playlists/Playlist_DefaultSquad.Playlist_DefaultSquad")));
        assert!(is_battle_royale_playlist(Some("Playlist_NoBuildBR_Ranked_Duo")));
        assert!(!is_battle_royale_playlist(Some("Playlist_Creative")));
        assert_eq!(fortnite_major_version(Some("++Fortnite+Release-43.00")), Some(43));
        assert_eq!(fortnite_major_version(Some("v41.10")), Some(41));
        assert_eq!(fortnite_major_version(Some("abc12.3")), None);
    }

    #[test]
    fn name_decoding() {
        // Encoded with the inverse shift of "Abc": len 3 -> base (3*3%8 + 1) = 2.
        let plain: Vec<u16> = "Abc".encode_utf16().collect();
        let enc: String = String::from_utf16_lossy(&plain.iter().enumerate().map(|(i, &c)| c - (((2 + i) * 3 % 8) as u16)).collect::<Vec<_>>());
        let mut data = vec![0u8, 0, 1];
        data.extend_from_slice(&(enc.len() as i32 + 1).to_le_bytes());
        data.extend_from_slice(enc.as_bytes());
        data.push(0);
        assert_eq!(decode_player_name(&data).unwrap(), "Abc");
    }
}
