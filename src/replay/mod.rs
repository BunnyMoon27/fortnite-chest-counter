//! Fortnite replay parser: a port of the parts of FortniteReplayDecompressor (Unreal.Core + FortniteReplayReader,
//! with ChestCounter's patches) that the chest counter needs.
//!
//! Every replicated property arrives with its size in bits, so properties the counter does not use are skipped
//! instead of decoded; the stream stays in sync either way.

pub mod bits;
pub mod bytes;
mod names;
pub mod oodle;
mod versions;

use std::collections::{HashMap, HashSet};

use aes::cipher::{BlockDecrypt, KeyInit};
use bits::{BitReader, END_CONTENT_BLOCK, END_FIELD_HEADER};
use bytes::{Bytes, R};
use versions::*;

const FILE_MAGIC: u32 = 0x1CA2E27F;
const NETWORK_MAGIC: u32 = 0x2CF5A13D;
const MAX_CHANNELS: usize = 32767;
const MAX_GUID_COUNT: i32 = 2048;
const MAX_PACKET_BITS: u32 = 16384;

// ---------------------------------------------------------------------------------------------------------------
// Public results
// ---------------------------------------------------------------------------------------------------------------

#[derive(Default, Debug, Clone)]
pub struct Info {
    pub file_version: u32,
    pub length_ms: u32,
    pub is_live: bool,
    /// .NET DateTime ticks (DateTime.FromBinary), 0 when absent.
    pub timestamp_ticks: i64,
    pub is_compressed: bool,
    pub is_encrypted: bool,
    pub key: Vec<u8>,
}

#[derive(Default, Debug, Clone)]
pub struct Header {
    pub network_version: u32,
    pub engine_version: u32,
    pub changelist: u32,
    pub branch: String,
    pub flags: u32,
}

#[derive(Default, Debug, Clone)]
pub struct ReplayMeta {
    pub info: Info,
    pub header: Header,
    /// AthenaMatchTeamStats: (position, total players).
    pub team_stats: Option<(u32, u32)>,
    /// AthenaMatchStats: eliminations of the recording player.
    pub eliminations: Option<u32>,
    pub is_partial: bool,
}

/// Values decoded from one property update (`OnExportRead`). `None` = not sent in this update.
#[derive(Debug, Clone)]
pub enum Export {
    Container { searching_pawn: Option<u32>, already_searched: bool, destroyed: bool },
    PlayerPawn { player_state: Option<u32> },
    PlayerState(PlayerStateExport),
    GameState { recorder: Option<u32>, session_id: Option<String>, utc_started_ticks: Option<u64> },
    Playlist { name: Option<String> },
    Other,
}

#[derive(Debug, Clone, Default)]
pub struct PlayerStateExport {
    pub player_id_int: Option<i32>,
    pub player_id_uint: Option<u32>,
    pub is_bot: Option<bool>,
    pub only_spectator: Option<bool>,
    pub unique_id: Option<String>,
    pub unique_id_alt: Option<String>,
    pub team_index: Option<i32>,
    pub place: Option<i32>,
}

/// An error that aborts the current bunch (a C# exception caught by the packet loop).
#[derive(Debug)]
pub struct Abort(pub String);

/// Read-only view of the parser state for the handler callbacks.
pub struct Ctx<'a> {
    pub time: f32,
    channels: &'a [Option<Channel>],
    guid_to_path: &'a HashMap<u32, String>,
}

impl Ctx<'_> {
    pub fn actor_id(&self, ch: u32) -> Option<u32> {
        self.channels.get(ch as usize)?.as_ref()?.actor.as_ref().map(|a| a.guid)
    }
    pub fn archetype_id(&self, ch: u32) -> Option<u32> {
        self.channels.get(ch as usize)?.as_ref()?.actor.as_ref()?.archetype
    }
    /// Cleaned object path of a net GUID ("" when unknown), like `PathOf` in the analyzer.
    pub fn path_of(&self, guid: Option<u32>) -> String {
        match guid {
            Some(g) if g != 0 => self.guid_to_path.get(&g).cloned().unwrap_or_default(),
            _ => String::new(),
        }
    }
}

/// Handler that ignores everything (header peeks).
pub struct NoHandler;
impl Handler for NoHandler {
    fn on_channel_opened(&mut self, _: &Ctx, _: u32) {}
    fn on_export(&mut self, _: &Ctx, _: u32, _: &Export) -> Result<(), Abort> {
        Ok(())
    }
    fn on_external_data(&mut self, _: &Ctx, _: u32, _: &[u8]) -> Result<(), Abort> {
        Ok(())
    }
}

pub trait Handler {
    fn on_netguid_exported(&mut self, _time: f32, _guid: u32, _path: &str) {}
    fn on_channel_opened(&mut self, ctx: &Ctx, ch: u32);
    fn on_export(&mut self, ctx: &Ctx, ch: u32, export: &Export) -> Result<(), Abort>;
    fn on_external_data(&mut self, ctx: &Ctx, ch: u32, data: &[u8]) -> Result<(), Abort>;
}

// ---------------------------------------------------------------------------------------------------------------
// Export groups (which property groups the reader decodes, and how)
// ---------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Container,
    PlayerPawn,
    PlayerState,
    GameState,
    Other,
}

/// Groups the original registers in its default parse mode ("Normal"): only these are read.
fn registered_kind(path: &str) -> Option<Kind> {
    Some(match path {
        "/Game/Athena/Athena_GameState.Athena_GameState_C" => Kind::GameState,
        "/Game/Athena/PlayerPawn_Athena.PlayerPawn_Athena_C" => Kind::PlayerPawn,
        "/Script/FortniteGame.FortPlayerStateAthena" => Kind::PlayerState,
        "/DelilahTreasureHuntGameplay/Gameplay/Actors/BGA_DelilahTreasureHuntAnimatedChest.BGA_DelilahTreasureHuntAnimatedChest_C"
        | "/Game/Spectating/BP_ReplayPC_Athena.BP_ReplayPC_Athena_C"
        | "/Script/FortniteGame.ActiveGameplayModifier"
        | "/Script/FortniteGame.FortAbilitySystemComponentAthena"
        | "/LootCannon/Gameplay/BGA_LootCannon.BGA_LootCannon_C:FireLootCannon"
        | "/Game/Athena/SafeZone/SafeZoneIndicator.SafeZoneIndicator_C"
        | "/Script/FortniteGame.FortPickupAthena"
        | "/Script/FortniteGame.FortInventory"
        | "CurrentPlaylistInfo" => Kind::Other,
        _ => return None,
    })
}

const TREASURE_ACTORS: [&str; 8] =
    ["flameceleb", "relicextractor", "relicfinder", "luckylocator", "smugglerextractionkey", "extractionaccelerat", "pirateshop", "delilahshop"];

/// ChestCounter's AliasResolver: unknown chest/container blueprints read as a generic container.
fn alias_kind(path: &str) -> Option<Kind> {
    if path.contains(':') || path.ends_with("_ClassNetCache") {
        return None;
    }
    let last = &path[path.rfind('/').map_or(0, |i| i + 1)..];
    let last_lower = last.to_lowercase();
    if last.starts_with("B_") && TREASURE_ACTORS.iter().any(|t| last_lower.contains(t)) {
        return Some(Kind::Other); // GenericWeapon
    }
    let containers = path.to_lowercase().contains("/containers/") || last_lower.contains("chest") || last_lower.contains("spriteposter");
    containers.then_some(Kind::Container)
}

/// Property names declared on the C# export classes. A received property missing here is flagged
/// "incompatible" by the original and skipped from then on, which changes when an update counts as having data.
fn declared_fields(kind: Kind) -> Option<&'static [&'static str]> {
    match kind {
        Kind::Container => Some(&[
            "bHidden", "RemoteRole", "Role", "ForceMetadataRelevant", "bDestroyed", "bInstantDeath", "StaticMesh", "AltMeshIdx",
            "WeaponData", "BuildTime", "RepairTime", "Health", "MaxHealth", "SearchedMesh", "ReplicatedLootTier", "SearchingPawn",
            "bAlreadySearched", "BounceNormal", "SearchAnimationCount", "ChosenRandomUpgrade", "bMirrored", "ReplicatedDrawScale3D",
            "bIsInitiallyBuilding", "bForceReplayRollback",
        ]),
        Kind::PlayerState => Some(&[
            "RemoteRole", "Owner", "Role", "Instigator", "Score", "PlayerID", "PlayerId", "Ping", "bIsABot", "bIsSpectator",
            "bOnlySpectator", "StartTime", "UniqueId", "UniqueID", "PlayerNamePrivate", "bIsGameSessionOwner", "bHasFinishedLoading",
            "bHasStartedPlaying", "PlayerRole", "PartyOwnerUniqueId", "WorldPlayerId", "HeroType", "Platform", "CharacterGender",
            "CharacterBodyType", "WasReplicatedFlags", "Parts", "WasPartReplicatedFlags", "RequiredVariantPartFlags",
            "VariantRequiredCharacterParts", "PlayerTeamPrivate", "PlatformUniqueNetId", "TeamIndex", "Place",
            "ReplicatedTeamMemberState", "bHasEverSkydivedFromBus", "bHasEverSkydivedFromBusAndLanded", "SquadListUpdateValue",
            "SquadId", "bInAircraft", "bThankedBusDriver", "bDidNotThankBusDriver", "TeamKillScore", "bUsingStreamerMode",
            "StreamerModeName", "PlayerNameCustomOverride", "TeamScorePlacement", "IconId", "TeamScore", "ColorId", "Level",
            "MapIndicatorPos", "KillScore", "FinisherOrDowner", "SeasonLevelUIDisplay", "bInitialized", "DeathCircumstance",
            "bUsingAnonymousMode", "bIsDisconnected", "bDBNO", "DeathCause", "Distance", "DeathTags", "VictimTags",
            "FinisherOrDownerTags", "bResurrectionChipAvailable", "ResurrectionExpirationTime", "ResurrectionExpirationLength",
            "WorldLocation", "bResurrectingNow", "RebootCounter", "bHoldsRebootVanLock", "BotUniqueId", "DeathLocation",
            "SimulatedAttributes", "KickedFromSessionReason", "NumRejoins", "bIsAnAthenaGameParticipant", "bHidden",
        ]),
        // shortcut: other groups do not track undeclared fields; only matters if an update holds nothing but those.
        _ => None,
    }
}

#[derive(Default)]
struct Fields {
    searching_pawn: Option<u32>,
    already_searched: Option<bool>,
    destroyed: Option<bool>,
    player_state: Option<u32>,
    ps: PlayerStateExport,
    recorder: Option<u32>,
    session_id: Option<String>,
    utc_started: Option<u64>,
}

enum FieldRead {
    NotDeclared,
    Read,
}

/// Decodes one property with the same layout type as the C# attribute. Unused properties are skipped.
fn read_field(kind: Kind, name: &str, r: &mut BitReader, f: &mut Fields) -> FieldRead {
    if let Some(list) = declared_fields(kind) {
        if !list.contains(&name) {
            return FieldRead::NotDeclared;
        }
    }
    // Each arm: read, then keep the value only if the read did not overflow (C#: data != null && !IsError).
    macro_rules! set {
        ($slot:expr, $val:expr) => {{
            let v = $val;
            if !r.error {
                $slot = Some(v);
            }
        }};
    }
    match (kind, name) {
        (Kind::Container, "SearchingPawn") => set!(f.searching_pawn, r.read_int_packed()),
        (Kind::Container, "bAlreadySearched") => set!(f.already_searched, r.read_bit()),
        (Kind::Container, "bDestroyed") => set!(f.destroyed, r.read_bit()),
        (Kind::PlayerPawn, "PlayerState") => set!(f.player_state, r.read_int_packed()),
        (Kind::PlayerState, "PlayerID") => set!(f.ps.player_id_int, r.read_i32()),
        (Kind::PlayerState, "PlayerId") => set!(f.ps.player_id_uint, r.read_u32()),
        (Kind::PlayerState, "bIsABot") => set!(f.ps.is_bot, r.read_bit()),
        (Kind::PlayerState, "bOnlySpectator") => set!(f.ps.only_spectator, r.read_bit()),
        (Kind::PlayerState, "UniqueId") => set!(f.ps.unique_id, r.read_net_id()),
        (Kind::PlayerState, "UniqueID") => set!(f.ps.unique_id_alt, r.read_net_id()),
        (Kind::PlayerState, "TeamIndex") => {
            let n = r.bits_left();
            set!(f.ps.team_index, r.read_bits_u64(n) as i32)
        }
        (Kind::PlayerState, "Place") => set!(f.ps.place, r.read_i32()),
        (Kind::GameState, "RecorderPlayerState") => set!(f.recorder, r.read_int_packed()),
        (Kind::GameState, "GameSessionId") => set!(f.session_id, r.read_fstring()),
        (Kind::GameState, "UtcTimeStartedMatch") => set!(f.utc_started, r.read_u64()),
        _ => {
            // Not needed by the counter: treat as fully read.
            r.pos = r.last_bit;
        }
    }
    FieldRead::Read
}

fn to_export(kind: Kind, f: Fields) -> Export {
    match kind {
        Kind::Container => Export::Container {
            searching_pawn: f.searching_pawn,
            already_searched: f.already_searched.unwrap_or(false),
            destroyed: f.destroyed.unwrap_or(false),
        },
        Kind::PlayerPawn => Export::PlayerPawn { player_state: f.player_state },
        Kind::PlayerState => Export::PlayerState(f.ps),
        Kind::GameState => Export::GameState { recorder: f.recorder, session_id: f.session_id, utc_started_ticks: f.utc_started },
        Kind::Other => Export::Other,
    }
}

// ---------------------------------------------------------------------------------------------------------------
// GUID cache
// ---------------------------------------------------------------------------------------------------------------

struct FieldExport {
    name: String,
    incompatible: bool,
}

struct Group {
    path_name: String,
    path_name_index: u32,
    exports: Vec<Option<FieldExport>>,
}

#[derive(Default)]
struct GuidCache {
    groups: Vec<Group>, // insertion order = C# Dictionary enumeration order
    by_path: HashMap<String, usize>,
    keys: Vec<String>,
    index_to_path: HashMap<u32, String>,
    from_index: HashMap<u32, usize>,
    guid_to_path: HashMap<u32, String>,
    path_fixed: HashMap<u32, usize>,
    arch_to_group: HashMap<u32, usize>,
    cleaned_paths: HashMap<u32, String>,
    failed_paths: HashSet<String>,
    external: HashMap<u32, Vec<u8>>,
}

/// UObjectBaseUtility::RemoveAllPathPrefixes as implemented by the C# reader.
pub fn remove_all_path_prefixes(path: &str) -> String {
    for (i, c) in path.char_indices().rev() {
        match c {
            '.' => return path[i + 1..].to_string(),
            '/' => return path.to_string(),
            _ => {}
        }
    }
    path.strip_prefix("Default__").unwrap_or(path).to_string()
}

fn clean_path_suffix(path: &str) -> &str {
    path.trim_end_matches(|c: char| c.is_ascii_digit() || c == '_')
}

impl GuidCache {
    fn add_group(&mut self, key: String, mut g: Group) -> usize {
        if key.ends_with("ClassNetCache") {
            g.path_name = remove_all_path_prefixes(&g.path_name);
        }
        let idx = g.path_name_index;
        let id = if let Some(&id) = self.by_path.get(&key) {
            self.groups[id] = g;
            id
        } else {
            self.groups.push(g);
            self.keys.push(key.clone());
            self.by_path.insert(key.clone(), self.groups.len() - 1);
            self.groups.len() - 1
        };
        self.index_to_path.insert(idx, key);
        self.from_index.insert(idx, id);
        id
    }

    fn group_from_index(&mut self, idx: u32) -> Option<usize> {
        if let Some(&id) = self.from_index.get(&idx) {
            return Some(id);
        }
        let id = *self.by_path.get(self.index_to_path.get(&idx)?)?;
        self.from_index.insert(idx, id);
        Some(id)
    }

    fn group_by_guid(&mut self, guid: Option<u32>) -> Option<usize> {
        let guid = guid?;
        if let Some(&id) = self.arch_to_group.get(&guid) {
            return Some(id);
        }
        let path = self.guid_to_path.get(&guid)?.clone();
        if self.failed_paths.contains(&path) {
            return None;
        }
        if let Some(&id) = self.path_fixed.get(&guid) {
            self.arch_to_group.insert(guid, id);
            return Some(id);
        }
        for id in 0..self.groups.len() {
            let idx = self.groups[id].path_name_index;
            let fixed = self.cleaned_paths.entry(idx).or_insert_with(|| remove_all_path_prefixes(&self.keys[id]));
            if path.contains(fixed.as_str()) {
                let found = self.by_path[&self.keys[id]];
                self.path_fixed.insert(guid, found);
                self.arch_to_group.insert(guid, found);
                return Some(found);
            }
        }
        let cleaned = clean_path_suffix(&path);
        for id in 0..self.groups.len() {
            if let Some(fixed) = self.cleaned_paths.get(&self.groups[id].path_name_index) {
                if fixed.contains(cleaned) {
                    let found = self.by_path[&self.keys[id]];
                    self.path_fixed.insert(guid, found);
                    self.arch_to_group.insert(guid, found);
                    return Some(found);
                }
            }
        }
        self.failed_paths.insert(path);
        None
    }

    fn class_net_cache(&self, group_path: &str, full_name: bool) -> Option<usize> {
        let key = if full_name { format!("{group_path}_ClassNetCache") } else { format!("{}_ClassNetCache", remove_all_path_prefixes(group_path)) };
        self.by_path.get(&key).copied()
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------------------------------------------

struct Actor {
    guid: u32,
    archetype: Option<u32>,
}

#[derive(Default)]
struct Channel {
    actor: Option<Actor>,
    ignore: HashSet<String>,
}

#[derive(Clone)]
struct Bunch {
    ar: BitReader,
    ch: u32,
    ch_seq: i32,
    open: bool,
    close: bool,
    reliable: bool,
    partial: bool,
    partial_initial: bool,
    partial_final: bool,
    has_package_map_exports: bool,
    has_must_be_mapped: bool,
}

pub struct Parser<H: Handler> {
    pub handler: H,
    meta: ReplayMeta,
    cache: GuidCache,
    channels: Vec<Option<Channel>>,
    kinds: HashMap<String, Option<Kind>>,
    time: f32,
    in_packet_id: i32,
    in_reliable: i32,
    partial_bunch: Option<Bunch>,
}

enum Flow {
    Continue,
    Break,
}

impl<H: Handler> Parser<H> {
    pub fn new(handler: H) -> Self {
        let mut channels = Vec::with_capacity(MAX_CHANNELS);
        channels.resize_with(MAX_CHANNELS, || None);
        Parser {
            handler,
            meta: ReplayMeta::default(),
            cache: GuidCache::default(),
            channels,
            kinds: HashMap::new(),
            time: 0.0,
            in_packet_id: 0,
            in_reliable: 0,
            partial_bunch: None,
        }
    }

    fn engine(&self) -> u32 {
        self.meta.header.engine_version
    }

    fn will_read(&mut self, path: &str) -> Option<Kind> {
        if let Some(k) = self.kinds.get(path) {
            return *k;
        }
        let k = registered_kind(path).or_else(|| alias_kind(path));
        self.kinds.insert(path.to_string(), k);
        k
    }

    /// Parses a whole replay file.
    pub fn parse(mut self, data: Vec<u8>) -> R<(ReplayMeta, H)> {
        let mut ar = Bytes::new(data);
        self.read_info(&mut ar)?;
        self.read_chunks(&mut ar)?;
        Ok((self.meta, self.handler))
    }

    /// Reads only the file header (first few KB): enough to know whether the match is still recording.
    pub fn peek_info(path: &std::path::Path) -> R<Info> {
        use std::io::Read;
        let mut head = Vec::new();
        std::fs::File::open(path).and_then(|f| f.take(64 * 1024).read_to_end(&mut head)).map_err(|e| e.to_string())?;
        // No channel table: only the header is read (this runs every few seconds while a match records).
        let mut p = Parser {
            handler: NoHandler,
            meta: ReplayMeta::default(),
            cache: GuidCache::default(),
            channels: Vec::new(),
            kinds: HashMap::new(),
            time: 0.0,
            in_packet_id: 0,
            in_reliable: 0,
            partial_bunch: None,
        };
        match p.read_info(&mut Bytes::new(head)) {
            Ok(()) => Ok(p.meta.info),
            // A live replay may have no key yet: still a valid header.
            Err(_) if p.meta.info.is_live => Ok(p.meta.info),
            Err(e) => Err(e),
        }
    }

    fn read_info(&mut self, ar: &mut Bytes) -> R<()> {
        if ar.u32()? != FILE_MAGIC {
            return Err("Invalid replay file".into());
        }
        let v = ar.u32()?;
        let info = &mut self.meta.info;
        info.file_version = v;
        if v >= REPLAY_CUSTOM_VERSIONS {
            let n = ar.i32()?;
            ar.skip(n.max(0) as usize * 20)?;
        }
        info.length_ms = ar.u32()?;
        let _network_version = ar.u32()?;
        let _changelist = ar.u32()?;
        let _friendly_name = ar.fstring()?;
        info.is_live = ar.u32_bool()?;
        if v >= REPLAY_RECORDED_TIMESTAMP {
            info.timestamp_ticks = ar.i64()?;
        }
        if v >= REPLAY_COMPRESSION {
            info.is_compressed = ar.u32_bool()?;
        }
        if v >= REPLAY_ENCRYPTION {
            info.is_encrypted = ar.u32_bool()?;
            let size = ar.u32()? as usize;
            info.key = ar.take_vec(size)?;
        }
        if info.is_encrypted && info.key.is_empty() {
            return Err(if info.is_live { "Replay is marked encrypted but not yet marked as completed!" } else { "Completed replay is marked encrypted but has no key!" }.into());
        }
        Ok(())
    }

    fn read_chunks(&mut self, ar: &mut Bytes) -> R<()> {
        while !ar.at_end() {
            if ar.len() - ar.pos < 8 {
                self.meta.is_partial = true;
                return Ok(());
            }
            let chunk_type = ar.u32()?;
            let size = ar.i32()?;
            let offset = ar.pos;
            if size > 0 && offset + size as usize > ar.len() {
                self.meta.is_partial = true;
                return Ok(());
            }
            if size <= 0 || size as i64 + offset as i64 > i32::MAX as i64 {
                return Ok(()); // C# sets the archive error and stops
            }
            match chunk_type {
                0 => self.read_header(ar)?,
                1 => match self.read_replay_data(ar, size as usize) {
                    Err(e) if e.starts_with("decrypt:") && self.meta.info.is_live => {
                        self.meta.is_partial = true;
                        return Ok(());
                    }
                    r => r?,
                },
                3 => match self.read_event(ar) {
                    Err(e) if e.starts_with("decrypt:") && self.meta.info.is_live => {}
                    r => r?,
                },
                _ => {} // checkpoints are not needed
            }
            if ar.pos != offset + size as usize {
                ar.seek(offset + size as usize);
            }
        }
        Ok(())
    }

    fn read_header(&mut self, ar: &mut Bytes) -> R<()> {
        if ar.u32()? != NETWORK_MAGIC {
            return Err("Header.Magic != NETWORK_DEMO_MAGIC".into());
        }
        let h = &mut self.meta.header;
        h.network_version = ar.u32()?;
        if h.network_version <= NET_EXTRA_VERSION {
            return Err("Header.Version < MIN_NETWORK_DEMO_VERSION".into());
        }
        if h.network_version >= NET_USE_CUSTOM_VERSION {
            let n = ar.i32()?;
            ar.skip(n.max(0) as usize * 20)?;
        }
        let _checksum = ar.u32()?;
        h.engine_version = ar.u32()?;
        let _game_protocol = ar.u32()?;
        if h.network_version >= NET_HEADER_GUID {
            ar.skip(16)?;
        }
        if h.network_version >= NET_SAVE_FULL_ENGINE_VERSION {
            ar.skip(6)?; // major, minor, patch
            h.changelist = ar.u32()?;
            h.branch = ar.fstring()?;
        } else {
            h.changelist = ar.u32()?;
        }
        if h.network_version >= NET_RECORDING_METADATA {
            ar.skip(12)?;
        }
        if h.network_version <= NET_MULTIPLE_LEVELS {
            return Err("HISTORY_MULTIPLE_LEVELS not supported yet.".into());
        }
        for _ in 0..ar.u32()? {
            ar.fstring()?;
            ar.u32()?;
        }
        if h.network_version >= 9 {
            h.flags = ar.u32()?;
        }
        for _ in 0..ar.u32()? {
            ar.fstring()?;
        }
        if h.network_version >= NET_SAVE_PACKAGE_VERSION_UE {
            ar.skip(16)?;
            ar.fstring()?;
            ar.skip(2)?;
        }
        Ok(())
    }

    fn has_streaming_fixes(&self) -> bool {
        has_flag(self.meta.header.flags, FLAG_HAS_STREAMING_FIXES)
    }

    /// AES-256-ECB with PKCS7 padding. Errors start with "decrypt:" (C# CryptographicException).
    fn decrypt(&self, ar: &mut Bytes, size: usize) -> R<Bytes> {
        let mut data = ar.take_vec(size)?;
        if !self.meta.info.is_encrypted {
            return Ok(Bytes::new(data));
        }
        let cipher = aes::Aes256::new_from_slice(&self.meta.info.key).map_err(|_| "decrypt: bad key length".to_string())?;
        if data.is_empty() || data.len() % 16 != 0 {
            return Err("decrypt: data is not a whole number of blocks".into());
        }
        for block in data.chunks_exact_mut(16) {
            cipher.decrypt_block(block.into());
        }
        let pad = *data.last().unwrap() as usize;
        if pad == 0 || pad > 16 || data[data.len() - pad..].iter().any(|&b| b as usize != pad) {
            return Err("decrypt: padding is invalid".into());
        }
        data.truncate(data.len() - pad);
        Ok(Bytes::new(data))
    }

    fn decompress(&self, mut ar: Bytes) -> R<Bytes> {
        if !self.meta.info.is_compressed {
            return Ok(ar);
        }
        let decompressed = ar.i32()?;
        let compressed = ar.i32()?;
        let buf = ar.take(compressed.max(0) as usize)?;
        Ok(Bytes::new(oodle::decompress(buf, decompressed.max(0) as usize)?))
    }

    fn read_replay_data(&mut self, ar: &mut Bytes, chunk_size: usize) -> R<()> {
        let v = self.meta.info.file_version;
        let length = if has_flag(v, REPLAY_STREAM_CHUNK_TIMES) {
            ar.u32()?;
            ar.u32()?;
            ar.u32()? as usize
        } else {
            chunk_size
        };
        if has_flag(v, REPLAY_ENCRYPTION) {
            ar.i32()?;
        }
        let decrypted = self.decrypt(ar, length)?;
        let mut data = self.decompress(decrypted)?;
        while !data.at_end() {
            self.read_demo_frame(&mut data)?;
        }
        Ok(())
    }

    fn read_event(&mut self, ar: &mut Bytes) -> R<()> {
        let _id = ar.fstring()?;
        let group = ar.fstring()?;
        let metadata = ar.fstring()?;
        ar.u32()?;
        ar.u32()?;
        let size = ar.i32()?.max(0) as usize;
        let mut ev = self.decrypt(ar, size)?;
        if group == "playerElim" {
            // shortcut: eliminations are not parsed yet (the C# fails the replay if one is malformed).
            return Ok(());
        }
        match metadata.as_str() {
            "AthenaMatchStats" => {
                ev.u32()?; // unknown
                ev.f32()?; // accuracy
                ev.u32()?; // assists
                self.meta.eliminations = Some(ev.u32()?);
            }
            "AthenaMatchTeamStats" => {
                ev.u32()?;
                let position = ev.u32()?;
                let total = ev.u32()?;
                self.meta.team_stats = Some((position, total));
            }
            _ => {}
        }
        Ok(())
    }

    fn read_demo_frame(&mut self, ar: &mut Bytes) -> R<()> {
        let nv = self.meta.header.network_version;
        if nv >= NET_MULTIPLE_LEVELS {
            ar.i32()?;
        }
        self.time = ar.f32()?;
        if nv >= NET_LEVEL_STREAMING_FIXES {
            self.read_net_field_exports(ar)?;
            self.read_net_export_guids(ar)?;
        }
        if self.has_streaming_fixes() {
            for _ in 0..ar.int_packed()? {
                ar.fstring()?;
            }
        } else {
            for _ in 0..ar.int_packed()? {
                ar.fstring()?;
                ar.fstring()?;
                ar.skip(40)?; // FTransform: quat + 2 vectors (floats)
            }
        }
        if self.has_streaming_fixes() {
            ar.u64()?;
        }
        loop {
            let bits = ar.int_packed()?;
            if bits == 0 {
                break;
            }
            let guid = ar.int_packed()?;
            let data = ar.take_vec(((bits as u64 + 7) >> 3) as usize)?;
            self.cache.external.insert(guid, data);
        }
        if has_flag(self.meta.header.flags, FLAG_GAME_SPECIFIC_FRAME_DATA) {
            let skip = ar.u64()?;
            if skip > 0 {
                ar.skip(skip as usize)?;
            }
        }
        loop {
            if self.has_streaming_fixes() {
                ar.int_packed()?;
            }
            let size = ar.i32()?;
            if size == 0 || !(0..=2048).contains(&size) {
                break;
            }
            let packet = ar.take_vec(size as usize)?;
            self.received_raw_packet(packet)?;
        }
        Ok(())
    }

    fn read_net_field_export_bytes(&self, ar: &mut Bytes) -> R<Option<(u32, String)>> {
        if !ar.bool()? {
            return Ok(None);
        }
        let handle = ar.int_packed()?;
        ar.u32()?; // compatible checksum
        let engine = self.engine();
        let name = if engine < ENGINE_NETEXPORT_SERIALIZATION {
            let n = ar.fstring()?;
            ar.fstring()?;
            n
        } else if engine < ENGINE_NETEXPORT_SERIALIZE_FIX {
            ar.fstring()?
        } else {
            ar.fname(engine)?
        };
        Ok(Some((handle, name)))
    }

    fn read_net_field_exports(&mut self, ar: &mut Bytes) -> R<()> {
        for _ in 0..ar.int_packed()? {
            let index = ar.int_packed()?;
            let exported = ar.int_packed()? == 1;
            let group = if exported {
                let path = ar.fstring()?;
                let num = ar.int_packed()? as usize;
                match self.cache.by_path.get(&path).copied() {
                    None => Some(self.cache.add_group(
                        path.clone(),
                        Group { path_name: path, path_name_index: index, exports: (0..num).map(|_| None).collect() },
                    )),
                    Some(id) => {
                        let g = &mut self.cache.groups[id];
                        if num > g.exports.len() {
                            g.exports.resize_with(num, || None);
                        }
                        Some(id)
                    }
                }
            } else {
                self.cache.group_from_index(index)
            };
            let field = self.read_net_field_export_bytes(ar)?;
            if let (Some(id), Some((handle, name))) = (group, field) {
                if let Some(slot) = self.cache.groups[id].exports.get_mut(handle as usize) {
                    *slot = Some(FieldExport { name, incompatible: false });
                }
            }
        }
        Ok(())
    }

    fn read_net_export_guids(&mut self, ar: &mut Bytes) -> R<()> {
        for _ in 0..ar.int_packed()? {
            let size = ar.i32()?.max(0) as usize;
            let data = ar.take_vec(size)?;
            let mut r = BitReader::new(data, size * 8, self.engine());
            self.internal_load_object(&mut r, true, 0);
        }
        Ok(())
    }

    fn internal_load_object(&mut self, ar: &mut BitReader, exporting: bool, depth: u32) -> u32 {
        if depth > 16 {
            return 0;
        }
        let guid = ar.read_int_packed();
        if guid == 0 {
            return guid;
        }
        if guid == 1 || exporting {
            let flags = ar.read_byte();
            if flags & 1 != 0 {
                self.internal_load_object(ar, true, depth + 1);
                let path = ar.read_fstring();
                if flags & 4 != 0 {
                    ar.read_u32();
                }
                if exporting {
                    self.cache.guid_to_path.insert(guid, remove_all_path_prefixes(&path));
                    self.handler.on_netguid_exported(self.time, guid, &path);
                }
            }
        }
        guid
    }

    fn received_raw_packet(&mut self, packet: Vec<u8>) -> R<()> {
        let last = *packet.last().unwrap_or(&0);
        if last == 0 {
            return Err("Malformed packet: Received packet with 0's in last byte of packet".into());
        }
        let bits = packet.len() * 8 - 1 - last.leading_zeros() as usize;
        let mut reader = BitReader::new(packet, bits, self.engine());
        // Errors abort this packet only (C# catch-all around ReceivedPacket).
        let _ = self.received_packet(&mut reader);
        Ok(())
    }

    fn received_packet(&mut self, r: &mut BitReader) -> Result<(), Abort> {
        self.in_packet_id += 1;
        let engine = self.engine();
        let custom_exports_bit = engine >= ENGINE_CUSTOM_EXPORTS;
        while !r.at_end() {
            if engine < ENGINE_ACKS_INCLUDED_IN_HEADER {
                r.read_bit();
            }
            let control = r.read_bit();
            let open = control && r.read_bit();
            let close = control && r.read_bit();
            if engine < ENGINE_CHANNEL_CLOSE_REASON {
                let _dormant = close && r.read_bit();
            } else if close {
                r.read_serialized_int(15);
            }
            let _paused = r.read_bit();
            let reliable = r.read_bit();
            let ch = if engine < ENGINE_MAX_ACTOR_CHANNELS_CUSTOMIZATION { r.read_serialized_int(10240) } else { r.read_int_packed() };
            let has_package_map_exports = r.read_bit();
            let has_must_be_mapped = r.read_bit();
            let partial = r.read_bit();
            let ch_seq = if reliable {
                self.in_reliable + 1
            } else if partial {
                self.in_packet_id
            } else {
                0
            };
            let partial_initial = partial && r.read_bit();
            if partial && custom_exports_bit {
                r.read_bit();
            }
            let partial_final = partial && r.read_bit();
            if engine < ENGINE_CHANNEL_NAMES {
                r.read_serialized_int(8);
            } else if reliable || open {
                r.read_fname();
            }
            let bunch_bits = r.read_serialized_int(MAX_PACKET_BITS) as usize;
            if r.error || !r.can_read(bunch_bits) {
                break; // bunch overflows the packet
            }
            let ar = r.sub_reader(bunch_bits);
            let mut bunch = Bunch {
                ar,
                ch,
                ch_seq,
                open,
                close,
                reliable,
                partial,
                partial_initial,
                partial_final,
                has_package_map_exports,
                has_must_be_mapped,
            };
            if bunch.has_package_map_exports {
                self.receive_net_guid_bunch(&mut bunch.ar);
            }
            let slot = self.channels.get_mut(ch as usize).ok_or_else(|| Abort("channel index out of range".into()))?;
            if slot.is_none() {
                *slot = Some(Channel::default());
            }
            // Errors abort this bunch only (C# catch around ReceivedRawBunch).
            let _ = self.received_next_bunch(bunch);
        }
        Ok(())
    }

    fn receive_net_guid_bunch(&mut self, ar: &mut BitReader) {
        if ar.read_bit() {
            // ReceiveNetFieldExportsCompat
            for _ in 0..ar.read_u32() {
                let index = ar.read_int_packed();
                let group = if ar.read_bit() {
                    let path = ar.read_fstring();
                    let num = ar.read_u32() as usize;
                    match self.cache.by_path.get(&path).copied() {
                        None => Some(self.cache.add_group(
                            path.clone(),
                            Group { path_name: path, path_name_index: index, exports: (0..num.min(1 << 16)).map(|_| None).collect() },
                        )),
                        Some(id) => Some(id),
                    }
                } else {
                    self.cache.group_from_index(index)
                };
                let field = if ar.read_bit() {
                    let handle = ar.read_int_packed();
                    ar.read_u32();
                    let engine = self.engine();
                    let name = if engine < ENGINE_NETEXPORT_SERIALIZATION {
                        let n = ar.read_fstring();
                        ar.read_fstring();
                        n
                    } else if engine < ENGINE_NETEXPORT_SERIALIZE_FIX {
                        ar.read_fstring()
                    } else {
                        ar.read_fname()
                    };
                    Some((handle, name))
                } else {
                    None
                };
                if let (Some(id), Some((handle, name))) = (group, field) {
                    if let Some(slot) = self.cache.groups[id].exports.get_mut(handle as usize) {
                        *slot = Some(FieldExport { name, incompatible: false });
                    }
                }
                if ar.error {
                    return;
                }
            }
            return;
        }
        let n = ar.read_i32();
        if n > MAX_GUID_COUNT {
            return;
        }
        for _ in 0..n.max(0) {
            self.internal_load_object(ar, true, 0);
            if ar.error {
                return;
            }
        }
    }

    fn received_next_bunch(&mut self, mut bunch: Bunch) -> Result<(), Abort> {
        if bunch.reliable {
            self.in_reliable = bunch.ch_seq;
        }
        if !bunch.partial {
            return self.received_sequenced_bunch(&mut bunch);
        }
        if bunch.partial_initial {
            if let Some(pb) = &self.partial_bunch {
                if !pb.partial_final && pb.reliable {
                    return Ok(()); // a reliable partial cannot be replaced
                }
                self.partial_bunch = None;
            }
            self.partial_bunch = Some(bunch);
            return Ok(());
        }
        let Some(mut pb) = self.partial_bunch.take() else {
            return Ok(()); // merge problem
        };
        let reliable_match = bunch.ch_seq == pb.ch_seq + 1;
        let seq_matches = if pb.reliable { reliable_match } else { reliable_match || bunch.ch_seq == pb.ch_seq };
        if pb.partial_final || !seq_matches || pb.reliable != bunch.reliable {
            self.partial_bunch = Some(pb);
            return Ok(()); // merge problem: keep the old partial bunch, like the C#
        }
        let bits_left = bunch.ar.bits_left();
        if !bunch.has_package_map_exports && bits_left > 0 {
            let data = bunch.ar.read_bits(bits_left);
            pb.ar.append(&data, bits_left);
        }
        if !bunch.has_package_map_exports && !bunch.partial_final && bits_left % 8 != 0 {
            self.partial_bunch = Some(pb);
            return Ok(()); // corrupt partial bunch
        }
        pb.ch_seq = bunch.ch_seq;
        if bunch.partial_final {
            if bunch.has_package_map_exports {
                self.partial_bunch = Some(pb);
                return Ok(());
            }
            pb.partial_final = true;
            pb.close = bunch.close;
            pb.has_must_be_mapped = bunch.has_must_be_mapped;
            let res = self.received_sequenced_bunch(&mut pb);
            self.partial_bunch = Some(pb);
            return res;
        }
        self.partial_bunch = Some(pb);
        Ok(())
    }

    fn received_sequenced_bunch(&mut self, bunch: &mut Bunch) -> Result<(), Abort> {
        if bunch.has_must_be_mapped {
            let n = bunch.ar.read_u16();
            for _ in 0..n {
                bunch.ar.read_int_packed();
            }
        }
        self.process_bunch(bunch)?;
        if bunch.close {
            self.channels[bunch.ch as usize] = None;
        }
        Ok(())
    }

    fn read_packed_vector(ar: &mut BitReader, scale: i32, max_bits: u32) {
        let engine = ar.engine_version;
        if engine >= ENGINE_PACKED_VECTOR_LWC_SUPPORT && engine != ENGINE_21_AND_VIEWPITCH_ONLY_DO_NOT_USE {
            let info = ar.read_serialized_int(1 << 7);
            let bits = (info & 63) as usize;
            let extra = info >> 6;
            if bits > 0 {
                ar.read_bits_u64(bits);
                ar.read_bits_u64(bits);
                ar.read_bits_u64(bits);
            } else if extra == 0 {
                ar.skip_bits(96);
            } else {
                ar.skip_bits(192);
            }
            return;
        }
        let _ = scale;
        let bits = ar.read_serialized_int(max_bits);
        if ar.error {
            return;
        }
        let max = 1i64.wrapping_shl((bits + 2) & 31);
        for _ in 0..3 {
            if max > 0 {
                ar.read_serialized_int(max.min(u32::MAX as i64) as u32);
            }
        }
    }

    fn conditionally_serialize_quantized_vector(ar: &mut BitReader) {
        if ar.read_bit() {
            let quantize = ar.engine_version < ENGINE_OPTIONALLY_QUANTIZE_SPAWN_INFO || ar.read_bit();
            if quantize {
                Self::read_packed_vector(ar, 10, 24);
            } else if ar.engine_version >= ENGINE_PACKED_VECTOR_LWC_SUPPORT {
                ar.skip_bits(192);
            } else {
                ar.skip_bits(96);
            }
        }
    }

    fn process_bunch(&mut self, bunch: &mut Bunch) -> Result<(), Abort> {
        let ch = bunch.ch;
        let opens_actor = matches!(self.channels.get(ch as usize), Some(Some(c)) if c.actor.is_none());
        if opens_actor {
            if !bunch.open {
                return Ok(());
            }
            let ar = &mut bunch.ar;
            let guid = self.internal_load_object(ar, false, 0);
            let dynamic = guid > 0 && guid & 1 != 1;
            if ar.at_end() && dynamic {
                return Ok(());
            }
            let mut archetype = None;
            if dynamic {
                archetype = Some(self.internal_load_object(ar, false, 0));
                if ar.engine_version >= ENGINE_NEW_ACTOR_OVERRIDE_LEVEL {
                    self.internal_load_object(ar, false, 0);
                }
                Self::conditionally_serialize_quantized_vector(ar); // location
                if ar.read_bit() {
                    for _ in 0..3 {
                        if ar.read_bit() {
                            ar.read_u16();
                        }
                    }
                }
                Self::conditionally_serialize_quantized_vector(ar); // scale
                Self::conditionally_serialize_quantized_vector(ar); // velocity
            }
            if let Some(Some(c)) = self.channels.get_mut(ch as usize) {
                c.actor = Some(Actor { guid, archetype });
            }
            let ctx = Ctx { time: self.time, channels: &self.channels, guid_to_path: &self.cache.guid_to_path };
            self.handler.on_channel_opened(&ctx, ch);
            // Replay player controllers carry an extra net player index byte.
            if self.cache.guid_to_path.get(&archetype.unwrap_or(0)).is_some_and(|p| p == "BP_ReplayPC_Athena_C") {
                bunch.ar.read_byte();
            }
        }

        while !bunch.ar.at_end() {
            let (rep_object, has_rep_layout, payload) = self.read_content_block_payload(bunch);
            if bunch.ar.error {
                break;
            }
            let Some(payload) = payload else { continue };
            bunch.ar.set_temp_end(payload as usize, END_CONTENT_BLOCK);
            let flow = if bunch.ar.error {
                Ok(Flow::Break)
            } else if rep_object.is_none() || bunch.ar.at_end() {
                Ok(Flow::Continue)
            } else {
                self.received_replicator_bunch(ch, &mut bunch.ar, rep_object, has_rep_layout).map(|_| Flow::Continue)
            };
            bunch.ar.restore_temp_end(END_CONTENT_BLOCK);
            if let Flow::Break = flow? {
                break;
            }
        }
        Ok(())
    }

    /// Returns (rep object, has rep layout, payload bits). Payload is None for a deleted sub-object.
    fn read_content_block_payload(&mut self, bunch: &mut Bunch) -> (Option<u32>, bool, Option<u32>) {
        let ar = &mut bunch.ar;
        let has_rep_layout = ar.read_bit();
        let is_actor = ar.read_bit();
        let rep_object = if is_actor {
            let channel = self.channels.get(bunch.ch as usize).and_then(|c| c.as_ref()).and_then(|c| c.actor.as_ref());
            channel.and_then(|a| a.archetype.or(Some(a.guid)))
        } else {
            let net_guid = self.internal_load_object(ar, false, 0);
            if ar.read_bit() {
                Some(net_guid) // stably named
            } else {
                let mut delete = false;
                let mut serialize_class = true;
                if ar.engine_version >= ENGINE_SUBOBJECT_DESTROY_FLAG && ar.read_bit() {
                    delete = true;
                    serialize_class = false;
                    ar.read_byte();
                }
                let mut class = 0;
                if serialize_class {
                    class = self.internal_load_object(ar, false, 0);
                    delete = class == 0;
                }
                if delete {
                    return (None, has_rep_layout, None);
                }
                if ar.engine_version >= ENGINE_SUBOBJECT_OUTER_CHAIN {
                    let actor_is_outer = ar.at_end() || ar.read_bit();
                    if !actor_is_outer {
                        self.internal_load_object(ar, false, 0);
                    }
                }
                Some(class)
            }
        };
        let payload = bunch.ar.read_int_packed();
        (rep_object, has_rep_layout, Some(payload))
    }

    fn received_replicator_bunch(&mut self, ch: u32, ar: &mut BitReader, rep_object: Option<u32>, has_rep_layout: bool) -> Result<bool, Abort> {
        let Some(gid) = self.cache.group_by_guid(rep_object) else {
            return Ok(true);
        };
        let path = self.cache.groups[gid].path_name.clone();
        if has_rep_layout {
            if !self.receive_properties(ar, gid, ch)? {
                return Ok(false);
            }
            self.receive_external_data(&path, ch)?;
        }
        if ar.at_end() {
            return Ok(true);
        }
        let full_name = self.engine() >= ENGINE_CLASSNETCACHE_FULLNAME;
        let Some(cache_id) = self.cache.class_net_cache(&path, full_name) else {
            return Ok(false);
        };
        let cache_path = self.cache.groups[cache_id].path_name.clone();
        while let Some((field, payload)) = self.read_field_header_and_payload(ar, cache_id)? {
            ar.set_temp_end(payload as usize, END_FIELD_HEADER);
            let res = self.read_class_net_cache_field(ch, ar, &cache_path, field.as_deref());
            ar.restore_temp_end(END_FIELD_HEADER);
            res?;
        }
        Ok(true)
    }

    /// Only GameState's CurrentPlaylistInfo is needed from the ClassNetCache fields (custom struct PlaylistInfo).
    fn read_class_net_cache_field(&mut self, ch: u32, ar: &mut BitReader, cache_path: &str, field: Option<&str>) -> Result<(), Abort> {
        if field != Some("CurrentPlaylistInfo") || cache_path != "Athena_GameState_C_ClassNetCache" || ar.error || ar.at_end() {
            return Ok(());
        }
        let bits = ar.bits_left();
        let mut r = ar.sub_reader(bits);
        if r.engine_version >= ENGINE_FAST_ARRAY_DELTA_STRUCT {
            r.read_bit();
        }
        r.read_bit();
        let id = r.read_int_packed();
        let name = self.cache.guid_to_path.get(&id).cloned();
        let ctx = Ctx { time: self.time, channels: &self.channels, guid_to_path: &self.cache.guid_to_path };
        self.handler.on_export(&ctx, ch, &Export::Playlist { name })
    }

    /// Returns the field name (None if the handle has no export) and payload size; None ends the field loop.
    #[allow(clippy::type_complexity)]
    fn read_field_header_and_payload(&self, ar: &mut BitReader, cache_id: usize) -> Result<Option<(Option<String>, u32)>, Abort> {
        if ar.at_end() {
            return Ok(None);
        }
        let group = &self.cache.groups[cache_id];
        let handle = ar.read_serialized_int(group.exports.len().max(2) as u32);
        if ar.error {
            return Ok(None);
        }
        // C# indexes the array directly: an out-of-range handle throws and aborts the bunch.
        let slot = group.exports.get(handle as usize).ok_or_else(|| Abort("field handle out of range".into()))?;
        let field = slot.as_ref().filter(|f| !f.incompatible).map(|f| f.name.clone());
        let payload = ar.read_int_packed();
        if ar.error || !ar.can_read(payload as usize) {
            return Ok(None);
        }
        Ok(Some((field, payload)))
    }

    fn receive_properties(&mut self, ar: &mut BitReader, gid: usize, ch: u32) -> Result<bool, Abort> {
        let path = self.cache.groups[gid].path_name.clone();
        match self.channels.get(ch as usize) {
            Some(Some(c)) if !c.ignore.contains(&path) => {}
            _ => return Ok(false),
        }
        let Some(kind) = self.will_read(&path) else {
            if let Some(Some(c)) = self.channels.get_mut(ch as usize) {
                c.ignore.insert(path);
            }
            return Ok(false);
        };
        ar.read_bit(); // property checksum flag
        let mut fields = Fields::default();
        let mut has_data = false;
        loop {
            let handle = ar.read_int_packed();
            if handle == 0 {
                break;
            }
            let handle = (handle - 1) as usize;
            if handle >= self.cache.groups[gid].exports.len() {
                return Ok(false);
            }
            let num_bits = ar.read_int_packed() as usize;
            if num_bits == 0 {
                continue;
            }
            let Some(export) = self.cache.groups[gid].exports[handle].as_mut() else {
                ar.skip_bits(num_bits);
                continue;
            };
            if export.incompatible {
                ar.skip_bits(num_bits);
                continue;
            }
            has_data = true;
            let mut cmd = ar.sub_reader(num_bits);
            match read_field(kind, &export.name, &mut cmd, &mut fields) {
                FieldRead::NotDeclared => export.incompatible = true,
                FieldRead::Read => {
                    if cmd.error || !cmd.at_end() {
                        export.incompatible = true;
                    }
                }
            }
        }
        if has_data {
            let export = to_export(kind, fields);
            let ctx = Ctx { time: self.time, channels: &self.channels, guid_to_path: &self.cache.guid_to_path };
            self.handler.on_export(&ctx, ch, &export)?;
        }
        Ok(true)
    }

    fn receive_external_data(&mut self, path: &str, ch: u32) -> Result<(), Abort> {
        let actor = match self.channels.get(ch as usize) {
            Some(Some(c)) if !c.ignore.contains(path) => c.actor.as_ref().map(|a| a.guid),
            _ => return Ok(()),
        };
        if let Some(data) = actor.and_then(|g| self.cache.external.remove(&g)) {
            let ctx = Ctx { time: self.time, channels: &self.channels, guid_to_path: &self.cache.guid_to_path };
            self.handler.on_external_data(&ctx, ch, &data)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_helpers() {
        assert_eq!(remove_all_path_prefixes("/Game/A/Tiered_Chest.Tiered_Chest_C"), "Tiered_Chest_C");
        assert_eq!(remove_all_path_prefixes("/Game/A/B"), "/Game/A/B");
        assert_eq!(remove_all_path_prefixes("Default__Foo"), "Foo");
        assert_eq!(clean_path_suffix("Tiered_Chest_6_"), "Tiered_Chest");
    }

    #[test]
    fn aliases() {
        assert_eq!(alias_kind("/Game/Building/ActorBlueprints/Containers/Tiered_Ammo_Athena.Tiered_Ammo_Athena_C"), Some(Kind::Container));
        assert_eq!(alias_kind("/Foo/B_RelicFinder_Athena.B_RelicFinder_Athena_C"), Some(Kind::Other));
        assert_eq!(alias_kind("/Foo/Bar.Bar_C:SomeRpc"), None);
        assert_eq!(alias_kind("/Game/Athena/PlayerPawn_Athena.PlayerPawn_Athena_C"), None);
    }
}
