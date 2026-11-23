//! T1.7: the conformance suite (conformance.rs) run against the fake game.
//!
//! These tests exist so the suite itself is verified before any real game crate
//! uses it (GAM-11, CI-7). Each is named after the acceptance test it mirrors.

use super::conformance::{
    generic_script, shape_malformed_actions, t_gam_1_same_seed_same_run, t_gam_2_metadata_is_pure,
    t_gam_3_malformed_actions_are_rejected, t_gam_4_initialize_resets_and_validates,
    t_gam_5_finished_is_final,
};
use super::fake::{FakeGame, FakeGameConfig, PASS};
use crate::game::{Game, GameEvent, GameInput, ScoringMode, TimeModel};
use crate::ids::{GameId, PlayerId};
use crate::outcome::PlayerOutcome;
use uuid::Uuid;

fn player(n: u128) -> PlayerId {
    // REQ: T-GAM-4 — ids are supplied by the server, never generated (TYP-3).
    PlayerId(Uuid::from_u128(n))
}

fn players() -> Vec<PlayerId> {
    vec![player(1), player(2)]
}

/// A fake game that finishes after three `TimeAdvanced` ticks.
fn finishing() -> Box<dyn Game> {
    Box::new(FakeGame::new(FakeGameConfig {
        ticks_to_finish: Some(3),
        ..FakeGameConfig::default()
    }))
}

/// A `PlayerClocks` fake game, so T-GAM-5 also covers `next_wakeup_ms` (GAM-12).
fn player_clocks_finishing() -> Box<dyn Game> {
    Box::new(FakeGame::new(FakeGameConfig {
        time_model: TimeModel::PlayerClocks,
        ticks_to_finish: Some(2),
        ..FakeGameConfig::default()
    }))
}

#[test]
fn t_gam_1_same_seed_same_run_holds_for_the_fake_game() {
    // REQ: GAM-1, GAM-3
    let players = players();
    let script = generic_script(&players);
    t_gam_1_same_seed_same_run(finishing, &players, 7, &script);

    // A different seed must change the events, otherwise the fake game ignores
    // its seed and the suite above proves nothing about GAM-3.
    let mut low = finishing();
    let mut high = finishing();
    low.initialize(0, &players, 1_000_000).expect("initialize");
    high.initialize(9, &players, 1_000_000).expect("initialize");
    let event = |game: &mut Box<dyn Game>| {
        game.tick(
            0,
            GameInput::Player {
                player: players[0],
                action: FakeGame::action(PASS),
                lag_credit_ms: 0,
            },
        )
        .expect("tick")
    };
    assert_ne!(
        event(&mut low),
        event(&mut high),
        "GAM-1: the fake game must draw from its seed"
    );
}

#[test]
fn t_gam_2_metadata_is_pure_for_the_fake_game() {
    // REQ: GAM-5, TYP-2
    t_gam_2_metadata_is_pure(finishing, &players(), 0);
}

#[test]
fn t_gam_3_malformed_actions_are_rejected_by_the_fake_game() {
    // REQ: GAM-6
    t_gam_3_malformed_actions_are_rejected(finishing, &players(), 0, &shape_malformed_actions());
}

#[test]
fn t_gam_4_initialize_resets_and_validates_for_the_fake_game() {
    // REQ: GAM-4, GAM-5
    t_gam_4_initialize_resets_and_validates(finishing, &players(), 1, 2);
}

#[test]
fn t_gam_5_a_finished_fake_game_is_final() {
    // REQ: GAM-7, GAM-8
    t_gam_5_finished_is_final(finishing, &players(), 0);
}

#[test]
fn t_gam_5_a_player_clocks_fake_game_reports_every_player() {
    // REQ: GAM-7, GAM-12
    // A `PlayerClocks` game that assigns places at the end reports everybody.
    t_gam_5_finished_is_final(player_clocks_finishing, &players(), 0);
}

#[test]
fn t_gam_7_an_eliminated_player_keeps_a_placement() {
    // REQ: GAM-7, PRS-11
    // A three-player game where seat 1 is eliminated by the rules while play
    // continues: the game reports a placement for that seat, never NoResult.
    let players = vec![player(1), player(2), player(3)];
    let mut game = FakeGame::new(FakeGameConfig {
        supported_players: vec![3],
        eliminated: Some(1),
        eliminated_place: 3,
        ..FakeGameConfig::default()
    });
    game.initialize(0, &players, 1_000_000).expect("initialize");

    for step in 0..3u64 {
        game.tick(step * 1_000, GameInput::TimeAdvanced)
            .expect("tick");
        let outcomes = game.result().outcomes;
        let eliminated = outcomes
            .iter()
            .find(|(player, _)| *player == players[1])
            .expect("T-GAM-7: the eliminated player must keep an outcome");
        assert_eq!(eliminated.1, PlayerOutcome::Placed(3));
        assert!(
            !matches!(eliminated.1, PlayerOutcome::NoResult(_)),
            "T-GAM-7: in-game elimination must not be a NoResult"
        );
        assert!(!game.is_finished(), "T-GAM-7: play must continue");
    }
}

#[test]
fn t_gam_7_a_round_deadline_game_scores_every_participant() {
    // REQ: GAM-7
    // A `RoundDeadlineOnly` game that scores as much as possible: nobody is
    // disqualified at the deadline.
    let players = players();
    let mut game = FakeGame::new(FakeGameConfig {
        scoring_mode: ScoringMode::HigherIsBetter,
        score_at_deadline: true,
        ticks_to_finish: Some(1),
        ..FakeGameConfig::default()
    });
    game.initialize(0, &players, 1_000_000).expect("initialize");
    game.tick(1_000, GameInput::TimeAdvanced).expect("tick");
    assert!(game.is_finished(), "the deadline ends the game");
    let outcomes = game.result().outcomes;
    assert_eq!(outcomes.len(), players.len());
    for (player, outcome) in outcomes {
        assert!(
            matches!(outcome, PlayerOutcome::Scored(_)),
            "{player:?} must be scored, got {outcome:?}"
        );
    }
}

#[test]
fn t_gam_2_a_player_clocks_game_asks_for_its_next_wakeup() {
    // REQ: GAM-12
    let players = players();
    let mut game = FakeGame::new(FakeGameConfig {
        time_model: TimeModel::PlayerClocks,
        ticks_to_finish: Some(2),
        ..FakeGameConfig::default()
    });
    game.initialize(0, &players, 2_000).expect("initialize");
    assert_eq!(game.next_wakeup_ms(), Some(1_000));
    game.tick(1_000, GameInput::TimeAdvanced).expect("tick");
    assert_eq!(game.next_wakeup_ms(), None, "the clock ran out");
}

#[test]
fn t_gam_6_player_left_is_not_a_broadcast() {
    // REQ: GAM-6, GAM-9
    let players = players();
    let mut game = finishing();
    game.initialize(0, &players, 1_000_000).expect("initialize");
    let events = game
        .tick(0, GameInput::PlayerLeft(players[1]))
        .expect("PlayerLeft must not be Fatal");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::Broadcast(_))),
        "GAM-9: PlayerLeft is not something to broadcast: {events:?}"
    );
}

#[test]
fn t_gam_4_an_unsupported_player_count_is_init_failed() {
    // REQ: GAM-5
    let config = FakeGameConfig {
        supported_players: vec![3],
        ..FakeGameConfig::default()
    };
    let mut game = FakeGame::new(config);
    assert!(!game.supports_players(2));
    let result = game.initialize(0, &players(), 1_000_000);
    assert!(
        matches!(result, Err(crate::game::GameError::InitFailed(_))),
        "GAM-5: 2 players must give InitFailed"
    );
}

#[test]
fn t_gam_2_the_fake_game_is_identified_by_a_game_id() {
    // REQ: GAM-5
    let game = FakeGame::new(FakeGameConfig::default());
    assert_eq!(game.id(), GameId("fake".to_string()));
    assert_eq!(game.name(), "Fake Game");
    assert_eq!(game.scoring_mode(), ScoringMode::Placement);
}

#[test]
fn t_gam_9_a_left_player_is_ranked_by_the_fake_game() {
    // REQ: GAM-9, PRS-11
    let players = players();
    let mut game = FakeGame::new(FakeGameConfig {
        ticks_to_finish: Some(1),
        ..FakeGameConfig::default()
    });
    game.initialize(0, &players, 1_000_000).expect("initialize");
    game.tick(0, GameInput::PlayerLeft(players[1]))
        .expect("PlayerLeft must not be Fatal");
    game.tick(1_000, GameInput::TimeAdvanced).expect("tick");
    assert!(game.is_finished());
    let outcomes = game.result().outcomes;
    assert_eq!(outcomes.len(), players.len());
    assert!(
        outcomes
            .iter()
            .all(|(_, outcome)| !matches!(outcome, PlayerOutcome::NoResult(_))),
        "GAM-9: this game ranks the player who left"
    );
    assert!(matches!(
        outcomes[1].1,
        PlayerOutcome::Placed(_) | PlayerOutcome::Scored(_)
    ));
}
