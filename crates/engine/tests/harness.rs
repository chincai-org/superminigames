//! T2.4: proves the engine test harness itself. The tests below exercise the
//! scripted fake's initialize, tick, result, wake-up and finish behaviour
//! (GAM-1, GAM-4, GAM-7, GAM-8), the registry helpers (TYP-2, GAM-5, RND-12), retry
//! and panic scripts (FLR-1, FLR-2, FLR-4), scripted outcomes (RND-9,
//! T-SCR-10) and the deterministic player helper (TYP-1, TYP-3, CI-2). GAM-11
//! runs as the shared conformance suite in the game crates, not here.
//!
//! A scripted [`FakeGame`] initializes, ticks and reports a scripted result; the
//! [`TestRegistry`] helper builds a registry from factories in any order and a
//! counted factory shows how often a game is created; and the deterministic
//! `player` helper is stable and distinct. Later M2 tasks (T2.5, T2.6) reuse the
//! same harness.

#[path = "support/mod.rs"]
mod support;

use std::sync::atomic::{AtomicUsize, Ordering};

use superminigames_core::config::GameFilterMode;
use superminigames_core::game::{
    Game, GameError, GameEvent, GameInput, GameResult, ScoringMode, TimeModel,
};
use superminigames_core::ids::PlayerId;
use superminigames_core::outcome::PlayerOutcome;

use support::{
    FakeGame, InitReply, TestRegistry, TickScript, broadcast, player, rejected, to_player,
};

fn game_a() -> Box<dyn Game> {
    Box::new(FakeGame::builder("A", "Game A").build())
}

fn game_b() -> Box<dyn Game> {
    Box::new(FakeGame::builder("B", "Game B").build())
}

/// Counts how often the engine creates a game from this factory (TYP-2, RND-12).
static CREATED: AtomicUsize = AtomicUsize::new(0);

fn counted_factory() -> Box<dyn Game> {
    CREATED.fetch_add(1, Ordering::SeqCst);
    Box::new(FakeGame::builder("counted", "Counted").build())
}

fn no_filter() -> GameFilterMode {
    GameFilterMode::BanList(Vec::new())
}

#[test]
fn t_harness_player_helper_is_stable_and_distinct() {
    // REQ: TYP-1, TYP-3, CI-2
    assert_eq!(player(1), player(1));
    assert_eq!(player(42), player(42));
    assert_ne!(player(1), player(2));
    assert_ne!(player(1), player(42));

    // Stable across calls, never a fresh random id (no `Uuid::new_v4`).
    let first: Vec<PlayerId> = (0..8).map(player).collect();
    let again: Vec<PlayerId> = (0..8).map(player).collect();
    assert_eq!(first, again);
    assert_eq!(
        first
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        8,
        "distinct ids for distinct n"
    );
}

#[test]
fn t_harness_scripted_game_initializes_ticks_and_reports() {
    // REQ: GAM-1, GAM-4, GAM-7, GAM-8
    let players = [player(1), player(2)];
    let events = vec![broadcast(b"tick-0")];
    let mut game = FakeGame::builder("smoke", "Smoke")
        .supported_players(vec![2])
        .scoring_mode(ScoringMode::Placement)
        .time_model(TimeModel::RoundDeadlineOnly)
        .tick_script(vec![
            TickScript::for_input(GameInput::TimeAdvanced)
                .events(events.clone())
                .finishes(),
        ])
        .build();

    game.initialize(7, &players, 60_000)
        .expect("initialize accepts 2 players");
    assert_eq!(game.seed(), 7, "the seed is scripted state");
    assert_eq!(game.players(), players);

    let ticked = game.tick(1_000, GameInput::TimeAdvanced).expect("tick");
    assert_eq!(ticked, events);
    assert_eq!(game.clock_ms(), 1_000);
    assert!(game.is_finished());

    // GAM-8: a finished game ignores further inputs.
    let after = game.tick(2_000, GameInput::TimeAdvanced).expect("tick");
    assert_eq!(after, Vec::<GameEvent>::new());

    // GAM-7: the result covers every participant once finished.
    assert_eq!(
        game.result().outcomes,
        vec![
            (players[0], PlayerOutcome::Placed(1)),
            (players[1], PlayerOutcome::Placed(2)),
        ]
    );
}

#[test]
fn t_harness_registry_builds_from_factories_in_any_order() {
    // REQ: TYP-2, GAM-5
    let registry = TestRegistry::from_factories(&[game_b, game_a]);
    let ids: Vec<&str> = registry
        .registry()
        .games()
        .iter()
        .map(|game| game.id().0.as_str())
        .collect();
    assert_eq!(ids, ["A", "B"], "registration order must not matter");

    let pool = registry.registry().pool(&no_filter(), 2);
    assert_eq!(pool.len(), 2);
    assert_eq!(pool[0].id().0, "A");
}

#[test]
fn t_harness_registry_counts_every_creation() {
    // REQ: TYP-2, RND-12
    CREATED.store(0, Ordering::SeqCst);
    let registry = TestRegistry::from_factories(&[counted_factory]);
    assert_eq!(
        CREATED.load(Ordering::SeqCst),
        1,
        "register calls the factory once for the prototype (TYP-2)"
    );

    let mut first = registry.create("counted");
    let _second = registry.create("counted");
    assert_eq!(
        CREATED.load(Ordering::SeqCst),
        3,
        "create builds a fresh instance each time (RND-12)"
    );
    first
        .initialize(0, &[player(1), player(2)], 1_000)
        .expect("initialize accepts 2 players");
}

#[test]
fn t_harness_scripted_failures_drive_retry_and_abort() {
    // REQ: FLR-1, FLR-2, FLR-4, GAM-6
    let players = [player(1), player(2)];

    // FLR-1: the first attempt reports `InitFailed`, the retry succeeds.
    let mut retried = FakeGame::builder("retry", "Retry")
        .init_script(vec![InitReply::Fail("first".to_string()), InitReply::Ok])
        .build();
    assert!(matches!(
        retried.initialize(0, &players, 1_000),
        Err(GameError::InitFailed(_))
    ));
    retried
        .initialize(0, &players, 1_000)
        .expect("the retry succeeds");
    assert_eq!(retried.initialize_calls(), 2);

    // FLR-2: a panic is catchable, and the next attempt can succeed.
    let mut panicked = FakeGame::builder("panic", "Panic")
        .init_script(vec![InitReply::Panic, InitReply::Ok])
        .build();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = panicked.initialize(0, &players, 1_000);
    }));
    assert!(caught.is_err(), "the scripted panic must reach the caller");
    panicked
        .initialize(0, &players, 1_000)
        .expect("the retry succeeds");

    // FLR-4: a `Fatal` tick error and a tick panic are scripted too.
    let mut fatal = FakeGame::builder("fatal", "Fatal")
        .tick_reply(TickScript::for_any().fatal("boom"))
        .build();
    fatal.initialize(0, &players, 1_000).expect("initialize");
    assert!(matches!(
        fatal.tick(0, GameInput::TimeAdvanced),
        Err(GameError::Fatal(_))
    ));

    let mut tick_panic = FakeGame::builder("tickpanic", "TickPanic")
        .tick_reply(TickScript::for_any().panics())
        .build();
    tick_panic
        .initialize(0, &players, 1_000)
        .expect("initialize");
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = tick_panic.tick(0, GameInput::TimeAdvanced);
    }));
    assert!(
        caught.is_err(),
        "the scripted tick panic must reach the caller"
    );
}

#[test]
fn t_harness_scripted_results_cover_provisional_and_invalid_reports() {
    // REQ: GAM-7, RND-9, T-SCR-10
    let players = [player(1), player(2)];

    // A provisional `Scored` result for unfinished players (GAM-7).
    let mut provisional = FakeGame::builder("prov", "Provisional")
        .scoring_mode(ScoringMode::HigherIsBetter)
        .scored(vec![10, 20])
        .build();
    provisional
        .initialize(0, &players, 1_000)
        .expect("initialize");
    assert_eq!(
        provisional.result().outcomes,
        vec![
            (players[0], PlayerOutcome::Scored(10)),
            (players[1], PlayerOutcome::Scored(20)),
        ]
    );

    // An invalid report that omits a participant (RND-9).
    let mut omitting = FakeGame::builder("omit", "Omitting")
        .result(GameResult {
            outcomes: vec![(players[0], PlayerOutcome::Placed(1))],
        })
        .build();
    omitting.initialize(0, &players, 1_000).expect("initialize");
    assert_eq!(omitting.result().outcomes.len(), 1);
}

#[test]
fn t_harness_next_wakeup_advances_per_tick() {
    // REQ: GAM-6
    let players = [player(1), player(2)];
    let mut game = FakeGame::builder("wakeups", "Wake-ups")
        .wakeups(vec![100, 200])
        .build();
    game.initialize(0, &players, 1_000).expect("initialize");

    assert_eq!(game.next_wakeup_ms(), Some(100), "entry 0 before any tick");
    game.tick(0, GameInput::TimeAdvanced).expect("tick");
    assert_eq!(game.next_wakeup_ms(), Some(200), "entry 1 after one tick");
    game.tick(50, GameInput::TimeAdvanced).expect("tick");
    assert_eq!(game.next_wakeup_ms(), None, "`None` past the end");
}

#[test]
fn t_harness_finish_after_ticks_and_default_never_finish() {
    // REQ: GAM-6, GAM-8
    let players = [player(1), player(2)];

    // `finish_after_ticks` flips `is_finished` after exactly that many ticks.
    let mut timed = FakeGame::builder("timed", "Timed")
        .finish_after_ticks(2)
        .build();
    timed.initialize(0, &players, 1_000).expect("initialize");
    timed.tick(0, GameInput::TimeAdvanced).expect("tick");
    assert!(!timed.is_finished(), "not finished before the tick count");
    timed.tick(100, GameInput::TimeAdvanced).expect("tick");
    assert!(timed.is_finished(), "finished on the second tick");

    // An unscripted fake runs forever, so T2.5 can drive its own deadline.
    let mut endless = FakeGame::builder("endless", "Endless").build();
    endless.initialize(0, &players, 1_000).expect("initialize");
    for at_ms in [0, 100, 200] {
        endless.tick(at_ms, GameInput::TimeAdvanced).expect("tick");
    }
    assert!(!endless.is_finished(), "an unscripted fake never finishes");
}

#[test]
fn t_harness_scripted_to_player_and_rejected_events() {
    // REQ: GAM-6
    let players = [player(1), player(2)];
    let mut game = FakeGame::builder("events", "Events")
        .tick_script(vec![TickScript::for_any().events(vec![
            to_player(players[0], b"private"),
            rejected(players[1], "not your turn"),
        ])])
        .build();
    game.initialize(0, &players, 1_000).expect("initialize");

    let events = game.tick(0, GameInput::TimeAdvanced).expect("tick");
    assert_eq!(
        events,
        vec![
            to_player(players[0], b"private"),
            rejected(players[1], "not your turn"),
        ]
    );
    assert!(matches!(
        &events[0],
        GameEvent::ToPlayer { player, .. } if *player == players[0]
    ));
    assert!(matches!(&events[1], GameEvent::InputRejected { .. }));
}

#[test]
fn t_harness_four_failures_are_scriptable() {
    // REQ: FLR-1, FLR-2, FLR-4
    let players = [player(1), player(2)];
    let mut game = FakeGame::builder("exhausted", "Exhausted")
        .init_script(vec![
            InitReply::Fail("one".to_string()),
            InitReply::Fail("two".to_string()),
            InitReply::Fail("three".to_string()),
            InitReply::Fail("four".to_string()),
        ])
        .build();

    for attempt in 1..=4 {
        assert!(
            matches!(
                game.initialize(0, &players, 1_000),
                Err(GameError::InitFailed(_))
            ),
            "attempt {attempt} must fail"
        );
    }
    assert_eq!(game.initialize_calls(), 4, "START_ATTEMPTS is 4 (FLR-1)");
    assert_eq!(
        game.initialize_attempts().len(),
        4,
        "every failed attempt is recorded (RNG-6, FLR-2)"
    );
    assert!(!game.is_finished(), "an unstarted game is never finished");
}

#[test]
fn t_harness_records_every_initialize_attempt() {
    // REQ: TMR-1, TMR-11, RNG-6, FLR-2
    let players = [player(1), player(2)];
    let mut game = FakeGame::builder("attempts", "Attempts")
        .init_script(vec![InitReply::Fail("first".to_string()), InitReply::Ok])
        .build();

    assert!(game.initialize(7, &players, 60_000).is_err());
    game.initialize(7, &players, 60_000)
        .expect("retry succeeds");

    assert_eq!(
        game.time_limit_ms(),
        60_000,
        "the engine passes the deadline"
    );
    assert_eq!(game.seed(), 7, "the retry reuses the game seed (RNG-6)");
    assert_eq!(
        game.initialize_attempts().to_vec(),
        vec![(7, players.to_vec(), 60_000), (7, players.to_vec(), 60_000),]
    );
}
