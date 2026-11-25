//! The engine actor: one task owns all lobby and match state (T2.6, ARC-2,
//! ARC-3, ARC-4, ARC-5, ARC-9, ARC-10).
//!
//! [`spawn_engine`] builds the lobby, validates its initial config (LOB-12,
//! section 12.1 row 1), spawns the owning task and returns its
//! [`EngineHandle`]. The task is the only reader of time: it reads
//! `tokio::time::Instant::now()` for every command (ARC-4), and the LOB-16 idle
//! deadline rides in the same `select!`. Nothing is shared and nothing is
//! locked (ARC-2, CI-6).
//!
//! The actor drives the lobby and its match slots from one task, including
//! presence checks, selected games, deadlines, reports and overload handling.

use std::time::Duration;
use superminigames_core::config::{MatchConfig, SettingsUpdate};
use superminigames_core::game::{GameAction, GameEvent};
use superminigames_core::ids::{GameId, JoinCode, LobbyId, MatchId, Player, PlayerId, Visibility};
use superminigames_logic::phase::{Phase, PhaseTrigger, next_phase};

use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::error::{EngineError, InputError, LobbyError};
use crate::lobby::{LOBBY_IDLE_TIMEOUT, Lobby};
use crate::presence::{apply_round_start, final_ranking};
use crate::registry::GameRegistry;
use crate::report::{
    LobbySnapshot, MatchReport, PlayerState, PlayerStatus, RoundRecord, RoundStatus, SkipReason,
    Standing,
};
use crate::round::{RoundEvent, RoundInputError, RoundRunner, StartFailed};
use crate::selection::{first_mover_seat, opening_bit, pick};

/// The inbox depth used by [`spawn_engine`] unless the server overrides
/// `EngineOptions::inbox_capacity` (ARC-2, SCP-4).
pub const INBOX_CAPACITY: usize = 256;

/// The outbound event depth (ARC-8). Tests override it through
/// [`spawn_engine_with_event_capacity`] to drive the ARC-10 overload path.
pub const DEFAULT_EVENT_CAPACITY: usize = 64;

/// A command the server sends to one actor (TYP-1, ARC-2).
#[derive(Clone, Debug)]
pub enum Command {
    /// A player joins an open lobby (LOB-2, LOB-3).
    Join {
        player: Player,
        code: Option<JoinCode>,
    },
    /// A player leaves the lobby or the match (LOB-11, LOB-15, section 8).
    Leave { player: PlayerId },
    /// A seated player sets or clears Ready (LOB-5).
    SetReady { player: PlayerId, ready: bool },
    /// The host replaces the lobby settings (LOB-6).
    UpdateSettings {
        player: PlayerId,
        update: SettingsUpdate,
    },
    /// The host cancels the lobby (LOB-10).
    Cancel { player: PlayerId },
    /// The host starts the match with the server-drawn seed (LOB-9, SRV-10).
    StartMatch { player: PlayerId, seed: u64 },
    /// A transport connection change from the server (LOB-15, PRS-1).
    Connection { player: PlayerId, connected: bool },
    /// In-round game input (section 7).
    Input {
        player: PlayerId,
        action: GameAction,
        lag_credit_ms: u32,
        received_at: Instant,
    },
}

/// Something an actor publishes to the server (TYP-1, ARC-3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineEvent {
    /// The client-visible lobby state after an accepted command (PRO-14).
    LobbyChanged(LobbySnapshot),
    /// A validated phase transition (ARC-3, section 12.1).
    PhaseChanged(Phase),
    /// A slot is starting (section 5, stage B).
    RoundStarting {
        round_index: u32,
        game: GameId,
        participants: Vec<PlayerId>,
        time_limit_secs: u64,
    },
    /// An event the running game produced (GAM-6).
    Game { round_index: u32, event: GameEvent },
    /// A player's observable presence (PRS-6).
    PlayerStatus {
        player: PlayerId,
        status: PlayerStatus,
    },
    /// A slot finished (MTH-2, stage B).
    RoundFinished(RoundRecord),
    /// The match is over (MTH-3).
    MatchFinished(MatchReport),
}

/// How [`spawn_engine`] is configured (TYP-1, ARC-9).
#[derive(Clone, Copy, Debug)]
pub struct EngineOptions {
    /// Record a `ReplayLog` per round (REP-1, REP-3).
    pub record_replays: bool,
    /// The bounded inbox depth (ARC-2).
    pub inbox_capacity: usize,
}

/// Everything the server passes to create one actor (TYP-1, ARC-9).
pub struct EngineInit {
    /// Assigned by the server (SRV-1).
    pub lobby_id: LobbyId,
    /// Assigned by the server; the engine never generates ids (TYP-3).
    pub match_id: MatchId,
    /// The creating player, who holds seat 0 (LOB-1).
    pub host: Player,
    /// The initial settings (LOB-12).
    pub config: MatchConfig,
    /// `Public` or `Private(code)` (LOB-3).
    pub visibility: Visibility,
    /// The games this match may select (TYP-2).
    pub registry: GameRegistry,
    /// Actor tuning (ARC-2, REP-3).
    pub options: EngineOptions,
}

/// The server's handle to one running actor (ARC-9).
pub struct EngineHandle {
    inbox: mpsc::Sender<Envelope>,
    events: Option<mpsc::Receiver<EngineEvent>>,
}

impl EngineHandle {
    /// Sends a command and waits for its typed result (ARC-2, ARC-9).
    ///
    /// # Errors
    /// Returns the command's own [`EngineError`]. Once the actor has stopped,
    /// a lobby command is [`LobbyError::WrongState`] and input is
    /// [`InputError::NotInRound`], exactly as the terminal phase would answer
    /// (LOB-4, section 8, ARC-10).
    pub async fn send(&self, command: Command) -> Result<(), EngineError> {
        let on_stop = terminal_error(&command);
        let (reply, receiver) = oneshot::channel();
        if self.inbox.send(Envelope { command, reply }).await.is_err() {
            return Err(on_stop);
        }
        match receiver.await {
            Ok(result) => result,
            Err(_) => Err(on_stop),
        }
    }

    /// Takes the single outbound event stream (ARC-3, ARC-9).
    ///
    /// An actor serves one consumer, so this returns the stream once; a second
    /// call returns an already-closed stream.
    pub fn subscribe(&mut self) -> mpsc::Receiver<EngineEvent> {
        self.events.take().unwrap_or_else(closed_event_stream)
    }
}

/// The typed error a stopped actor answers with (ARC-10, LOB-4, section 8).
fn terminal_error(command: &Command) -> EngineError {
    match command {
        Command::Input { .. } => EngineError::Input(InputError::NotInRound),
        _ => EngineError::Lobby(LobbyError::WrongState),
    }
}

/// A stream that is closed from the start (a second [`EngineHandle::subscribe`]).
fn closed_event_stream() -> mpsc::Receiver<EngineEvent> {
    let (sender, receiver) = mpsc::channel(1);
    drop(sender);
    receiver
}

/// One inbox item: a command and the channel its result returns on (ARC-2).
struct Envelope {
    command: Command,
    reply: oneshot::Sender<Result<(), EngineError>>,
}

/// Creates the actor and returns its handle (ARC-2, ARC-9).
///
/// The initial config is validated through the lobby's own path (LOB-12,
/// section 12.1 row 1), so a bad config fails here rather than at `StartMatch`.
///
/// # Errors
/// Returns [`LobbyError::Config`] when `init.config` fails validation.
pub fn spawn_engine(init: EngineInit) -> Result<EngineHandle, LobbyError> {
    spawn_engine_with_event_capacity(init, DEFAULT_EVENT_CAPACITY)
}

/// [`spawn_engine`] with an explicit outbound event capacity (ARC-8, ARC-10).
///
/// Normally the capacity is [`DEFAULT_EVENT_CAPACITY`]; tests use a tiny value
/// to fill the channel and drive the overload path.
#[doc(hidden)]
pub fn spawn_engine_with_event_capacity(
    init: EngineInit,
    event_capacity: usize,
) -> Result<EngineHandle, LobbyError> {
    let EngineInit {
        lobby_id,
        match_id,
        host,
        config,
        visibility,
        registry,
        options,
    } = init;
    let host_id = host.id;
    // The actor's own clock: construction happens inside the runtime with
    // tokio time, so paused-time tests see a deterministic start (ARC-4).
    let now = Instant::now();
    let mut lobby = Lobby::new(lobby_id, host, config, visibility, registry, now);
    // Reuse the lobby's validation (LOB-12): an empty update validates the
    // candidate config and changes nothing else.
    lobby.update_settings(host_id, SettingsUpdate::default(), now)?;

    let (events, event_stream) = mpsc::channel(event_capacity.max(1));
    let (inbox, receiver) = mpsc::channel(options.inbox_capacity.max(1));
    let snapshot = lobby.snapshot();
    let states = snapshot
        .seats
        .iter()
        .map(|(player, ready)| PlayerState {
            player: player.clone(),
            ready: *ready,
            connected: true,
            standing: Standing::InMatch,
            absent_streak: 0,
            total_points: 0,
        })
        .collect();
    let phase = lobby.phase();
    let config = snapshot.config;
    let lobby_id = snapshot.lobby_id;
    let opening = opening_bit(config.seed);
    let actor = Actor {
        lobby,
        phase,
        states,
        rounds: Vec::new(),
        current_removed: Vec::new(),
        config,
        lobby_id,
        opening,
        games_started: 0,
        current_round: 0,
        ended_early: false,
        runner: None,
        record_replays: options.record_replays,
        match_id,
        host: host_id,
        events,
        last_command: now,
        overloaded: false,
        game_wake: None,
        slot_away_announced: Vec::new(),
    };
    let _task = tokio::spawn(actor.run(receiver));
    Ok(EngineHandle {
        inbox,
        events: Some(event_stream),
    })
}

/// The single owner of one lobby and its match (ARC-2).
struct Actor {
    lobby: Lobby,
    phase: Phase,
    states: Vec<PlayerState>,
    rounds: Vec<RoundRecord>,
    current_removed: Vec<PlayerId>,
    config: MatchConfig,
    lobby_id: LobbyId,
    opening: u8,
    games_started: usize,
    current_round: u32,
    ended_early: bool,
    runner: Option<Box<RoundRunner>>,
    record_replays: bool,
    /// Retained for the `MatchReport` that stage B builds (MTH-3).
    match_id: MatchId,
    /// The host, used to force the ARC-10 terminal transition.
    host: PlayerId,
    events: mpsc::Sender<EngineEvent>,
    /// Mirrors the lobby's idle clock so the actor can time its own wake-up
    /// (LOB-16). Every received command updates it, accepted or not (LOB-13).
    last_command: Instant,
    /// Set when an event could not be enqueued (ARC-10).
    overloaded: bool,
    /// The game wake-up instant the actor already ticked this round (GAM-6).
    /// A game that keeps reporting the same instant is not re-armed.
    game_wake: Option<Instant>,
    /// Participants already reported `Away` while this slot was `Starting`
    /// (PRS-12). `seed_disconnected` must not repeat those events at `InRound`
    /// (RND-13), so they are filtered when the grace is seeded.
    slot_away_announced: Vec<PlayerId>,
}

impl Actor {
    async fn run(mut self, mut inbox: mpsc::Receiver<Envelope>) {
        while let Some(envelope) = self.next_envelope(&mut inbox).await {
            let now = Instant::now();
            let start_requested = matches!(envelope.command, Command::StartMatch { .. });
            let result = self.dispatch(envelope.command, now);
            let _ = envelope.reply.send(result);
            if self.overloaded {
                self.finish_overloaded();
                return;
            }
            if start_requested && self.phase == Phase::Starting {
                tokio::task::yield_now().await;
                self.run_match(&mut inbox).await;
                if self.overloaded {
                    self.finish_overloaded();
                }
                return;
            }
        }
    }

    /// Waits for the next command or the LOB-16 idle deadline (ARC-4).
    ///
    /// Returns `None` when the handle was dropped (inbox closed) or an idle
    /// lobby was cancelled; either way the actor ends.
    async fn next_envelope(&mut self, inbox: &mut mpsc::Receiver<Envelope>) -> Option<Envelope> {
        loop {
            tokio::select! {
                biased;
                envelope = inbox.recv() => return envelope,
                () = tokio::time::sleep_until(self.idle_at()), if self.in_lobby() => {
                    if self.expire_idle() {
                        return None;
                    }
                }
            }
        }
    }

    /// When the actor must wake to enforce LOB-16.
    fn idle_at(&self) -> Instant {
        self.last_command + LOBBY_IDLE_TIMEOUT
    }

    /// True while the lobby phase is the current phase.
    fn in_lobby(&self) -> bool {
        self.phase == Phase::Lobby
    }

    /// LOB-16: cancel an idle lobby. Returns true when the phase reached
    /// `Cancelled`, so the actor ends.
    fn expire_idle(&mut self) -> bool {
        if !self.lobby.expire_if_idle(Instant::now()) {
            return false;
        }
        tracing::debug!(host = ?self.host, "lobby expired while idle (LOB-16)");
        self.phase = Phase::Cancelled;
        self.publish(EngineEvent::PhaseChanged(Phase::Cancelled));
        true
    }

    /// Applies one command and publishes what it changed (ARC-3, ARC-5).
    fn dispatch(&mut self, command: Command, now: Instant) -> Result<(), EngineError> {
        self.last_command = now;
        match command {
            Command::Join { player, code } => {
                self.apply_lobby(now, |lobby, now| lobby.join(player, code, now))
            }
            Command::Leave { player } => {
                if self.in_lobby() {
                    self.apply_lobby(now, |lobby, now| lobby.leave(player, now))
                } else if self.phase == Phase::Starting || self.phase == Phase::RoundFinished {
                    self.set_connected(player, false);
                    Ok(())
                } else {
                    self.handle_round_command(Command::Leave { player }, self.current_round)
                }
            }
            Command::SetReady { player, ready } => {
                self.apply_lobby(now, |lobby, now| lobby.set_ready(player, ready, now))
            }
            Command::UpdateSettings { player, update } => {
                self.apply_lobby(now, |lobby, now| lobby.update_settings(player, update, now))
            }
            Command::Cancel { player } => {
                self.apply_lobby(now, |lobby, now| lobby.cancel(player, now))
            }
            Command::StartMatch { player, seed } => {
                match self.lobby.start_match(player, seed, now) {
                    Ok(()) => {
                        let snapshot = self.lobby.snapshot();
                        self.config = snapshot.config;
                        self.states = snapshot
                            .seats
                            .iter()
                            .map(|(player, ready)| PlayerState {
                                player: player.clone(),
                                ready: *ready,
                                connected: true,
                                standing: Standing::InMatch,
                                absent_streak: 0,
                                total_points: 0,
                            })
                            .collect();
                        self.opening = opening_bit(self.config.seed);
                        self.publish(EngineEvent::LobbyChanged(self.lobby.snapshot()));
                        self.advance(PhaseTrigger::StartMatch)
                            .map_err(EngineError::Lobby)
                    }
                    Err(error) => Err(EngineError::Lobby(error)),
                }
            }
            Command::Connection { player, connected } => {
                if self.in_lobby() {
                    self.apply_lobby(now, |lobby, now| lobby.connection(player, connected, now))
                } else if self.phase == Phase::Starting || self.phase == Phase::RoundFinished {
                    self.set_connected(player, connected);
                    Ok(())
                } else {
                    self.handle_round_command(
                        Command::Connection { player, connected },
                        self.current_round,
                    )
                }
            }
            // Stage A never enters `Phase::InRound`, so every input is out of
            // round (section 8). Stage B routes it to the running round.
            Command::Input { .. } => Err(EngineError::Input(InputError::NotInRound)),
        }
    }

    /// Runs one lobby command, publishing `LobbyChanged` and any phase change
    /// on success (ARC-3, LOB-4).
    fn apply_lobby(
        &mut self,
        now: Instant,
        command: impl FnOnce(&mut Lobby, Instant) -> Result<(), LobbyError>,
    ) -> Result<(), EngineError> {
        let before = self.phase;
        match command(&mut self.lobby, now) {
            Ok(()) => {
                self.phase = self.lobby.phase();
                self.publish(EngineEvent::LobbyChanged(self.lobby.snapshot()));
                self.publish_phase_change(before);
                Ok(())
            }
            Err(error) => Err(EngineError::Lobby(error)),
        }
    }

    /// Publishes `PhaseChanged` when the last accepted command moved the phase
    /// (ARC-3).
    fn publish_phase_change(&mut self, before: Phase) {
        let after = self.phase;
        if after != before {
            self.publish(EngineEvent::PhaseChanged(after));
        }
    }

    /// Validates and publishes one state-machine transition (ARC-5).
    fn advance(&mut self, trigger: PhaseTrigger) -> Result<(), LobbyError> {
        let next = next_phase(self.phase, trigger).ok_or(LobbyError::WrongState)?;
        self.phase = next;
        self.publish(EngineEvent::PhaseChanged(next));
        Ok(())
    }

    /// Runs the ordered match slots (MTH-1, MTH-2).
    async fn run_match(&mut self, inbox: &mut mpsc::Receiver<Envelope>) {
        for round_index in 0..self.config.rounds {
            tokio::task::yield_now().await;
            self.process_starting_commands(inbox);
            if self.overloaded {
                return;
            }
            if self.phase != Phase::Starting {
                self.cancel_match();
                return;
            }
            if !self.run_slot(inbox, round_index).await {
                return;
            }
            if self.phase == Phase::RoundFinished && self.rounds.len() < self.config.rounds as usize
            {
                let _ = self.advance(PhaseTrigger::MoreSlotsRemain);
            }
            if self.overloaded {
                return;
            }
        }
    }

    /// Drains presence commands that arrived between slots (PRS-12).
    ///
    /// The host keeps no special power after `StartMatch` (DEC-15): a disconnect
    /// is ordinary presence, not a cancel. The phase is `Starting`, so the
    /// change is announced and nothing else happens.
    fn process_starting_commands(&mut self, inbox: &mut mpsc::Receiver<Envelope>) {
        while let Ok(envelope) = inbox.try_recv() {
            self.last_command = Instant::now();
            let result = match envelope.command {
                Command::Connection { player, connected } => {
                    self.set_connected(player, connected);
                    self.announce_starting_presence(player, connected);
                    Ok(())
                }
                Command::Leave { player } => {
                    self.set_connected(player, false);
                    self.announce_starting_presence(player, false);
                    Ok(())
                }
                Command::Input { .. } => Err(EngineError::Input(InputError::NotInRound)),
                _ => Err(EngineError::Lobby(LobbyError::WrongState)),
            };
            let _ = envelope.reply.send(result);
            if self.overloaded {
                return;
            }
        }
    }

    fn cancel_match(&mut self) {
        self.phase = Phase::Cancelled;
        self.publish(EngineEvent::PhaseChanged(Phase::Cancelled));
    }

    async fn run_slot(&mut self, inbox: &mut mpsc::Receiver<Envelope>, round_index: u32) -> bool {
        self.current_round = round_index;
        let start = apply_round_start(&mut self.states, self.config.max_absent_round_starts);
        self.current_removed = start.removed.clone();
        for &player in &start.removed {
            self.publish(EngineEvent::PlayerStatus {
                player,
                status: PlayerStatus::Removed,
            });
        }
        if self
            .states
            .iter()
            .filter(|s| s.standing == Standing::InMatch)
            .count()
            <= 1
        {
            self.ended_early = true;
            if self.advance(PhaseTrigger::PresenceCheckFailed).is_err() {
                self.cancel_match();
                return false;
            }
            self.publish_report();
            return false;
        }
        let participants = self
            .states
            .iter()
            .filter(|s| s.standing == Standing::InMatch && s.connected)
            .map(|s| s.player.id)
            .collect::<Vec<_>>();
        if participants.len() < 2 {
            return self.skip_slot(
                round_index,
                participants,
                RoundStatus::Skipped(SkipReason::NotEnoughPlayers),
                0,
                start.removed.clone(),
            );
        }
        let Some(runner) = self
            .start_selected_slot(inbox, round_index, &participants, start.removed.clone())
            .await
        else {
            return self.phase != Phase::MatchFinished
                && self.phase != Phase::Cancelled
                && !self.overloaded;
        };
        self.runner = Some(runner);
        self.run_round(inbox, round_index).await
    }

    async fn run_round(&mut self, inbox: &mut mpsc::Receiver<Envelope>, round_index: u32) -> bool {
        loop {
            // TMR-2: all four waits are armed at once. `if` guards keep a
            // timer arm off when there is nothing to wait for, so a paused-time
            // test never depends on the executor's "all tasks idle" fallback.
            let game_wake = self.game_wake;
            let Some((deadline, grace, game)) = self.runner.as_mut().map(|runner| {
                let deadline = runner.deadline();
                let grace = runner.next_grace_deadline();
                // GAM-6/TMR-2: the game is asked again after every tick. A
                // wake-up that overflows the clock, or that repeats the instant
                // the actor already ticked, is not a timer: arming it would
                // spin the actor instead of waiting.
                let game = runner
                    .next_wakeup_ms()
                    .and_then(|ms| runner.round_start().checked_add(Duration::from_millis(ms)))
                    .filter(|due| Some(*due) != game_wake);
                (deadline, grace, game)
            }) else {
                self.cancel_match();
                return false;
            };
            let deadline_due = tokio::time::sleep_until(deadline);
            let grace_due = tokio::time::sleep_until(grace.unwrap_or(deadline));
            let game_due = tokio::time::sleep_until(game.unwrap_or(deadline));
            let wake = tokio::select! {
                biased;
                // TMR-2, PRS-7: a due timer is applied before a queued command,
                // so a grace that expired at this instant disqualifies the player
                // before their input can be accepted.
                () = deadline_due => Wake::Deadline,
                () = grace_due, if grace.is_some() => Wake::Grace,
                () = game_due, if game.is_some() => Wake::Game,
                envelope = inbox.recv() => Wake::Input(envelope),
            };
            match wake {
                Wake::Deadline => break,
                Wake::Input(None) => return false,
                Wake::Grace => {
                    self.last_command = Instant::now();
                    let step = match self.runner.as_mut() {
                        Some(runner) => runner.expire(Instant::now()),
                        None => break,
                    };
                    self.publish_round_events(round_index, step.events);
                    if self.overloaded {
                        return false;
                    }
                    if self.runner.as_ref().is_none_or(|runner| runner.ended()) {
                        break;
                    }
                }
                Wake::Game => {
                    // Remember the instant, so a game that keeps reporting it
                    // does not re-arm (GAM-6, TMR-2).
                    self.game_wake = game;
                    self.last_command = Instant::now();
                    let step = match self.runner.as_mut() {
                        Some(runner) => runner.tick_advanced(Instant::now()),
                        None => break,
                    };
                    self.publish_round_events(round_index, step.events);
                    if self.overloaded {
                        return false;
                    }
                    if self.runner.as_ref().is_none_or(|runner| runner.ended()) {
                        break;
                    }
                }
                Wake::Input(Some(envelope)) => {
                    self.last_command = Instant::now();
                    let result = self.handle_round_command(envelope.command, round_index);
                    let _ = envelope.reply.send(result);
                    if self.overloaded {
                        return false;
                    }
                    if self.runner.as_ref().is_none_or(|runner| runner.ended()) {
                        break;
                    }
                }
            }
        }
        let Some(runner) = self.runner.take() else {
            self.cancel_match();
            return false;
        };
        let aborted = runner.aborted();
        let record = runner.finish(
            &mut self.states,
            round_index,
            self.current_removed.clone(),
            &[],
            self.record_replays,
        );
        let trigger = if aborted
            || matches!(
                record.status,
                RoundStatus::Skipped(SkipReason::GameAborted | SkipReason::InvalidReport)
            ) {
            PhaseTrigger::RoundAborted
        } else {
            PhaseTrigger::RoundEnded
        };
        if self.advance(trigger).is_err() {
            self.cancel_match();
            return false;
        }
        if trigger == PhaseTrigger::RoundEnded || trigger == PhaseTrigger::RoundAborted {
            self.games_started += 1;
        }
        self.finish_slot(record)
    }

    fn handle_round_command(
        &mut self,
        command: Command,
        round_index: u32,
    ) -> Result<(), EngineError> {
        if self.phase == Phase::MatchFinished || self.phase == Phase::Cancelled {
            return Err(match command {
                Command::Input { .. } => EngineError::Input(InputError::NotInRound),
                _ => EngineError::Lobby(LobbyError::WrongState),
            });
        }
        match command {
            Command::Input {
                player,
                action,
                lag_credit_ms,
                received_at,
            } => {
                let step = match self.runner.as_mut() {
                    Some(runner) => runner.submit_input(received_at, player, action, lag_credit_ms),
                    None => return Err(EngineError::Input(InputError::NotInRound)),
                };
                match step {
                    Ok(step) => {
                        self.publish_round_events(round_index, step.events);
                        Ok(())
                    }
                    Err(error) => Err(EngineError::Input(match error {
                        RoundInputError::NotInRound => InputError::NotInRound,
                        RoundInputError::NotParticipant => InputError::NotParticipant,
                        RoundInputError::PlayerLeftRound => InputError::PlayerLeftRound,
                    })),
                }
            }
            Command::Leave { player } => {
                self.set_connected(player, false);
                self.round_connection(player, false, round_index);
                Ok(())
            }
            Command::Connection { player, connected } => {
                self.set_connected(player, connected);
                self.round_connection(player, connected, round_index);
                Ok(())
            }
            _ => Err(EngineError::Lobby(LobbyError::WrongState)),
        }
    }

    fn set_connected(&mut self, player: PlayerId, connected: bool) {
        if let Some(state) = self
            .states
            .iter_mut()
            .find(|state| state.player.id == player)
        {
            state.connected = connected;
        }
    }

    /// Feeds a mid-round presence change to the runner and publishes what it
    /// reports (PRS-6, PRS-11): `Away` with a fresh grace, or `Present` when a
    /// reconnect lands inside the grace. A command that arrives after the round
    /// is over changes nothing in the runner (TMR-4).
    fn round_connection(&mut self, player: PlayerId, connected: bool, round_index: u32) {
        let events = match self.runner.as_mut() {
            Some(runner) => runner.on_connection(player, connected, Instant::now()),
            None => Vec::new(),
        };
        self.publish_round_events(round_index, events);
    }

    fn publish_round_events(&mut self, round_index: u32, events: Vec<RoundEvent>) {
        for event in events {
            match event {
                RoundEvent::Game(event) => self.publish(EngineEvent::Game { round_index, event }),
                RoundEvent::PlayerStatus { player, status } => {
                    self.publish(EngineEvent::PlayerStatus { player, status })
                }
            }
        }
    }

    /// Starts one selected slot (RND-5, RND-6).
    ///
    /// The start attempts run on a spawned task and this actor keeps reading
    /// its inbox while they do (T-PRS-9): a `Connection` that arrives between
    /// the presence snapshot and a successful `initialize` is applied to
    /// `states` right away, and [`RoundRunner::seed_disconnected`] seeds that
    /// player's grace at `at_ms = 0` the moment the runner exists (RND-13).
    ///
    /// Returns `None` when the slot was skipped or the actor must stop; either
    /// way the caller checks the phase to tell the two apart.
    async fn start_selected_slot(
        &mut self,
        inbox: &mut mpsc::Receiver<Envelope>,
        round_index: u32,
        participants: &[PlayerId],
        removed: Vec<PlayerId>,
    ) -> Option<Box<RoundRunner>> {
        self.game_wake = None;
        self.slot_away_announced.clear();
        let selected = {
            let pool = self
                .lobby
                .registry()
                .pool(&self.config.filter, participants.len());
            pick(&pool, self.config.seed, u64::from(round_index))
                .map(|pick| pick.game.owned_pick(pick.game_seed))
        };
        let Some(selected) = selected else {
            self.skip_slot(
                round_index,
                participants.to_vec(),
                RoundStatus::Skipped(SkipReason::EmptyPool),
                0,
                removed,
            );
            return None;
        };
        let time_limit_ms = self.config.round_time_limit_secs * 1000;
        // RND-12, T-PRS-9: the task takes the factory and seed by value, so it
        // borrows nothing from the registry and the actor stays responsive.
        let seat = first_mover_seat(self.opening, self.games_started);
        let record_replays = self.record_replays;
        let roster = participants.to_vec();
        let (started_tx, mut started_rx) = oneshot::channel();
        let _task = tokio::task::spawn_blocking(move || {
            let result = RoundRunner::start_owned_with_clock(
                selected,
                &roster,
                seat,
                time_limit_ms,
                record_replays,
                Instant::now,
            );
            let _ = started_tx.send(result);
        });
        enum SlotWake {
            Command(Option<Envelope>),
            Started(Result<Box<RoundRunner>, StartFailed>),
        }
        let wake = tokio::select! {
            biased;
            envelope = inbox.recv() => SlotWake::Command(envelope),
            started = &mut started_rx => SlotWake::Started(started.unwrap_or(Err(StartFailed {
                attempts: 0,
                fatal: false,
            }))),
        };
        match wake {
            SlotWake::Command(None) => {
                // Dropping the join handle detaches the task; its send on the
                // dropped receiver is ignored.
                self.cancel_match();
                None
            }
            SlotWake::Command(Some(envelope)) => {
                // The command arrived while `initialize` ran (T-PRS-9): apply it,
                // then keep waiting for the slot start.
                let result = self.handle_starting_command(envelope.command);
                let _ = envelope.reply.send(result);
                self.wait_for_slot_start(inbox, started_rx, round_index, participants, removed)
                    .await
            }
            SlotWake::Started(started) => {
                self.complete_slot_start(started, round_index, participants, removed)
            }
        }
    }

    /// Awaits one spawned slot start while continuing to apply commands
    /// (T-PRS-9). Only reached after a command already arrived mid-initialize.
    async fn wait_for_slot_start(
        &mut self,
        inbox: &mut mpsc::Receiver<Envelope>,
        mut started_rx: oneshot::Receiver<Result<Box<RoundRunner>, StartFailed>>,
        round_index: u32,
        participants: &[PlayerId],
        removed: Vec<PlayerId>,
    ) -> Option<Box<RoundRunner>> {
        loop {
            tokio::select! {
                biased;
                envelope = inbox.recv() => match envelope {
                    None => {
                        self.cancel_match();
                        return None;
                    }
                    Some(envelope) => {
                        let result = self.handle_starting_command(envelope.command);
                        let _ = envelope.reply.send(result);
                        if self.overloaded {
                            return None;
                        }
                    }
                },
                started = &mut started_rx => {
                    let outcome = started.unwrap_or(Err(StartFailed {
                        attempts: 0,
                        fatal: false,
                    }));
                    return self
                        .complete_slot_start(outcome, round_index, participants, removed);
                }
            }
        }
    }

    /// Turns a finished slot start into a runner or a skipped slot (RND-6,
    /// FLR-3, FLR-4).
    fn complete_slot_start(
        &mut self,
        started: Result<Box<RoundRunner>, StartFailed>,
        round_index: u32,
        participants: &[PlayerId],
        removed: Vec<PlayerId>,
    ) -> Option<Box<RoundRunner>> {
        match started {
            Err(failed) => {
                self.skip_slot(
                    round_index,
                    participants.to_vec(),
                    failed.status(),
                    failed.attempts,
                    removed,
                );
                None
            }
            Ok(mut runner) => {
                if self.advance(PhaseTrigger::AttemptSucceeded).is_err() {
                    self.cancel_match();
                    return None;
                }
                // PLAN "Round start" step 4: the phase is `InRound` now, so this
                // is where `RoundStarting` is announced (ARC-3).
                if matches!(
                    self.events.try_send(EngineEvent::RoundStarting {
                        round_index,
                        game: runner.game_id().clone(),
                        participants: runner.participants().to_vec(),
                        time_limit_secs: self.config.round_time_limit_secs,
                    }),
                    Err(mpsc::error::TrySendError::Full(_))
                ) {
                    self.overloaded = true;
                    return None;
                }
                // RND-13, T-PRS-9: the presence snapshot preceded `initialize`,
                // so anyone whose flag went false during it is seeded here.
                let disconnected = self
                    .states
                    .iter()
                    .filter(|state| !state.connected)
                    .map(|state| state.player.id)
                    .collect::<Vec<_>>();
                // PRS-12 already announced `Away` for a presence change during
                // `Starting`; the grace start must not repeat it (RND-13).
                let events = runner
                    .seed_disconnected(&disconnected)
                    .into_iter()
                    .filter(|event| {
                        !matches!(
                            event,
                            RoundEvent::PlayerStatus {
                                player,
                                status: PlayerStatus::Away,
                            } if self.slot_away_announced.contains(player)
                        )
                    })
                    .collect::<Vec<_>>();
                self.publish_round_events(round_index, events);
                if self.overloaded {
                    return None;
                }
                Some(runner)
            }
        }
    }

    /// Applies one command that arrived while a slot was starting (T-PRS-9).
    ///
    /// Only presence may change: lobby commands are out of phase and inputs
    /// cannot be accepted before `InRound` (LOB-4, section 8). A presence change
    /// is announced here (PRS-12); the grace itself starts at `at_ms = 0` when
    /// the runner exists (RND-13).
    fn handle_starting_command(&mut self, command: Command) -> Result<(), EngineError> {
        match command {
            Command::Connection { player, connected } => {
                self.set_connected(player, connected);
                self.announce_starting_presence(player, connected);
                Ok(())
            }
            Command::Leave { player } => {
                self.set_connected(player, false);
                self.announce_starting_presence(player, false);
                Ok(())
            }
            Command::Input { .. } => Err(EngineError::Input(InputError::NotInRound)),
            _ => Err(EngineError::Lobby(LobbyError::WrongState)),
        }
    }

    /// Emits the `PlayerStatus` for a presence change during `Starting`
    /// (PRS-12) and remembers an `Away` so the grace seeding does not repeat it.
    fn announce_starting_presence(&mut self, player: PlayerId, connected: bool) {
        let known = self.states.iter().any(|state| state.player.id == player);
        if !known {
            return;
        }
        if connected {
            self.publish(EngineEvent::PlayerStatus {
                player,
                status: PlayerStatus::Present,
            });
        } else {
            if !self.slot_away_announced.contains(&player) {
                self.slot_away_announced.push(player);
            }
            self.publish(EngineEvent::PlayerStatus {
                player,
                status: PlayerStatus::Away,
            });
        }
    }

    fn skip_slot(
        &mut self,
        round_index: u32,
        participants: Vec<PlayerId>,
        status: RoundStatus,
        attempts: u8,
        removed: Vec<PlayerId>,
    ) -> bool {
        if self.advance(PhaseTrigger::SlotSkipped).is_err() {
            self.cancel_match();
            return false;
        }
        let record = RoundRecord {
            round_index,
            game: None,
            participants,
            attempts,
            status,
            placements: Vec::new(),
            round_disqualified: Vec::new(),
            removed_this_round: removed,
            replay: None,
        };
        self.finish_slot(record)
    }

    fn finish_slot(&mut self, record: RoundRecord) -> bool {
        self.phase = Phase::RoundFinished;
        self.publish(EngineEvent::RoundFinished(record.clone()));
        self.rounds.push(record);
        if self.rounds.len() >= self.config.rounds as usize {
            if self.advance(PhaseTrigger::LastSlotProcessed).is_err() {
                self.cancel_match();
                return false;
            }
            self.publish_report();
            false
        } else {
            true
        }
    }

    fn publish_report(&mut self) {
        self.publish(EngineEvent::MatchFinished(MatchReport {
            match_id: self.match_id,
            lobby_id: self.lobby_id,
            config: self.config.clone(),
            rounds: self.rounds.clone(),
            final_ranking: final_ranking(&self.states),
            ended_early: self.ended_early,
        }));
    }

    /// Enqueues an event without ever blocking (ARC-8, ARC-10).
    fn publish(&mut self, event: EngineEvent) {
        match self.events.try_send(event) {
            Ok(()) => {}
            // ARC-10: a full channel means the server fell behind.
            Err(mpsc::error::TrySendError::Full(_)) => self.overloaded = true,
            // No subscriber: drop the event and keep serving.
            Err(mpsc::error::TrySendError::Closed(_)) => {}
        }
    }

    /// ARC-10: mark the terminal phase and end, dropping the event channel.
    fn finish_overloaded(&mut self) {
        tracing::warn!(host = ?self.host, "engine event channel full; cancelling (ARC-10)");
        self.phase = Phase::Cancelled;
        // ARC-10: overload is the one transition section 12.1 does not model as
        // a command, so the terminal phase is announced directly. The actor
        // ends next, so no later command can observe an inconsistent lobby.
        self.publish(EngineEvent::PhaseChanged(Phase::Cancelled));
    }
}

/// Why the in-round wait returned (TMR-2).
enum Wake {
    /// A command arrived, or the inbox closed (`None`).
    Input(Option<Envelope>),
    /// The round's hard deadline (TMR-1, TMR-4).
    Deadline,
    /// A reconnect grace expired (PRS-6).
    Grace,
    /// The game's `next_wakeup_ms` was reached (GAM-6).
    Game,
}
