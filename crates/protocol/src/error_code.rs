//! Structured error codes (PRO-8).
//!
//! The engine's `LobbyError`, `InputError` and `ConfigError` are engine types
//! (spec 1.1), so this crate cannot depend on them: the server owns the mapping
//! from an engine error to one of these codes (PRO-4, PRO-8).

use serde::{Deserialize, Serialize};

/// Every error the server can report, over the WebSocket or over HTTP (PRO-8).
///
/// The wire form is `snake_case` (plan section 3.5) and the message beside it is
/// human-readable text only, so a client MUST branch on the code, never on the
/// text (PRO-8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    // ---- protocol (PRO-2, PRO-10) ----
    /// `Envelope::v` is not `PROTOCOL_VERSION` (PRO-2).
    ProtocolMismatch,
    /// The frame could not be decoded or violated the schema (PRO-10, PRO-16).
    MalformedMessage,

    // ---- server level (PRO-8) ----
    /// The connection is not authenticated (SEC-7).
    Unauthorized,
    /// A rate limit or the anti-dodge block fired (SEC-9, MAT-5).
    RateLimited,
    /// Admission control, the hash pool or the drain is full (SEC-3, PERF-7).
    ServerBusy,
    /// The player already sits in a lobby or a queue (SRV-11, MAT-4).
    AlreadyInLobby,

    // ---- HTTP auth endpoints (PRO-8, section 21) ----
    /// Wrong username or password (SEC-5).
    InvalidCredentials,
    /// Username outside `^[A-Za-z0-9_-]{3,20}$` (SEC-19).
    InvalidUsername,
    /// Password below the policy (SEC-4).
    WeakPassword,
    /// Username already taken (SEC-19).
    UsernameTaken,
    /// The email is not a deliverable address (SEC-19).
    InvalidEmail,
    /// The account is still pending (SEC-19).
    EmailNotVerified,
    /// A verification, reset or WebSocket ticket is unknown, used or expired (SEC-7, SEC-19, SEC-20).
    InvalidToken,

    // ---- `LobbyError` (TYP-1) ----
    /// The command is not allowed in the current phase (section 12).
    WrongState,
    /// Only the host may run this command (LOB-13).
    NotHost,
    /// Both seats are taken (LOB-4).
    LobbyFull,
    /// The join code is unknown or malformed (LOB-3).
    InvalidJoinCode,
    /// Someone is ready, so settings are frozen (LOB-6).
    SettingsLocked,
    /// Not every seated player is ready (LOB-15).
    NotAllReady,
    /// Fewer than `MIN_PLAYERS` seated players (LOB-15).
    NotEnoughPlayers,
    /// The command names a player the lobby does not know (LOB-9).
    UnknownPlayer,

    // ---- `ConfigError` (TYP-1, LOB-12) ----
    /// A configuration value was rejected; the text names the field (LOB-12).
    InvalidConfig,

    // ---- `InputError` (TYP-1) ----
    /// No round is running (RND-1).
    NotInRound,
    /// The player is not seated in the running round (PRS-7).
    NotParticipant,
    /// The player left the round (PRS-6).
    PlayerLeftRound,
}

impl ErrorCode {
    /// The `snake_case` wire form (plan section 3.5).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProtocolMismatch => "protocol_mismatch",
            Self::MalformedMessage => "malformed_message",
            Self::Unauthorized => "unauthorized",
            Self::RateLimited => "rate_limited",
            Self::ServerBusy => "server_busy",
            Self::AlreadyInLobby => "already_in_lobby",
            Self::InvalidCredentials => "invalid_credentials",
            Self::InvalidUsername => "invalid_username",
            Self::WeakPassword => "weak_password",
            Self::UsernameTaken => "username_taken",
            Self::InvalidEmail => "invalid_email",
            Self::EmailNotVerified => "email_not_verified",
            Self::InvalidToken => "invalid_token",
            Self::WrongState => "wrong_state",
            Self::NotHost => "not_host",
            Self::LobbyFull => "lobby_full",
            Self::InvalidJoinCode => "invalid_join_code",
            Self::SettingsLocked => "settings_locked",
            Self::NotAllReady => "not_all_ready",
            Self::NotEnoughPlayers => "not_enough_players",
            Self::UnknownPlayer => "unknown_player",
            Self::InvalidConfig => "invalid_config",
            Self::NotInRound => "not_in_round",
            Self::NotParticipant => "not_participant",
            Self::PlayerLeftRound => "player_left_round",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}
