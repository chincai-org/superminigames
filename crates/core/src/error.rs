//! Configuration errors (TYP-1).
//!
//! `LobbyError`, `InputError` and `EngineError` describe engine state, so they
//! live in the engine crate (T2.6).

/// Why a configuration was rejected (TYP-1, LOB-12).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// `rounds` outside `1..=100`.
    #[error("rounds must be 1..=100")]
    RoundsOutOfRange,
    /// `round_time_limit_secs` outside `1..=3600`.
    #[error("round time limit must be 1..=3600 seconds")]
    TimeLimitOutOfRange,
    /// `max_absent_round_starts` outside `1..=10`.
    #[error("max absent round starts must be 1..=10")]
    AbsentLimitOutOfRange,
    /// An allow list with no games (LOB-12).
    #[error("the allow list must not be empty")]
    EmptyAllowList,
    /// The same game twice in one list (LOB-12).
    #[error("a game appears twice in the filter list")]
    DuplicateGameInList,
    /// A game that is not registered (LOB-12).
    #[error("unknown game: {0}")]
    UnknownGame(crate::ids::GameId),
    /// No game is eligible after filtering (LOB-12).
    #[error("no eligible games")]
    NoEligibleGames,
    /// Two games with the same id were registered (TYP-2).
    #[error("duplicate game id: {0}")]
    DuplicateGameId(crate::ids::GameId),
    /// A join code that is not exactly 6 characters from `A-Z` and `0-9` (TYP-1).
    #[error("a join code is exactly 6 characters from A-Z and 0-9")]
    InvalidJoinCode,
}
