//! Lobby state machine (T2.2, LOB-1..LOB-16, section 4, section 12.1).
//!
//! `Lobby` is pure and synchronous: time is passed in as `tokio::time::Instant`
//! values and nothing reads the clock or spawns a task (ARC-4, ARC-6). The
//! actor owns the state and calls these methods as commands arrive.
//!
//! Every accepted phase change goes through [`next_phase`], and a trigger that
//! does not apply to the current phase is a [`LobbyError::WrongState`] (ARC-5).
//! `Leave` and `Connection` are the LOB-4 exceptions: outside `Phase::Lobby`
//! they are presence events for the running match (section 8, T2.6) and are
//! accepted here without a phase change.

use std::time::Duration;

use superminigames_core::config::{
    GameFilterMode, MAX_PLAYERS, MIN_PLAYERS, MatchConfig, SettingsUpdate,
};
use superminigames_core::error::ConfigError;
use superminigames_core::ids::{GameId, JoinCode, LobbyId, Player, PlayerId, Visibility};
use superminigames_logic::phase::{Phase, PhaseTrigger, next_phase};
use tokio::time::Instant;

use crate::error::LobbyError;
use crate::registry::GameRegistry;
use crate::report::LobbySnapshot;

/// How long a lobby in `Phase::Lobby` may go without a command before the
/// actor cancels it (LOB-16, SCP-4). Defined here because `core` does not
/// export it yet (SCP-4).
pub const LOBBY_IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// One lobby's state machine (T2.2, ARC-2).
pub struct Lobby {
    lobby_id: LobbyId,
    /// The creating player, who holds seat 0 (LOB-1).
    host: PlayerId,
    visibility: Visibility,
    config: MatchConfig,
    registry: GameRegistry,
    /// `(player, ready)` in seat order; seat 0 is the host (LOB-1).
    seats: Vec<(Player, bool)>,
    phase: Phase,
    /// The instant of the most recent command, for LOB-16.
    last_command: Instant,
}

impl Lobby {
    /// A fresh lobby: the host is seated at seat 0 and is not Ready (LOB-1).
    ///
    /// `config` is not validated here; the create path checks it (LOB-12 is
    /// enforced at `UpdateSettings` and `StartMatch`).
    pub fn new(
        lobby_id: LobbyId,
        host: Player,
        config: MatchConfig,
        visibility: Visibility,
        registry: GameRegistry,
        now: Instant,
    ) -> Self {
        let host_id = host.id;
        Self {
            lobby_id,
            host: host_id,
            visibility,
            config,
            registry,
            seats: vec![(host, false)],
            phase: Phase::Lobby,
            last_command: now,
        }
    }

    /// What a client may see of the lobby (PRO-14, PRO-15).
    pub fn snapshot(&self) -> LobbySnapshot {
        LobbySnapshot {
            lobby_id: self.lobby_id,
            host: self.host,
            seats: self.seats.clone(),
            config: self.config.clone(),
            private: matches!(self.visibility, Visibility::Private(_)),
            settings_locked: self.settings_locked(),
        }
    }

    /// The current engine phase (section 12.1).
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The games this lobby may select (TYP-2). The actor's stage B slot loop
    /// reads this to build each round's pool (PLL-1).
    pub fn registry(&self) -> &GameRegistry {
        &self.registry
    }

    /// True while any seated player is Ready, which locks settings (LOB-6).
    pub fn settings_locked(&self) -> bool {
        self.seats.iter().any(|(_, ready)| *ready)
    }

    /// True when the lobby has been in `Phase::Lobby` with no command for
    /// `LOBBY_IDLE_TIMEOUT` (LOB-16). This is a pure predicate; the actor's
    /// timer calls [`Lobby::expire_if_idle`] to act on it.
    pub fn is_idle(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_command) >= LOBBY_IDLE_TIMEOUT
    }

    /// The actor's LOB-16 timer target: if the phase is `Lobby` and
    /// [`Lobby::is_idle`] holds at `now`, cancel the lobby and return `true`;
    /// otherwise leave the state untouched and return `false`. The engine does
    /// not spawn timers itself (ARC-2).
    pub fn expire_if_idle(&mut self, now: Instant) -> bool {
        if self.phase != Phase::Lobby || !self.is_idle(now) {
            return false;
        }
        if let Some(phase) = next_phase(self.phase, PhaseTrigger::Cancel) {
            self.phase = phase;
            true
        } else {
            false
        }
    }

    /// A player joins an open lobby (LOB-2, LOB-3).
    ///
    /// Only valid in `Phase::Lobby` (LOB-4, LOB-14). LOB-2 and LOB-3 fix no
    /// precedence; the user ruled that the join code is validated first, so a
    /// private lobby with a missing or mismatched code is `InvalidJoinCode`
    /// even when full, and only a correct code reaches the `LobbyFull` check.
    /// The new player is seated last and unready.
    pub fn join(
        &mut self,
        player: Player,
        code: Option<JoinCode>,
        now: Instant,
    ) -> Result<(), LobbyError> {
        self.last_command = now;
        self.require_lobby()?;
        if let Visibility::Private(expected) = &self.visibility
            && code.as_ref() != Some(expected)
        {
            return Err(LobbyError::InvalidJoinCode);
        }
        if self.seats.len() >= MAX_PLAYERS {
            return Err(LobbyError::LobbyFull);
        }
        self.seats.push((player, false));
        Ok(())
    }

    /// A player leaves the lobby (LOB-11, LOB-4).
    ///
    /// In `Phase::Lobby` the host leaving cancels the lobby and a non-host
    /// leaving frees their seat and drops their Ready flag. Outside `Lobby`
    /// this is a presence event for the match (section 8, T2.6) and is
    /// accepted without a phase change.
    pub fn leave(&mut self, player: PlayerId, now: Instant) -> Result<(), LobbyError> {
        self.last_command = now;
        if self.phase != Phase::Lobby {
            return Ok(());
        }
        let index = self.seat_index(player).ok_or(LobbyError::UnknownPlayer)?;
        if player == self.host {
            self.phase = self.transition(PhaseTrigger::HostLeft)?;
        } else {
            self.seats.remove(index);
        }
        Ok(())
    }

    /// A connection state change from the server (LOB-15).
    ///
    /// In `Phase::Lobby` a disconnect for a seated player is exactly a
    /// [`Lobby::leave`]; every other case (a connect, or any state change for
    /// an unseated player) is ignored. Outside `Lobby` it is a presence event
    /// for the match (section 8, T2.6).
    pub fn connection(
        &mut self,
        player: PlayerId,
        connected: bool,
        now: Instant,
    ) -> Result<(), LobbyError> {
        self.last_command = now;
        if self.phase != Phase::Lobby {
            return Ok(());
        }
        if !connected && self.seat_index(player).is_some() {
            return self.leave(player, now);
        }
        Ok(())
    }

    /// A seated player sets or clears Ready (LOB-5). Repeatable.
    pub fn set_ready(
        &mut self,
        player: PlayerId,
        ready: bool,
        now: Instant,
    ) -> Result<(), LobbyError> {
        self.last_command = now;
        self.require_lobby()?;
        let index = self.seat_index(player).ok_or(LobbyError::UnknownPlayer)?;
        self.seats[index].1 = ready;
        Ok(())
    }

    /// The seat index of `player`, if seated.
    fn seat_index(&self, player: PlayerId) -> Option<usize> {
        self.seats
            .iter()
            .position(|(seated, _)| seated.id == player)
    }

    /// `WrongState` unless the lobby is still in `Phase::Lobby` (LOB-4).
    fn require_lobby(&self) -> Result<(), LobbyError> {
        if self.phase == Phase::Lobby {
            Ok(())
        } else {
            Err(LobbyError::WrongState)
        }
    }

    /// The trigger's next phase, or `WrongState` when it does not apply to the
    /// current phase (section 12.1, VER-14).
    fn transition(&self, trigger: PhaseTrigger) -> Result<Phase, LobbyError> {
        next_phase(self.phase, trigger).ok_or(LobbyError::WrongState)
    }
}

/// The settings and lifecycle commands of T2.2, in a second block so the
/// read-only surface above stays separate from the mutating one.
impl Lobby {
    /// The host replaces settings (LOB-6). Locked by `NotHost`, by any seated
    /// player being Ready (`SettingsLocked`) and by an invalid candidate
    /// (`Config`). A rejected update leaves the config unchanged (LOB-12).
    pub fn update_settings(
        &mut self,
        player: PlayerId,
        update: SettingsUpdate,
        now: Instant,
    ) -> Result<(), LobbyError> {
        self.last_command = now;
        self.require_lobby()?;
        if player != self.host {
            return Err(LobbyError::NotHost);
        }
        if self.settings_locked() {
            return Err(LobbyError::SettingsLocked);
        }
        let mut config = self.config.clone();
        apply_update(&mut config, &update);
        validate_config(&config, &self.registry)?;
        self.config = config;
        if let Some(visibility) = update.visibility {
            self.visibility = visibility;
        }
        Ok(())
    }

    /// The host cancels the lobby (LOB-10). Only valid in `Phase::Lobby`.
    pub fn cancel(&mut self, player: PlayerId, now: Instant) -> Result<(), LobbyError> {
        self.last_command = now;
        self.require_lobby()?;
        if player != self.host {
            return Err(LobbyError::NotHost);
        }
        self.phase = self.transition(PhaseTrigger::Cancel)?;
        Ok(())
    }

    /// The host starts the match with the server-drawn `seed` (LOB-9).
    ///
    /// The lobby needs `MIN_PLAYERS` seated and every seated player Ready.
    /// On success `seed` is stored in `config.seed`, the seats freeze and the
    /// phase becomes `Starting` (LOB-7).
    pub fn start_match(
        &mut self,
        player: PlayerId,
        seed: u64,
        now: Instant,
    ) -> Result<(), LobbyError> {
        self.last_command = now;
        self.require_lobby()?;
        if player != self.host {
            return Err(LobbyError::NotHost);
        }
        if self.seats.len() < MIN_PLAYERS {
            return Err(LobbyError::NotEnoughPlayers);
        }
        if self.seats.iter().any(|(_, ready)| !*ready) {
            return Err(LobbyError::NotAllReady);
        }
        validate_config(&self.config, &self.registry)?;
        self.config.seed = seed;
        self.phase = self.transition(PhaseTrigger::StartMatch)?;
        Ok(())
    }
}

/// Applies a partial settings change to `config` (LOB-6). Visibility is not
/// part of `MatchConfig`, so `UpdateSettings` stores it on the lobby.
fn apply_update(config: &mut MatchConfig, update: &SettingsUpdate) {
    if let Some(rounds) = update.rounds {
        config.rounds = rounds;
    }
    if let Some(secs) = update.round_time_limit_secs {
        config.round_time_limit_secs = secs;
    }
    if let Some(k) = update.max_absent_round_starts {
        config.max_absent_round_starts = k;
    }
    if let Some(filter) = &update.filter {
        config.filter = filter.clone();
    }
}

/// Validates a candidate config against the registry (LOB-12), at both
/// `UpdateSettings` and `StartMatch`. Filter lists are checked before the
/// "at least one eligible game" rule, so the most specific error wins.
fn validate_config(config: &MatchConfig, registry: &GameRegistry) -> Result<(), LobbyError> {
    if !(1..=100).contains(&config.rounds) {
        return Err(LobbyError::Config(ConfigError::RoundsOutOfRange));
    }
    if !(1..=3600).contains(&config.round_time_limit_secs) {
        return Err(LobbyError::Config(ConfigError::TimeLimitOutOfRange));
    }
    if !(1..=10).contains(&config.max_absent_round_starts) {
        return Err(LobbyError::Config(ConfigError::AbsentLimitOutOfRange));
    }
    if let GameFilterMode::AllowList(ids) = &config.filter
        && ids.is_empty()
    {
        return Err(LobbyError::Config(ConfigError::EmptyAllowList));
    }
    let ids = match &config.filter {
        GameFilterMode::AllowList(ids) | GameFilterMode::BanList(ids) => ids,
    };
    if has_duplicate(ids) {
        return Err(LobbyError::Config(ConfigError::DuplicateGameInList));
    }
    for id in ids {
        if registry.get(id).is_none() {
            return Err(LobbyError::Config(ConfigError::UnknownGame(id.clone())));
        }
    }
    let any_eligible = registry.games().iter().any(|game| match &config.filter {
        GameFilterMode::AllowList(ids) => ids.contains(game.id()),
        GameFilterMode::BanList(ids) => !ids.contains(game.id()),
    });
    if !any_eligible {
        return Err(LobbyError::Config(ConfigError::NoEligibleGames));
    }
    Ok(())
}

/// Whether `ids` repeats an id (LOB-12). A linear scan avoids a `HashMap`,
/// which engine crates may not use (CI-2).
fn has_duplicate(ids: &[GameId]) -> bool {
    ids.iter()
        .enumerate()
        .any(|(index, id)| ids[index + 1..].contains(id))
}
