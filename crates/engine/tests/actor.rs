//! T2.6 stage A: the engine actor (ARC-2, ARC-3, ARC-4, ARC-5, ARC-7, ARC-8,
//! ARC-9, ARC-10, LOB-4, LOB-16).
//!
//! Every test runs on tokio's paused clock, so the LOB-16 idle deadline is
//! deterministic. Commands are sent through the handle and events are read
//! straight after the reply, which the actor always sends last.

#[path = "support/mod.rs"]
mod support;

use std::time::Duration;

use superminigames_core::config::{GameFilterMode, MatchConfig, SettingsUpdate};
use superminigames_core::error::ConfigError;
use superminigames_core::game::{Game, GameAction};
use superminigames_core::ids::{LobbyId, MatchId, Player, Visibility};
use superminigames_engine::actor::{
    Command, EngineEvent, EngineInit, EngineOptions, INBOX_CAPACITY, spawn_engine,
    spawn_engine_with_event_capacity,
};
use superminigames_engine::error::{EngineError, LobbyError};
use superminigames_engine::lobby::LOBBY_IDLE_TIMEOUT;
use superminigames_engine::registry::GameRegistry;
use superminigames_logic::phase::Phase;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::Dispatch;
use tracing::dispatcher::set_default;
use tracing::subscriber::NoSubscriber;

use support::{FakeGame, player};

fn game_a() -> Box<dyn Game> {
    Box::new(FakeGame::builder("A", "Game A").build())
}

fn game_b() -> Box<dyn Game> {
    Box::new(FakeGame::builder("B", "Game B").build())
}

/// A registry with exactly the games `A` and `B` (TYP-2).
fn registry() -> GameRegistry {
    let mut registry = GameRegistry::new();
    registry.register(game_a).expect("A is unique");
    registry.register(game_b).expect("B is unique");
    registry
}

/// A deterministic id (the deterministic-id trick from `support`).
fn lobby_id() -> LobbyId {
    serde_json::from_str(&format!("\"{:032x}\"", 7)).expect("a 32-hex uuid literal")
}

fn match_id() -> MatchId {
    serde_json::from_str(&format!("\"{:032x}\"", 8)).expect("a 32-hex uuid literal")
}

/// A seated player with a stable id and name.
fn lobby_player(n: u64) -> Player {
    Player {
        id: player(n),
        name: format!("P{n}"),
    }
}

/// A lobby-ready config whose filter keeps `A` and `B` (LOB-12).
fn config() -> MatchConfig {
    MatchConfig {
        rounds: 3,
        round_time_limit_secs: 60,
        seed: 0,
        max_absent_round_starts: 2,
        filter: GameFilterMode::BanList(Vec::new()),
    }
}

fn init(config: MatchConfig) -> EngineInit {
    EngineInit {
        lobby_id: lobby_id(),
        match_id: match_id(),
        host: lobby_player(1),
        config,
        visibility: Visibility::Public,
        registry: registry(),
        options: EngineOptions {
            record_replays: false,
            inbox_capacity: INBOX_CAPACITY,
        },
    }
}

/// A running actor and its single event stream.
fn started() -> (
    superminigames_engine::actor::EngineHandle,
    mpsc::Receiver<EngineEvent>,
) {
    let mut handle = spawn_engine(init(config())).expect("a valid config spawns");
    let events = handle.subscribe();
    (handle, events)
}

/// Every event already published, without waiting (the actor replies after
/// publishing, so they are all present once a `send` returns).
fn drain(events: &mut mpsc::Receiver<EngineEvent>) -> Vec<EngineEvent> {
    let mut collected = Vec::new();
    while let Ok(event) = events.try_recv() {
        collected.push(event);
    }
    collected
}

fn join_guest(name: u64) -> Command {
    Command::Join {
        player: lobby_player(name),
        code: None,
    }
}

fn input() -> Command {
    Command::Input {
        player: player(2),
        action: GameAction(Vec::new()),
        lag_credit_ms: 0,
        received_at: Instant::now(),
    }
}

#[tokio::test(start_paused = true)]
async fn t_arc_2_lobby_commands_change_the_lobby() {
    // REQ: ARC-2, ARC-3, LOB-5
    let (handle, mut events) = started();
    let guest = lobby_player(2);
    let host = player(1);

    handle
        .send(join_guest(2))
        .await
        .expect("the second seat is free");
    let after_join = drain(&mut events);
    let EngineEvent::LobbyChanged(snapshot) = &after_join[0] else {
        panic!("expected LobbyChanged, got {after_join:?}");
    };
    assert_eq!(snapshot.seats.len(), 2, "the guest is seated");

    handle
        .send(Command::SetReady {
            player: guest.id,
            ready: true,
        })
        .await
        .expect("a seated player may ready");
    let after_ready = drain(&mut events);
    let EngineEvent::LobbyChanged(snapshot) = &after_ready[0] else {
        panic!("expected LobbyChanged, got {after_ready:?}");
    };
    assert!(snapshot.settings_locked, "Ready locks the settings (LOB-6)");

    handle
        .send(Command::UpdateSettings {
            player: host,
            update: SettingsUpdate {
                rounds: Some(5),
                ..SettingsUpdate::default()
            },
        })
        .await
        .expect_err("settings are locked while a player is ready");
}

#[tokio::test(start_paused = true)]
async fn t_arc_3_events_report_snapshots_and_phase_changes() {
    // REQ: ARC-3
    let (handle, mut events) = started();
    let host = player(1);

    handle.send(join_guest(2)).await.expect("join");
    let after_join = drain(&mut events);
    assert!(
        matches!(after_join.as_slice(), [EngineEvent::LobbyChanged(_)]),
        "an accepted lobby command publishes LobbyChanged, got {after_join:?}"
    );

    // The host cancels: LobbyChanged, then the PhaseChanged(Cancelled) of LOB-10.
    handle
        .send(Command::Cancel { player: host })
        .await
        .expect("cancel");
    let after_cancel = drain(&mut events);
    assert!(
        matches!(
            after_cancel.as_slice(),
            [
                EngineEvent::LobbyChanged(_),
                EngineEvent::PhaseChanged(Phase::Cancelled)
            ]
        ),
        "cancel publishes the snapshot then the phase, got {after_cancel:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn t_arc_5_invalid_transition_is_a_typed_error() {
    // REQ: ARC-5, LOB-4, LOB-9
    let (handle, mut events) = started();
    let host = player(1);
    let guest = lobby_player(2);

    handle.send(join_guest(2)).await.expect("join");
    handle
        .send(Command::SetReady {
            player: host,
            ready: true,
        })
        .await
        .expect("host readies");
    handle
        .send(Command::SetReady {
            player: guest.id,
            ready: true,
        })
        .await
        .expect("guest readies");
    let _ = drain(&mut events);
    handle
        .send(Command::StartMatch {
            player: host,
            seed: 7,
        })
        .await
        .expect("start");
    let after_start = drain(&mut events);
    assert!(
        after_start.contains(&EngineEvent::PhaseChanged(Phase::Starting)),
        "StartMatch enters Starting before the slot loop, got {after_start:?}"
    );

    // T-PRS-9: the slot start is asynchronous, so the engine reaches `InRound`
    // only once `initialize` returns; `RoundStarting` signals that.
    while !matches!(events.recv().await, Some(EngineEvent::RoundStarting { .. })) {}

    // Lobby commands are now WrongState, a typed error and never a panic (LOB-4).
    assert_eq!(
        handle.send(Command::Cancel { player: host }).await,
        Err(EngineError::Lobby(LobbyError::WrongState))
    );
    assert_eq!(
        handle
            .send(Command::SetReady {
                player: guest.id,
                ready: false
            })
            .await,
        Err(EngineError::Lobby(LobbyError::WrongState))
    );
    // A presence event is still accepted (LOB-4 exception).
    assert_eq!(
        handle.send(Command::Leave { player: guest.id }).await,
        Ok(())
    );
    // The round is running, so a participant's input is accepted even though
    // that player is now away inside their grace (PRS-6, T-PRS-11).
    assert_eq!(handle.send(input()).await, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn t_arc_10_full_event_channel_stops_the_actor() {
    // REQ: ARC-8, ARC-10, T-ARC-1
    let mut handle = spawn_engine_with_event_capacity(init(config()), 1).expect("spawn");
    let mut events = handle.subscribe();
    let guest = lobby_player(2);

    // The first accepted command fills the single slot.
    handle.send(join_guest(2)).await.expect("join");

    // The next one cannot publish: the actor stops without blocking (ARC-10).
    let second = handle.send(Command::SetReady {
        player: guest.id,
        ready: true,
    });
    let reply = tokio::time::timeout(Duration::from_secs(1), second)
        .await
        .expect("the actor must answer, not block");
    assert_eq!(reply, Ok(()), "the accepted command still gets its reply");

    // One LobbyChanged was buffered; the PhaseChanged(Cancelled) cannot fit, so
    // the observable is the closed channel (ARC-10).
    assert!(matches!(
        events.recv().await,
        Some(EngineEvent::LobbyChanged(_))
    ));
    let closed = tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .expect("the channel closes without blocking");
    assert!(closed.is_none(), "the actor dropped the event channel");

    // Everything after is rejected as the terminal phase would (ARC-10, LOB-4).
    assert_eq!(
        handle
            .send(Command::SetReady {
                player: guest.id,
                ready: true
            })
            .await,
        Err(EngineError::Lobby(LobbyError::WrongState))
    );
}

/// The same lobby command sequence, as an event log (ARC-7).
async fn run_scenario() -> Vec<EngineEvent> {
    let (handle, mut events) = started();
    let guest = lobby_player(2);
    let mut log = Vec::new();

    handle.send(join_guest(2)).await.expect("join");
    log.extend(drain(&mut events));
    handle
        .send(Command::UpdateSettings {
            player: player(1),
            update: SettingsUpdate {
                rounds: Some(5),
                ..SettingsUpdate::default()
            },
        })
        .await
        .expect("update");
    log.extend(drain(&mut events));
    handle
        .send(Command::SetReady {
            player: guest.id,
            ready: true,
        })
        .await
        .expect("ready");
    log.extend(drain(&mut events));
    log
}

#[tokio::test(start_paused = true)]
async fn t_arc_7_tracing_does_not_change_the_events() {
    // REQ: ARC-7, T-ARC-4
    let dispatch = Dispatch::new(NoSubscriber::new());
    let guard = set_default(&dispatch);
    let with_subscriber = run_scenario().await;
    drop(guard);

    let without_subscriber = run_scenario().await;
    assert_eq!(with_subscriber, without_subscriber);
}

#[tokio::test(start_paused = true)]
async fn t_arc_9_dropping_the_handle_ends_the_actor() {
    // REQ: ARC-2, ARC-9
    let mut handle = spawn_engine(init(config())).expect("spawn");
    let mut events = handle.subscribe();
    drop(handle);

    let ended = tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .expect("the actor ends when its inbox closes");
    assert!(ended.is_none(), "the actor dropped its event channel");
}

#[tokio::test(start_paused = true)]
async fn t_lob_16_an_idle_lobby_is_cancelled_by_the_actor() {
    // REQ: LOB-16, ARC-4
    let (_handle, mut events) = started();
    tokio::time::advance(LOBBY_IDLE_TIMEOUT).await;

    let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .expect("the idle timer fires")
        .expect("an event");
    assert_eq!(event, EngineEvent::PhaseChanged(Phase::Cancelled));

    let ended = tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .expect("the actor ends");
    assert!(ended.is_none());
}

#[tokio::test(start_paused = true)]
async fn t_arc_2_an_invalid_config_is_rejected_at_spawn() {
    // REQ: LOB-12, ARC-9
    let invalid = MatchConfig {
        rounds: 0,
        ..config()
    };
    let error = spawn_engine(init(invalid)).err();
    assert_eq!(
        error,
        Some(LobbyError::Config(ConfigError::RoundsOutOfRange))
    );
}
