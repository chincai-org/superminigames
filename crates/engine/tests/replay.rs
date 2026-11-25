//! T2.5: replay recording and replaying (REP-1, REP-2, T-REP-1).
//!
//! A `ReplayLog` from a real round is replayed on a fresh instance and must
//! reproduce the same events and the same `result()` (GAM-1, REP-2). A
//! panicking game is mapped to `GameError::Fatal`, never a crash (FLR-7).

#[path = "support/mod.rs"]
mod support;

use std::time::Duration;

use superminigames_core::config::GameFilterMode;
use superminigames_core::game::{
    Game, GameAction, GameError, GameEvent, GameInput, GameResult, ScoringMode,
};
use superminigames_core::ids::{GameId, Player, PlayerId};
use superminigames_core::outcome::PlayerOutcome;
use superminigames_engine::replay::{ReplayRecorder, replay};
use superminigames_engine::report::{PlayerState, ReplayLog, RoundStatus, Standing};
use superminigames_engine::round::{LAG_CAP_MS, RoundRunner};
use superminigames_engine::selection::{RoundPick, pick};
use tokio::time::Instant;

use support::{FakeGame, InitReply, TestRegistry, TickScript, broadcast, player, to_player};

/// No exclusions; keeps the whole registry.
fn no_filter() -> GameFilterMode {
    GameFilterMode::BanList(Vec::new())
}

/// The pick for `round` when every game supports 2 players (RNG-3).
fn pick_round(registry: &TestRegistry, round: u64) -> RoundPick<'_> {
    let pool = registry.registry().pool(&no_filter(), 2);
    pick(&pool, 0, round).expect("a non-empty pool")
}

/// InMatch states in seat order, as the actor owns them (TYP-1).
fn states(participants: &[PlayerId]) -> Vec<PlayerState> {
    participants
        .iter()
        .map(|&player| PlayerState {
            player: Player {
                id: player,
                name: String::new(),
            },
            ready: true,
            connected: true,
            standing: Standing::InMatch,
            absent_streak: 0,
            total_points: 0,
        })
        .collect()
}

/// A player's move payload for the fake game.
fn action(byte: u8, player: PlayerId) -> GameInput {
    GameInput::Player {
        player,
        action: GameAction(vec![byte]),
        lag_credit_ms: 0,
    }
}

/// A deterministic game whose replies depend only on the input sequence
/// (GAM-1): the first move returns two events, the clock advance finishes.
fn replay_factory() -> Box<dyn Game> {
    Box::new(
        FakeGame::builder("replay", "Replay")
            .scoring_mode(ScoringMode::HigherIsBetter)
            .tick_script(vec![
                TickScript::for_input(action(1, player(1)))
                    .events(vec![broadcast(b"one"), to_player(player(2), b"private")]),
                TickScript::for_input(GameInput::TimeAdvanced)
                    .events(vec![broadcast(b"clock")])
                    .finishes(),
            ])
            .scored(vec![3, 7])
            .build(),
    )
}

/// A factory whose `tick` panics (FLR-7).
fn panicking_tick_factory() -> Box<dyn Game> {
    Box::new(
        FakeGame::builder("panictick", "PanicTick")
            .tick_reply(TickScript::for_any().panics())
            .build(),
    )
}

/// A factory whose `initialize` panics (FLR-7).
fn panicking_init_factory() -> Box<dyn Game> {
    Box::new(
        FakeGame::builder("panicinit", "PanicInit")
            .init_script(vec![InitReply::Panic])
            .build(),
    )
}

#[test]
fn t_rep_1_replay_reproduces_the_events_and_the_result() {
    // REQ: REP-1, REP-2, GAM-1, T-REP-1
    let registry = TestRegistry::from_factories(&[replay_factory]);
    let participants = [player(1), player(2)];
    let run = pick_round(&registry, 0);
    let expected_game = run.game.id().clone();
    let expected_seed = run.game_seed;

    let mut runner =
        RoundRunner::start(&run, &participants, 0, 60_000, true).expect("initialize succeeds");
    let input_one = action(1, player(1));
    let first = runner.tick(0, input_one.clone()).expect("tick");
    let second = runner.tick(250, GameInput::TimeAdvanced).expect("tick");
    assert!(second.finished, "the scripted game finishes on this tick");
    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &[], true);
    assert_eq!(record.status, RoundStatus::Scored);
    let log = record.replay.expect("record_replays was on");

    // REP-1: the header and the exact tick order are recorded.
    assert_eq!(log.game, expected_game);
    assert_eq!(log.game_seed, expected_seed);
    assert_eq!(log.players, vec![player(1), player(2)]);
    assert_eq!(log.time_limit_ms, 60_000);
    assert_eq!(
        log.inputs,
        vec![(0, input_one), (250, GameInput::TimeAdvanced)]
    );

    // REP-2, T-REP-1: a fresh instance reproduces the events and the result.
    let replayed = replay(replay_factory, &log).expect("replay runs");
    let mut observed: Vec<GameEvent> = first.events.clone();
    observed.extend(second.events.clone());
    assert_eq!(replayed.events, observed);
    assert_eq!(
        replayed.result,
        GameResult {
            outcomes: vec![
                (player(1), PlayerOutcome::Scored(3)),
                (player(2), PlayerOutcome::Scored(7)),
            ],
        }
    );
}

#[test]
fn t_rep_1_recorder_keeps_the_ticked_order() {
    // REQ: REP-1, TMR-6
    let mut recorder = ReplayRecorder::new(
        GameId("replay".to_string()),
        42,
        vec![player(2), player(1)],
        1_000,
    );
    recorder.record(0, GameInput::TimeAdvanced);
    recorder.record(10, action(1, player(2)));

    let log = recorder.into_log();
    assert_eq!(log.game, GameId("replay".to_string()));
    assert_eq!(log.game_seed, 42);
    assert_eq!(log.players, vec![player(2), player(1)]);
    assert_eq!(log.time_limit_ms, 1_000);
    assert_eq!(
        log.inputs,
        vec![(0, GameInput::TimeAdvanced), (10, action(1, player(2)))]
    );
}

#[test]
fn t_rep_2_replay_maps_game_panics_to_fatal() {
    // REQ: REP-2, FLR-7
    let log = ReplayLog {
        game: GameId("panic".to_string()),
        game_seed: 1,
        players: vec![player(1), player(2)],
        time_limit_ms: 1_000,
        inputs: vec![(0, GameInput::TimeAdvanced)],
    };

    let tick_error =
        replay(panicking_tick_factory, &log).expect_err("a panicking tick is a failure");
    assert!(
        matches!(tick_error, GameError::Fatal(_)),
        "a tick panic becomes GameError::Fatal (FLR-7)"
    );

    let init_error =
        replay(panicking_init_factory, &log).expect_err("a panicking initialize is a failure");
    assert!(
        matches!(init_error, GameError::Fatal(_)),
        "an initialize panic becomes GameError::Fatal (FLR-7)"
    );
}

#[tokio::test(start_paused = true)]
async fn t_rep_1_replay_records_the_clamped_input_exactly() {
    // REQ: REP-1, REP-2, TMR-11, T-TMR-7, T-REP-1
    let registry = TestRegistry::from_factories(&[replay_factory]);
    let participants = [player(1), player(2)];
    let run = pick_round(&registry, 0);
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(&run, &participants, 0, 60_000, true, round_start)
        .expect("initialize succeeds");

    runner
        .submit_input(
            round_start + Duration::from_millis(5),
            player(1),
            GameAction(vec![1]),
            5_000,
        )
        .expect("move");
    let step = runner.tick_advanced(round_start + Duration::from_millis(250));
    assert!(step.finished, "the scripted clock advance finishes");

    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &[], true);
    let log = record.replay.expect("record_replays was on");
    assert_eq!(
        log.inputs,
        vec![
            (
                5,
                GameInput::Player {
                    player: player(1),
                    action: GameAction(vec![1]),
                    lag_credit_ms: LAG_CAP_MS,
                }
            ),
            (250, GameInput::TimeAdvanced),
        ],
        "the replay stores the clamped input (TMR-11)"
    );

    // REP-2, T-REP-1: the clamped log replays to the same events and result.
    let replayed = replay(replay_factory, &log).expect("replay runs");
    assert_eq!(replayed.events, vec![broadcast(b"clock")]);
    assert_eq!(
        replayed.result,
        GameResult {
            outcomes: vec![
                (player(1), PlayerOutcome::Scored(3)),
                (player(2), PlayerOutcome::Scored(7)),
            ],
        }
    );
}
