//! Client to server messages (PRO-5, PRO-6, PRO-16).
//!
//! Every type here is untrusted input, so each one rejects unknown fields
//! (PRO-16). That is why the settings update is a protocol-owned struct rather
//! than `core::config::SettingsUpdate`: a message carrying a `seed` field must
//! fail as `MalformedMessage`, and a nested type without `deny_unknown_fields`
//! would silently accept one (PRO-16, SRV-10, T-PRO-7).

use serde::{Deserialize, Serialize};
use superminigames_core::config::{GameFilterMode, MatchConfig};
use superminigames_core::game::GameAction;
use superminigames_core::ids::LobbyId;

/// The host's match settings on the wire: `MatchConfig` without `seed` (PRO-16).
///
/// The seed is drawn by the server at `StartMatch` and never taken from a client
/// (SRV-10), so it is not part of this type at all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LobbyConfig {
    /// Round slots, `1..=100` (MTH-1).
    pub rounds: u32,
    /// Wall-clock limit of one round, `1..=3600` seconds.
    pub round_time_limit_secs: u64,
    /// `K`, the absent round starts that remove a player, `1..=10` (PRS-3).
    pub max_absent_round_starts: u32,
    /// Which games may be selected (TYP-1).
    pub filter: GameFilterMode,
}

impl LobbyConfig {
    /// Adds the server-drawn seed to make an engine `MatchConfig` (PRO-16, SRV-10).
    pub fn with_seed(self, seed: u64) -> MatchConfig {
        MatchConfig {
            rounds: self.rounds,
            round_time_limit_secs: self.round_time_limit_secs,
            seed,
            max_absent_round_starts: self.max_absent_round_starts,
            filter: self.filter,
        }
    }

    /// Drops the seed from an engine `MatchConfig`, for a snapshot (PRO-16).
    pub fn from_match_config(config: &MatchConfig) -> Self {
        Self {
            rounds: config.rounds,
            round_time_limit_secs: config.round_time_limit_secs,
            max_absent_round_starts: config.max_absent_round_starts,
            filter: config.filter.clone(),
        }
    }
}

/// Whether a new lobby is listed, without choosing its join code (PRO-6, SRV-4).
///
/// `core::ids::Visibility::Private` carries the code, which only the server may
/// generate (SRV-4), so the client asks for privacy without naming the code.
///
/// SPEC-GAP: the wire needs a code-free visibility; a client-supplied join code
/// would let it pick a guessable code (SRV-4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LobbyVisibility {
    /// Listed in `ListPublicLobbies` (SRV-5).
    Public,
    /// Joining requires the code (LOB-3).
    Private,
}

/// A partial settings change from the host (PRO-6, LOB-12).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsUpdate {
    /// New round count (MTH-1).
    pub rounds: Option<u32>,
    /// New per-round time limit.
    pub round_time_limit_secs: Option<u64>,
    /// New absent-round limit (PRS-3).
    pub max_absent_round_starts: Option<u32>,
    /// New game filter (TYP-1).
    pub filter: Option<GameFilterMode>,
    /// New visibility (PRO-6).
    pub visibility: Option<LobbyVisibility>,
}

/// Everything a client can send (PRO-6).
///
/// There is no score, place, outcome or timestamp message anywhere in this type,
/// because the server is authoritative for all of them (PRO-5, SEC-8). Premoves
/// are not a message either: a client sends an ordinary `GameInput` (PRO-12).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ClientMessage {
    /// Open a new lobby; the server assigns its id and join code (PRO-6, SRV-1).
    CreateLobby {
        /// The host's initial settings (PRO-16).
        config: LobbyConfig,
        /// Whether the lobby is listed (PRO-6).
        visibility: LobbyVisibility,
    },
    /// Join an existing lobby (PRO-6).
    JoinLobby {
        /// Which lobby (PRO-6).
        lobby_id: LobbyId,
        /// The code, required for a private lobby. It stays a `String` so a
        /// malformed code is a typed `InvalidJoinCode`, never a value that
        /// bypassed `JoinCode`'s validating constructor (LOB-3, SEC-10).
        code: Option<String>,
    },
    /// List the open public lobbies (PRO-6, SRV-5).
    ListPublicLobbies,
    /// Enter the matchmaking queue (PRO-6, MAT-2).
    JoinQueue {
        /// The preset id, `quick` today (MAT-1).
        preset: String,
    },
    /// Leave the matchmaking queue (PRO-6, MAT-4).
    LeaveQueue,
    /// Leave the current lobby (PRO-6, LOB-10).
    LeaveLobby,
    /// Accept a match or mark yourself ready (PRO-6, MAT-3).
    SetReady {
        /// The new ready state.
        ready: bool,
    },
    /// Change the lobby settings (PRO-6, LOB-6).
    UpdateSettings {
        /// The fields to change (PRO-6).
        update: SettingsUpdate,
    },
    /// Start the match (PRO-6, LOB-13).
    StartMatch,
    /// Cancel the lobby (PRO-6, LOB-14).
    CancelLobby,
    /// One game action for the running round (PRO-6, PRO-12).
    GameInput {
        /// The game's own opaque encoding; the engine never reads it (GAM-5).
        action: GameAction,
    },
    /// Latency probe for display only; the RTT the server uses comes from ping
    /// frames, not from this message (PRO-6, LAT-6, DEC-57).
    Ping {
        /// Echoed back unchanged in `Pong` (PRO-7).
        nonce: u64,
    },
}
