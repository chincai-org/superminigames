//! T2.2: the lobby state machine (LOB-1..LOB-16, T-LOB-1..T-LOB-12).
//!
//! Every method is synchronous and runtime-free; time is passed in as
//! `tokio::time::Instant` values. T-LOB-10 uses tokio's paused clock, and the
//! rest run on the plain test harness.

#[path = "support/mod.rs"]
#[allow(unused_imports)] // the harness re-exports TestRegistry for T2.5/T2.6 too.
mod support;

use std::time::Duration;

use superminigames_core::config::{
    GameFilterMode, MAX_PLAYERS, MIN_PLAYERS, MatchConfig, SettingsUpdate,
};
use superminigames_core::error::ConfigError;
use superminigames_core::game::Game;
use superminigames_core::ids::{GameId, JoinCode, LobbyId, Player, Visibility};
use superminigames_engine::error::LobbyError;
use superminigames_engine::lobby::{LOBBY_IDLE_TIMEOUT, Lobby};
use superminigames_engine::registry::GameRegistry;
use superminigames_logic::phase::Phase;
use tokio::time::Instant;

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

/// A deterministic lobby id (the deterministic-id trick from `support`).
fn lobby_id() -> LobbyId {
    serde_json::from_str(&format!("\"{:032x}\"", 7)).expect("a 32-hex uuid literal")
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

fn join_code(code: &str) -> JoinCode {
    JoinCode::new(code).expect("a well-formed test code")
}

fn new_lobby(visibility: Visibility) -> Lobby {
    Lobby::new(
        lobby_id(),
        lobby_player(1),
        config(),
        visibility,
        registry(),
        Instant::now(),
    )
}

fn private_lobby(code: &str) -> Lobby {
    new_lobby(Visibility::Private(join_code(code)))
}

/// A public lobby with the host and one guest seated.
fn two_player_lobby() -> Lobby {
    let mut lobby = new_lobby(Visibility::Public);
    lobby
        .join(lobby_player(2), None, Instant::now())
        .expect("the second seat is free");
    lobby
}

#[test]
fn t_lob_1_settings_lock_follows_readiness() {
    // REQ: LOB-6
    let now = Instant::now();
    let host = lobby_player(1);
    let guest = lobby_player(2);
    let mut lobby = two_player_lobby();
    let update = || SettingsUpdate {
        rounds: Some(5),
        ..SettingsUpdate::default()
    };

    // Nobody ready: the host may update.
    assert_eq!(lobby.update_settings(host.id, update(), now), Ok(()));
    assert_eq!(lobby.snapshot().config.rounds, 5);

    // A seated player is Ready: settings lock.
    assert_eq!(lobby.set_ready(guest.id, true, now), Ok(()));
    assert_eq!(
        lobby.update_settings(host.id, update(), now),
        Err(LobbyError::SettingsLocked)
    );
    assert!(lobby.settings_locked());

    // All unready: settings unlock again.
    assert_eq!(lobby.set_ready(guest.id, false, now), Ok(()));
    assert_eq!(lobby.update_settings(host.id, update(), now), Ok(()));
    assert!(!lobby.settings_locked());

    // The host being Ready locks settings too (LOB-6 is any seated player).
    assert_eq!(lobby.set_ready(host.id, true, now), Ok(()));
    assert_eq!(
        lobby.update_settings(host.id, update(), now),
        Err(LobbyError::SettingsLocked)
    );
}

#[test]
fn t_lob_2_private_lobby_requires_the_exact_code() {
    // REQ: LOB-3
    let now = Instant::now();
    let mut lobby = private_lobby("ABC123");
    assert!(lobby.snapshot().private, "PRO-15: the client sees the flag");
    assert_eq!(
        lobby.join(lobby_player(2), None, now),
        Err(LobbyError::InvalidJoinCode)
    );
    assert_eq!(
        lobby.join(lobby_player(2), Some(join_code("ABC124")), now),
        Err(LobbyError::InvalidJoinCode)
    );
    // A lowercase code is never well-formed, so it can never equal a stored
    // code: `JoinCode::new` rejects it (LOB-3: the match is case-sensitive).
    assert_eq!(JoinCode::new("abc123"), Err(ConfigError::InvalidJoinCode));
    assert_eq!(
        lobby.join(lobby_player(2), Some(join_code("ABC123")), now),
        Ok(())
    );
    assert_eq!(lobby.snapshot().seats.len(), MAX_PLAYERS);
}

#[test]
fn t_lob_3_full_lobby_rejects_a_third_player() {
    // REQ: LOB-2
    let now = Instant::now();
    let mut lobby = two_player_lobby();
    assert_eq!(lobby.snapshot().seats.len(), MAX_PLAYERS);
    assert_eq!(
        lobby.join(lobby_player(3), None, now),
        Err(LobbyError::LobbyFull)
    );
    assert_eq!(lobby.snapshot().seats.len(), MAX_PLAYERS);
}

#[test]
fn t_lob_4_start_match_requires_every_seat_ready() {
    // REQ: LOB-9, LOB-1
    let now = Instant::now();
    let host = lobby_player(1);
    let guest = lobby_player(2);
    let mut lobby = two_player_lobby();
    assert_eq!(lobby.snapshot().seats.len(), MIN_PLAYERS);

    // The host is a player and must press Ready like everyone else (LOB-1).
    assert_eq!(
        lobby.start_match(host.id, 42, now),
        Err(LobbyError::NotAllReady)
    );
    lobby.set_ready(host.id, true, now).expect("host readies");
    assert_eq!(
        lobby.start_match(host.id, 42, now),
        Err(LobbyError::NotAllReady)
    );
    assert_eq!(lobby.phase(), Phase::Lobby);

    // Both ready: accepted and the phase is Starting.
    lobby.set_ready(guest.id, true, now).expect("guest readies");
    assert_eq!(lobby.start_match(host.id, 42, now), Ok(()));
    assert_eq!(lobby.phase(), Phase::Starting);
}

#[test]
fn t_lob_5_start_match_requires_two_players() {
    // REQ: LOB-9
    let now = Instant::now();
    let host = lobby_player(1);
    let mut lobby = new_lobby(Visibility::Public);
    assert_eq!(lobby.snapshot().seats.len(), MIN_PLAYERS - 1);
    lobby.set_ready(host.id, true, now).expect("host readies");
    assert_eq!(
        lobby.start_match(host.id, 1, now),
        Err(LobbyError::NotEnoughPlayers)
    );
    assert_eq!(lobby.phase(), Phase::Lobby);
}

#[test]
fn t_lob_6_host_leave_cancels_and_further_commands_fail() {
    // REQ: LOB-11, LOB-10
    let now = Instant::now();
    let host = lobby_player(1);
    let guest = lobby_player(2);
    let mut lobby = two_player_lobby();
    assert_eq!(lobby.leave(host.id, now), Ok(()));
    assert_eq!(lobby.phase(), Phase::Cancelled, "the host left in Lobby");

    // Cancelled is terminal: the lobby commands here are WrongState (LOB-4).
    assert_eq!(
        lobby.set_ready(guest.id, true, now),
        Err(LobbyError::WrongState)
    );
    assert_eq!(
        lobby.update_settings(host.id, SettingsUpdate::default(), now),
        Err(LobbyError::WrongState)
    );
    assert_eq!(lobby.cancel(host.id, now), Err(LobbyError::WrongState));
    assert_eq!(
        lobby.start_match(host.id, 1, now),
        Err(LobbyError::WrongState)
    );
    assert_eq!(
        lobby.join(lobby_player(3), None, now),
        Err(LobbyError::WrongState)
    );
}

/// A public lobby with both seats ready and the match already started.
fn started_lobby() -> Lobby {
    let now = Instant::now();
    let mut lobby = two_player_lobby();
    lobby.set_ready(player(1), true, now).expect("host readies");
    lobby
        .set_ready(player(2), true, now)
        .expect("guest readies");
    lobby.start_match(player(1), 9, now).expect("start");
    lobby
}

#[test]
fn t_lob_7_lobby_commands_are_wrong_state_during_a_match() {
    // REQ: LOB-4, LOB-10, LOB-14
    let now = Instant::now();
    let host = lobby_player(1);
    let guest = lobby_player(2);
    let mut lobby = started_lobby();
    assert_eq!(lobby.phase(), Phase::Starting);

    assert_eq!(
        lobby.join(lobby_player(3), None, now),
        Err(LobbyError::WrongState),
        "no new players after StartMatch (LOB-14)"
    );
    assert_eq!(
        lobby.set_ready(guest.id, false, now),
        Err(LobbyError::WrongState)
    );
    assert_eq!(
        lobby.update_settings(host.id, SettingsUpdate::default(), now),
        Err(LobbyError::WrongState)
    );
    assert_eq!(lobby.cancel(host.id, now), Err(LobbyError::WrongState));
    assert_eq!(
        lobby.start_match(host.id, 9, now),
        Err(LobbyError::WrongState)
    );

    // LOB-4 exceptions: Leave and Connection are presence, not WrongState.
    assert_eq!(lobby.leave(guest.id, now), Ok(()));
    assert_eq!(lobby.connection(guest.id, false, now), Ok(()));
    assert_eq!(
        lobby.phase(),
        Phase::Starting,
        "a presence event does not change the phase"
    );
}

#[test]
fn t_lob_8_filter_validation_maps_to_config_errors() {
    // REQ: LOB-12, LOB-8
    let now = Instant::now();
    let host = lobby_player(1);
    let mut lobby = new_lobby(Visibility::Public);
    let update = |filter: GameFilterMode| SettingsUpdate {
        filter: Some(filter),
        ..SettingsUpdate::default()
    };

    // An allow list with an id that is not registered.
    assert_eq!(
        lobby.update_settings(
            host.id,
            update(GameFilterMode::AllowList(vec![GameId("C".to_string())])),
            now
        ),
        Err(LobbyError::Config(ConfigError::UnknownGame(GameId(
            "C".to_string()
        ))))
    );
    // An allow list with no games.
    assert_eq!(
        lobby.update_settings(host.id, update(GameFilterMode::AllowList(Vec::new())), now),
        Err(LobbyError::Config(ConfigError::EmptyAllowList))
    );
    // A list that repeats a game.
    assert_eq!(
        lobby.update_settings(
            host.id,
            update(GameFilterMode::AllowList(vec![
                GameId("A".to_string()),
                GameId("A".to_string()),
            ])),
            now
        ),
        Err(LobbyError::Config(ConfigError::DuplicateGameInList))
    );
    // A ban list that covers every registered game.
    assert_eq!(
        lobby.update_settings(
            host.id,
            update(GameFilterMode::BanList(vec![
                GameId("A".to_string()),
                GameId("B".to_string()),
            ])),
            now
        ),
        Err(LobbyError::Config(ConfigError::NoEligibleGames))
    );
    // A valid allow list is accepted and stored.
    assert_eq!(
        lobby.update_settings(
            host.id,
            update(GameFilterMode::AllowList(vec![GameId("A".to_string())])),
            now
        ),
        Ok(())
    );
    assert_eq!(
        lobby.snapshot().config.filter,
        GameFilterMode::AllowList(vec![GameId("A".to_string())])
    );
}

#[test]
fn t_lob_9_disconnect_frees_a_seated_non_host_seat() {
    // REQ: LOB-15
    let now = Instant::now();
    let guest = lobby_player(2);
    let mut lobby = two_player_lobby();
    lobby.set_ready(guest.id, true, now).expect("guest readies");
    assert!(lobby.settings_locked());

    // A disconnect for a seated player is exactly a Leave (LOB-15).
    assert_eq!(lobby.connection(guest.id, false, now), Ok(()));
    let snapshot = lobby.snapshot();
    assert_eq!(snapshot.seats.len(), MIN_PLAYERS - 1);
    assert!(
        snapshot
            .seats
            .iter()
            .all(|(seated, _)| seated.id != guest.id)
    );
    assert!(!lobby.settings_locked(), "their Ready flag is discarded");
    assert_eq!(lobby.phase(), Phase::Lobby);

    // The freed seat is joinable again.
    assert_eq!(lobby.join(lobby_player(3), None, now), Ok(()));
    assert_eq!(lobby.snapshot().seats.len(), MAX_PLAYERS);
}

#[tokio::test(start_paused = true)]
async fn t_lob_10_idle_lobby_expires_and_a_recent_one_does_not() {
    // REQ: LOB-16
    let mut lobby = new_lobby(Visibility::Public);

    // Inside the timeout: not idle, and expire_if_idle changes nothing.
    tokio::time::advance(LOBBY_IDLE_TIMEOUT - Duration::from_secs(1)).await;
    assert!(!lobby.is_idle(Instant::now()));
    assert!(!lobby.expire_if_idle(Instant::now()));
    assert_eq!(lobby.phase(), Phase::Lobby);

    // Any command resets the idle clock.
    assert_eq!(lobby.join(lobby_player(2), None, Instant::now()), Ok(()));
    tokio::time::advance(LOBBY_IDLE_TIMEOUT - Duration::from_secs(1)).await;
    assert!(!lobby.is_idle(Instant::now()), "the join reset the clock");

    // Past the timeout with no command: idle, and expire cancels the lobby.
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(lobby.is_idle(Instant::now()), "no command for the timeout");
    assert!(lobby.expire_if_idle(Instant::now()));
    assert_eq!(lobby.phase(), Phase::Cancelled);

    // A cancelled lobby is not cancelled again.
    assert!(!lobby.expire_if_idle(Instant::now()));
}

#[test]
fn t_lob_10_cancel_is_host_only_while_in_lobby() {
    // REQ: LOB-10
    let now = Instant::now();
    let host = lobby_player(1);
    let guest = lobby_player(2);
    let mut lobby = two_player_lobby();
    assert_eq!(lobby.cancel(guest.id, now), Err(LobbyError::NotHost));
    assert_eq!(lobby.phase(), Phase::Lobby);
    assert_eq!(lobby.cancel(host.id, now), Ok(()));
    assert_eq!(lobby.phase(), Phase::Cancelled);
}

#[test]
fn t_lob_11_non_host_leave_frees_the_seat_and_drops_ready() {
    // REQ: LOB-11
    let now = Instant::now();
    let guest = lobby_player(2);
    let mut lobby = two_player_lobby();
    lobby.set_ready(guest.id, true, now).expect("guest readies");
    assert_eq!(lobby.leave(guest.id, now), Ok(()));
    assert_eq!(lobby.phase(), Phase::Lobby, "only the host leaving cancels");
    let snapshot = lobby.snapshot();
    assert_eq!(snapshot.seats.len(), MIN_PLAYERS - 1);
    assert!(
        snapshot
            .seats
            .iter()
            .all(|(seated, _)| seated.id != guest.id)
    );
    assert!(!snapshot.settings_locked, "their Ready flag is discarded");
}

#[test]
fn t_lob_11_start_match_stores_the_seed_and_update_has_no_seed() {
    // REQ: LOB-6, LOB-9 (T-LOB-11)
    let now = Instant::now();
    let host = lobby_player(1);
    let guest = lobby_player(2);
    let mut lobby = two_player_lobby();
    assert_eq!(lobby.snapshot().config.seed, 0);
    lobby.set_ready(host.id, true, now).expect("host readies");
    lobby.set_ready(guest.id, true, now).expect("guest readies");
    assert_eq!(lobby.start_match(host.id, 0xDEAD_BEEF, now), Ok(()));
    assert_eq!(lobby.snapshot().config.seed, 0xDEAD_BEEF);
    // `SettingsUpdate` has no seed field: the seed is not a setting, so it
    // can only enter through `StartMatch` (LOB-6, LOB-9).
}

#[test]
fn t_lob_12_arrival_order_decides_the_winner() {
    // REQ: LOB-13
    let now = Instant::now();
    let host = lobby_player(1);
    let guest = lobby_player(2);
    let update = SettingsUpdate {
        rounds: Some(7),
        ..SettingsUpdate::default()
    };

    // Ready first, then UpdateSettings: the update is locked, so it loses.
    let mut ready_first = two_player_lobby();
    ready_first
        .set_ready(guest.id, true, now)
        .expect("guest readies");
    assert_eq!(
        ready_first.update_settings(host.id, update.clone(), now),
        Err(LobbyError::SettingsLocked)
    );

    // UpdateSettings first, then Ready: the update wins.
    let mut update_first = two_player_lobby();
    assert_eq!(update_first.update_settings(host.id, update, now), Ok(()));
    assert_eq!(update_first.set_ready(guest.id, true, now), Ok(()));
    assert_eq!(update_first.snapshot().config.rounds, 7);
    assert!(update_first.settings_locked());
}

#[test]
fn t_lob_12_start_match_revalidates_the_config() {
    // REQ: LOB-12
    let now = Instant::now();
    let host = lobby_player(1);
    let guest = lobby_player(2);
    // The create path is not validated here, so `StartMatch` must catch it.
    let mut lobby = Lobby::new(
        lobby_id(),
        lobby_player(1),
        MatchConfig {
            rounds: 0,
            ..config()
        },
        Visibility::Public,
        registry(),
        now,
    );
    lobby.join(lobby_player(2), None, now).expect("second seat");
    lobby.set_ready(host.id, true, now).expect("host readies");
    lobby.set_ready(guest.id, true, now).expect("guest readies");
    assert_eq!(
        lobby.start_match(host.id, 1, now),
        Err(LobbyError::Config(ConfigError::RoundsOutOfRange))
    );
    assert_eq!(lobby.phase(), Phase::Lobby, "an invalid config blocks it");
}

#[test]
fn t_lob_12_range_updates_are_validated() {
    // REQ: LOB-12
    let now = Instant::now();
    let host = lobby_player(1);
    let mut lobby = new_lobby(Visibility::Public);
    let cases = [
        (
            SettingsUpdate {
                rounds: Some(0),
                ..SettingsUpdate::default()
            },
            ConfigError::RoundsOutOfRange,
        ),
        (
            SettingsUpdate {
                rounds: Some(101),
                ..SettingsUpdate::default()
            },
            ConfigError::RoundsOutOfRange,
        ),
        (
            SettingsUpdate {
                round_time_limit_secs: Some(0),
                ..SettingsUpdate::default()
            },
            ConfigError::TimeLimitOutOfRange,
        ),
        (
            SettingsUpdate {
                round_time_limit_secs: Some(3601),
                ..SettingsUpdate::default()
            },
            ConfigError::TimeLimitOutOfRange,
        ),
        (
            SettingsUpdate {
                max_absent_round_starts: Some(0),
                ..SettingsUpdate::default()
            },
            ConfigError::AbsentLimitOutOfRange,
        ),
        (
            SettingsUpdate {
                max_absent_round_starts: Some(11),
                ..SettingsUpdate::default()
            },
            ConfigError::AbsentLimitOutOfRange,
        ),
    ];
    for (update, expected) in cases {
        assert_eq!(
            lobby.update_settings(host.id, update, now),
            Err(LobbyError::Config(expected))
        );
    }
    // A rejected update leaves the config unchanged (LOB-12).
    assert_eq!(lobby.snapshot().config.rounds, 3);
    assert_eq!(lobby.snapshot().config.round_time_limit_secs, 60);
    assert_eq!(lobby.snapshot().config.max_absent_round_starts, 2);
}

#[test]
fn t_lob_5_ready_can_be_toggled_repeatedly() {
    // REQ: LOB-5
    let now = Instant::now();
    let guest = lobby_player(2);
    let mut lobby = two_player_lobby();
    for _ in 0..3 {
        assert_eq!(lobby.set_ready(guest.id, true, now), Ok(()));
        assert!(lobby.settings_locked());
        assert_eq!(lobby.set_ready(guest.id, false, now), Ok(()));
        assert!(!lobby.settings_locked());
    }
}

#[test]
fn t_lob_6_update_settings_can_change_visibility() {
    // REQ: LOB-6
    let now = Instant::now();
    let host = lobby_player(1);
    let mut lobby = new_lobby(Visibility::Public);
    assert!(!lobby.snapshot().private);
    assert_eq!(
        lobby.update_settings(
            host.id,
            SettingsUpdate {
                visibility: Some(Visibility::Private(join_code("ZZZ999"))),
                ..SettingsUpdate::default()
            },
            now
        ),
        Ok(())
    );
    assert!(lobby.snapshot().private);
}

#[test]
fn t_lob_5_unseated_commands_are_unknown_players() {
    // REQ: LOB-5, LOB-9
    let now = Instant::now();
    let mut lobby = new_lobby(Visibility::Public);
    assert_eq!(
        lobby.set_ready(player(42), true, now),
        Err(LobbyError::UnknownPlayer)
    );
    assert_eq!(lobby.leave(player(42), now), Err(LobbyError::UnknownPlayer));
    assert_eq!(lobby.phase(), Phase::Lobby);
}

#[test]
fn t_lob_9_start_match_is_host_only() {
    // REQ: LOB-9
    let now = Instant::now();
    let host = lobby_player(1);
    let guest = lobby_player(2);
    let mut lobby = two_player_lobby();
    lobby.set_ready(host.id, true, now).expect("host readies");
    lobby.set_ready(guest.id, true, now).expect("guest readies");
    assert_eq!(
        lobby.start_match(guest.id, 1, now),
        Err(LobbyError::NotHost)
    );
    assert_eq!(lobby.phase(), Phase::Lobby);
}

#[test]
fn t_lob_3_full_lobby_checks_the_join_code_first() {
    // REQ: LOB-2, LOB-3
    // User ruling: LOB-2 and LOB-3 are both unqualified MUSTs and fix no
    // precedence, so the join code is validated first. A full private lobby
    // therefore reports a bad code before it reports fullness.
    let now = Instant::now();
    let mut lobby = private_lobby("ABC123");
    lobby
        .join(lobby_player(2), Some(join_code("ABC123")), now)
        .expect("the guest joins with the right code");
    assert_eq!(lobby.snapshot().seats.len(), MAX_PLAYERS);
    // Full private + missing code: the code check wins.
    assert_eq!(
        lobby.join(lobby_player(3), None, now),
        Err(LobbyError::InvalidJoinCode)
    );
    // Full private + wrong code: the code check wins.
    assert_eq!(
        lobby.join(lobby_player(3), Some(join_code("ABC124")), now),
        Err(LobbyError::InvalidJoinCode)
    );
    // Full private + correct code: now the fullness check applies.
    assert_eq!(
        lobby.join(lobby_player(3), Some(join_code("ABC123")), now),
        Err(LobbyError::LobbyFull)
    );
    // A full public lobby has no code to check, so it is still LobbyFull.
    let mut public = two_player_lobby();
    assert_eq!(
        public.join(lobby_player(3), None, now),
        Err(LobbyError::LobbyFull)
    );
    assert_eq!(lobby.snapshot().seats.len(), MAX_PLAYERS);
}

#[test]
fn t_lob_15_connection_for_an_unseated_player_is_ignored() {
    // REQ: LOB-15
    let now = Instant::now();
    let host = lobby_player(1);
    let guest = lobby_player(2);
    let mut lobby = two_player_lobby();
    assert_eq!(lobby.connection(guest.id, true, now), Ok(()));
    assert_eq!(lobby.connection(player(99), false, now), Ok(()));
    assert_eq!(lobby.connection(player(99), true, now), Ok(()));
    assert_eq!(lobby.snapshot().seats.len(), MAX_PLAYERS);
    assert_eq!(lobby.phase(), Phase::Lobby);
    // A seated host disconnect is exactly a Leave, so it cancels.
    assert_eq!(lobby.connection(host.id, false, now), Ok(()));
    assert_eq!(lobby.phase(), Phase::Cancelled);
}
