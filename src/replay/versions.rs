//! Unreal replay version numbers used by the parser (subset of the C# enums).

// EngineNetworkVersionHistory
pub const ENGINE_MAX_ACTOR_CHANNELS_CUSTOMIZATION: u32 = 3;
pub const ENGINE_NEW_ACTOR_OVERRIDE_LEVEL: u32 = 5;
pub const ENGINE_CHANNEL_NAMES: u32 = 6;
pub const ENGINE_CHANNEL_CLOSE_REASON: u32 = 7;
pub const ENGINE_ACKS_INCLUDED_IN_HEADER: u32 = 8;
pub const ENGINE_NETEXPORT_SERIALIZATION: u32 = 9;
pub const ENGINE_NETEXPORT_SERIALIZE_FIX: u32 = 10;
pub const ENGINE_FAST_ARRAY_DELTA_STRUCT: u32 = 11;
pub const ENGINE_OPTIONALLY_QUANTIZE_SPAWN_INFO: u32 = 13;
pub const ENGINE_CLASSNETCACHE_FULLNAME: u32 = 15;
pub const ENGINE_SUBOBJECT_OUTER_CHAIN: u32 = 18;
pub const ENGINE_PACKED_VECTOR_LWC_SUPPORT: u32 = 23;
pub const ENGINE_21_AND_VIEWPITCH_ONLY_DO_NOT_USE: u32 = 26;
pub const ENGINE_SUBOBJECT_DESTROY_FLAG: u32 = 30;
pub const ENGINE_CUSTOM_EXPORTS: u32 = 36;

// NetworkVersionHistory
pub const NET_EXTRA_VERSION: u32 = 5;
pub const NET_MULTIPLE_LEVELS: u32 = 6;
pub const NET_LEVEL_STREAMING_FIXES: u32 = 10;
pub const NET_SAVE_FULL_ENGINE_VERSION: u32 = 11;
pub const NET_HEADER_GUID: u32 = 12;
pub const NET_SAVE_PACKAGE_VERSION_UE: u32 = 17;
pub const NET_RECORDING_METADATA: u32 = 18;
pub const NET_USE_CUSTOM_VERSION: u32 = 19;

// ReplayVersionHistory
pub const REPLAY_COMPRESSION: u32 = 2;
pub const REPLAY_RECORDED_TIMESTAMP: u32 = 3;
pub const REPLAY_STREAM_CHUNK_TIMES: u32 = 4;
pub const REPLAY_ENCRYPTION: u32 = 6;
pub const REPLAY_CUSTOM_VERSIONS: u32 = 7;

// ReplayHeaderFlags
pub const FLAG_HAS_STREAMING_FIXES: u32 = 1 << 1;
pub const FLAG_GAME_SPECIFIC_FRAME_DATA: u32 = 1 << 3;

/// C# `Enum.HasFlag` on a plain version number is a bit test; the original relies on it.
pub fn has_flag(value: u32, flag: u32) -> bool {
    value & flag == flag
}
