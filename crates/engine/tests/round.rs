//! T2.5: the round runner's start attempts, in-round loop, lag clamp, receipt
//! time, presence handling, result validation and `RoundRecord`
//! (RND-5..RND-13, FLR-1..FLR-7, SCR-2..SCR-11, TMR-1..TMR-11, PRS-5..PRS-11).
//!
//! T-RNG-4 is verified here at the round level: the real retry loop reuses the
//! pick and the game seed (T2.1 covers selection-level purity).
//!
//! The stage-2 loop is synchronous, so the paused-time tests inject the
//! `tokio::time::Instant` the actor would read, and never sleep (TMR-7).

#[path = "support/mod.rs"]
mod support;

use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use superminigames_core::config::GameFilterMode;
use superminigames_core::game::{
    Game, GameAction, GameError, GameEvent, GameFactory, GameInput, GameResult, ScoringMode,
    TimeModel,
};
use superminigames_core::ids::{GameId, Player, PlayerId};
use superminigames_core::outcome::{DisqualificationReason, PlayerOutcome};
use superminigames_engine::presence::{RECONNECT_GRACE, apply_round_start};
use superminigames_engine::report::{PlayerState, PlayerStatus, RoundStatus, SkipReason, Standing};
use superminigames_engine::round::{
    LAG_CAP_MS, LAG_TOTAL_CAP_MS, RoundEvent, RoundInputError, RoundRunner, RoundStep,
    START_ATTEMPTS,
};
use superminigames_engine::selection::{RoundPick, pick};
use tokio::time::{Instant, advance};

use support::{FakeGame, InitReply, TestRegistry, TickScript, player};

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

/// A player's move payload for the fake games.
fn action(byte: u8) -> GameAction {
    GameAction(vec![byte])
}

/// The two failures `FakeGame` cannot script: a `Fatal` from `initialize` (no
/// retry, FLR-4) and a panicking `result()` (FLR-7).
#[derive(Clone, Copy)]
enum Failure {
    FatalInit,
    PanicResult,
    CountResult,
}

/// How many times a [`Failure::CountResult`] game's `result()` was called.
static RESULT_CALLS: AtomicUsize = AtomicUsize::new(0);

/// A minimal `Game` that fails in exactly the configured callback.
struct FailingGame {
    failure: Failure,
}

impl Game for FailingGame {
    fn id(&self) -> GameId {
        GameId("failing".to_string())
    }

    fn name(&self) -> &'static str {
        "Failing"
    }

    fn scoring_mode(&self) -> ScoringMode {
        ScoringMode::Placement
    }

    fn time_model(&self) -> TimeModel {
        TimeModel::RoundDeadlineOnly
    }

    fn supports_players(&self, _count: usize) -> bool {
        true
    }

    fn initialize(
        &mut self,
        _seed: u64,
        _players: &[PlayerId],
        _time_limit_ms: u64,
    ) -> Result<(), GameError> {
        match self.failure {
            Failure::FatalInit => Err(GameError::Fatal("scripted initialize fatal".to_string())),
            Failure::PanicResult | Failure::CountResult => Ok(()),
        }
    }

    fn tick(&mut self, _at_ms: u64, _input: GameInput) -> Result<Vec<GameEvent>, GameError> {
        Ok(Vec::new())
    }

    fn next_wakeup_ms(&self) -> Option<u64> {
        None
    }

    fn is_finished(&self) -> bool {
        false
    }

    fn result(&self) -> GameResult {
        match self.failure {
            Failure::PanicResult => panic!("FailingGame: scripted result panic"),
            Failure::CountResult => {
                RESULT_CALLS.fetch_add(1, Ordering::SeqCst);
                GameResult {
                    outcomes: Vec::new(),
                }
            }
            Failure::FatalInit => GameResult {
                outcomes: Vec::new(),
            },
        }
    }
}

static FATAL_INIT_CALLS: AtomicUsize = AtomicUsize::new(0);

fn fatal_init_factory() -> Box<dyn Game> {
    FATAL_INIT_CALLS.fetch_add(1, Ordering::SeqCst);
    Box::new(FailingGame {
        failure: Failure::FatalInit,
    })
}

fn panic_result_factory() -> Box<dyn Game> {
    Box::new(FailingGame {
        failure: Failure::PanicResult,
    })
}

fn count_result_factory() -> Box<dyn Game> {
    Box::new(FailingGame {
        failure: Failure::CountResult,
    })
}

/// A `HigherIsBetter` game that always reports a provisional score for every
/// seat (GAM-7, T-TMR-5).
fn scored_factory() -> Box<dyn Game> {
    Box::new(
        FakeGame::builder("scored", "Scored")
            .scoring_mode(ScoringMode::HigherIsBetter)
            .scored(vec![10, 20])
            .build(),
    )
}

/// A game that accepts every input without ever finishing, so a test can drive
/// its own deadline and grace (GAM-6).
fn endless_factory() -> Box<dyn Game> {
    Box::new(FakeGame::builder("endless", "Endless").build())
}

/// A game that finishes on its first tick (GAM-8, TMR-3).
fn finish_once_factory() -> Box<dyn Game> {
    Box::new(
        FakeGame::builder("finishonce", "FinishOnce")
            .finish_after_ticks(1)
            .build(),
    )
}

/// A game whose wake-ups are 100 ms then 200 ms (GAM-6, T-TMR-6).
fn wakeup_factory() -> Box<dyn Game> {
    Box::new(
        FakeGame::builder("wakeup", "WakeUp")
            .wakeups(vec![100, 200])
            .build(),
    )
}

/// A factory that fails `initialize` for the created instances in
/// `fail_calls`; every other creation succeeds (FLR-1, FLR-2, RND-12).
fn scripted_factory(
    id: &'static str,
    name: &'static str,
    fail_calls: std::ops::RangeInclusive<usize>,
    counter: &'static AtomicUsize,
) -> Box<dyn Game> {
    let call = counter.fetch_add(1, Ordering::SeqCst);
    let builder = FakeGame::builder(id, name)
        .scoring_mode(ScoringMode::HigherIsBetter)
        .scored(vec![10, 20]);
    if fail_calls.contains(&call) {
        Box::new(
            builder
                .init_script(vec![InitReply::Fail(format!("attempt {call}"))])
                .build(),
        )
    } else {
        Box::new(builder.build())
    }
}

static FLR1_CALLS: AtomicUsize = AtomicUsize::new(0);

fn flr1_factory() -> Box<dyn Game> {
    // The prototype is call 0; attempts are calls 1..=4 (TYP-2, FLR-1).
    scripted_factory("flr1", "Flr1", 1..=3, &FLR1_CALLS)
}

#[test]
fn t_flr_1_init_failed_three_times_then_the_fourth_attempt_scores() {
    // REQ: FLR-1, FLR-2, FLR-6, RND-12, T-FLR-1
    FLR1_CALLS.store(0, Ordering::SeqCst);
    let registry = TestRegistry::from_factories(&[flr1_factory]);
    let participants = [player(1), player(2)];

    let runner = RoundRunner::start(&pick_round(&registry, 0), &participants, 0, 60_000, true)
        .expect("the fourth attempt succeeds");

    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &[], true);

    assert_eq!(record.status, RoundStatus::Scored);
    assert_eq!(
        record.attempts, 4,
        "three retries after the first attempt (FLR-1)"
    );
    assert_eq!(
        FLR1_CALLS.load(Ordering::SeqCst),
        5,
        "one prototype plus one fresh instance per attempt (RND-12)"
    );
    // HigherIsBetter scores 10 and 20: the higher score is place 1.
    assert_eq!(
        record.placements,
        vec![(player(1), 2, 0), (player(2), 1, 12)]
    );
    let total: u32 = states.iter().map(|state| state.total_points).sum();
    assert_eq!(total, 6 * 2, "a Scored round awards exactly 6 * n (SCR-11)");
}

static FLR2_CALLS: AtomicUsize = AtomicUsize::new(0);

fn flr2_factory() -> Box<dyn Game> {
    // Calls 1..=4 fail, so the first slot exhausts every attempt (FLR-3) and
    // the next slot's instance (call 5) succeeds.
    scripted_factory("flr2", "Flr2", 1..=4, &FLR2_CALLS)
}

#[test]
fn t_flr_2_four_failures_skip_the_slot_and_the_next_slot_runs() {
    // REQ: FLR-3, FLR-6, T-FLR-2
    FLR2_CALLS.store(0, Ordering::SeqCst);
    let registry = TestRegistry::from_factories(&[flr2_factory]);
    let participants = [player(1), player(2)];

    let failed = match RoundRunner::start(&pick_round(&registry, 0), &participants, 0, 60_000, true)
    {
        Ok(_) => panic!("every attempt must fail"),
        Err(failed) => failed,
    };
    assert_eq!(
        failed.attempts, START_ATTEMPTS,
        "exactly 4 attempts (FLR-1)"
    );
    assert_eq!(
        failed.status(),
        RoundStatus::Skipped(SkipReason::StartFailed)
    );
    assert_eq!(
        FLR2_CALLS.load(Ordering::SeqCst),
        5,
        "one prototype plus four attempts (RND-12)"
    );

    // FLR-3: the slot is consumed and the match continues with the next slot.
    let runner = RoundRunner::start(&pick_round(&registry, 1), &participants, 0, 60_000, false)
        .expect("the next slot starts normally");
    let mut states = states(&participants);
    let record = runner.finish(&mut states, 1, Vec::new(), &[], false);
    assert_eq!(record.status, RoundStatus::Scored);
    assert_eq!(record.attempts, 1);
}

static FATAL_CALLS: AtomicUsize = AtomicUsize::new(0);

fn fatal_factory() -> Box<dyn Game> {
    FATAL_CALLS.fetch_add(1, Ordering::SeqCst);
    Box::new(
        FakeGame::builder("fatal", "Fatal")
            .tick_reply(TickScript::for_any().fatal("boom"))
            .build(),
    )
}

#[test]
fn t_flr_3_fatal_tick_aborts_with_no_retry() {
    // REQ: FLR-4, T-FLR-3
    FATAL_CALLS.store(0, Ordering::SeqCst);
    let registry = TestRegistry::from_factories(&[fatal_factory]);
    let participants = [player(1), player(2)];

    let mut runner = RoundRunner::start(&pick_round(&registry, 0), &participants, 0, 60_000, false)
        .expect("initialize succeeds");
    assert_eq!(
        runner.tick(0, GameInput::TimeAdvanced),
        Err(SkipReason::GameAborted)
    );
    // FLR-4: no retry; only the prototype and the single playing instance exist.
    assert_eq!(FATAL_CALLS.load(Ordering::SeqCst), 2);
}

static PANIC_INIT_CALLS: AtomicUsize = AtomicUsize::new(0);

fn panic_init_factory() -> Box<dyn Game> {
    // Calls 1 and 2 panic, call 3 succeeds: the panic is caught and retried.
    let call = PANIC_INIT_CALLS.fetch_add(1, Ordering::SeqCst);
    let builder = FakeGame::builder("panicinit", "PanicInit")
        .scoring_mode(ScoringMode::HigherIsBetter)
        .scored(vec![5, 6]);
    if (1..=2).contains(&call) {
        Box::new(builder.init_script(vec![InitReply::Panic]).build())
    } else {
        Box::new(builder.build())
    }
}

fn panic_tick_factory() -> Box<dyn Game> {
    Box::new(
        FakeGame::builder("panictick", "PanicTick")
            .tick_reply(TickScript::for_any().panics())
            .build(),
    )
}

#[test]
fn t_flr_4_panics_in_initialize_and_tick_are_caught() {
    // REQ: FLR-1, FLR-4, FLR-7, T-FLR-4
    PANIC_INIT_CALLS.store(0, Ordering::SeqCst);
    let registry = TestRegistry::from_factories(&[panic_init_factory]);
    let participants = [player(1), player(2)];

    // FLR-7, FLR-1: a panicking `initialize` is caught and the next attempt runs.
    let runner = RoundRunner::start(&pick_round(&registry, 0), &participants, 0, 60_000, false)
        .expect("the third attempt succeeds");
    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &[], false);
    assert_eq!(record.status, RoundStatus::Scored);
    assert_eq!(record.attempts, 3);

    // FLR-7, FLR-4: a panicking `tick` is caught and aborts the round.
    let registry = TestRegistry::from_factories(&[panic_tick_factory]);
    let mut runner = RoundRunner::start(&pick_round(&registry, 0), &participants, 0, 60_000, false)
        .expect("initialize succeeds");
    assert_eq!(
        runner.tick(0, GameInput::TimeAdvanced),
        Err(SkipReason::GameAborted)
    );
}

fn silent_factory() -> Box<dyn Game> {
    // A Placement game whose `result()` is empty until it finishes (GAM-7).
    Box::new(FakeGame::builder("silent", "Silent").build())
}

#[test]
fn t_scr_5_all_no_result_is_skipped_with_zero_points() {
    // REQ: RND-9, SCR-9, T-SCR-5
    let registry = TestRegistry::from_factories(&[silent_factory]);
    let participants = [player(1), player(2)];

    let runner = RoundRunner::start(&pick_round(&registry, 0), &participants, 0, 60_000, false)
        .expect("initialize succeeds");
    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &[], false);

    assert_eq!(record.status, RoundStatus::Skipped(SkipReason::NoResults));
    assert!(record.placements.is_empty());
    // RND-9: the round reached its cap, so both are NoResult(TimerExpired).
    assert_eq!(
        record.round_disqualified,
        vec![
            (player(1), DisqualificationReason::TimerExpired),
            (player(2), DisqualificationReason::TimerExpired),
        ]
    );
    let total: u32 = states.iter().map(|state| state.total_points).sum();
    assert_eq!(total, 0, "a skipped round awards 0 to everyone (SCR-9)");
}

fn omitting_factory() -> Box<dyn Game> {
    // Placement, unfinished, reports only player 1 (RND-9).
    Box::new(
        FakeGame::builder("omit", "Omit")
            .result(GameResult {
                outcomes: vec![(player(1), PlayerOutcome::Placed(1))],
            })
            .build(),
    )
}

#[test]
fn t_rnd_9_missing_outcomes_fill_left_round_and_timer_expired() {
    // REQ: RND-9, SCR-5
    let registry = TestRegistry::from_factories(&[omitting_factory]);
    let participants = [player(1), player(2)];

    // The omitted player left mid-round: NoResult(LeftRound) (PRS-6).
    let runner = RoundRunner::start(&pick_round(&registry, 0), &participants, 0, 60_000, false)
        .expect("initialize succeeds");
    let mut left_states = states(&participants);
    let left_record = runner.finish(&mut left_states, 0, Vec::new(), &[player(2)], false);
    assert_eq!(left_record.status, RoundStatus::Scored);
    assert_eq!(
        left_record.round_disqualified,
        vec![(player(2), DisqualificationReason::LeftRound)]
    );
    assert_eq!(
        left_record.placements,
        vec![(player(1), 1, 12), (player(2), 2, 0)]
    );

    // Nobody left: the omitted player is NoResult(TimerExpired) (TMR-4).
    let runner = RoundRunner::start(&pick_round(&registry, 0), &participants, 0, 60_000, false)
        .expect("initialize succeeds");
    let mut timed_states = states(&participants);
    let timed_record = runner.finish(&mut timed_states, 0, Vec::new(), &[], false);
    assert_eq!(timed_record.status, RoundStatus::Scored);
    assert_eq!(
        timed_record.round_disqualified,
        vec![(player(2), DisqualificationReason::TimerExpired)]
    );
    assert_eq!(
        timed_record.placements,
        vec![(player(1), 1, 12), (player(2), 2, 0)]
    );
}

fn placement_scored_factory() -> Box<dyn Game> {
    Box::new(
        FakeGame::builder("ps", "Ps")
            .finish_after_ticks(1)
            .result(GameResult {
                outcomes: vec![
                    (player(1), PlayerOutcome::Scored(1)),
                    (player(2), PlayerOutcome::Scored(2)),
                ],
            })
            .build(),
    )
}

fn higher_placed_factory() -> Box<dyn Game> {
    Box::new(
        FakeGame::builder("hp", "Hp")
            .scoring_mode(ScoringMode::HigherIsBetter)
            .finish_after_ticks(1)
            .result(GameResult {
                outcomes: vec![
                    (player(1), PlayerOutcome::Placed(1)),
                    (player(2), PlayerOutcome::Placed(2)),
                ],
            })
            .build(),
    )
}

fn duplicate_factory() -> Box<dyn Game> {
    Box::new(
        FakeGame::builder("dup", "Dup")
            .finish_after_ticks(1)
            .result(GameResult {
                outcomes: vec![
                    (player(1), PlayerOutcome::Placed(1)),
                    (player(1), PlayerOutcome::Placed(2)),
                ],
            })
            .build(),
    )
}

fn placed_zero_factory() -> Box<dyn Game> {
    Box::new(
        FakeGame::builder("pz", "Pz")
            .finish_after_ticks(1)
            .result(GameResult {
                outcomes: vec![
                    (player(1), PlayerOutcome::Placed(0)),
                    (player(2), PlayerOutcome::Placed(2)),
                ],
            })
            .build(),
    )
}

fn finished_omits_factory() -> Box<dyn Game> {
    Box::new(
        FakeGame::builder("fo", "Fo")
            .finish_after_ticks(1)
            .result(GameResult {
                outcomes: vec![(player(1), PlayerOutcome::Placed(1))],
            })
            .build(),
    )
}

fn non_participant_factory() -> Box<dyn Game> {
    Box::new(
        FakeGame::builder("np", "Np")
            .finish_after_ticks(1)
            .result(GameResult {
                outcomes: vec![
                    (player(1), PlayerOutcome::Placed(1)),
                    (player(3), PlayerOutcome::Placed(2)),
                ],
            })
            .build(),
    )
}

fn placed_too_large_factory() -> Box<dyn Game> {
    Box::new(
        FakeGame::builder("pl", "Pl")
            .finish_after_ticks(1)
            .result(GameResult {
                outcomes: vec![
                    (player(1), PlayerOutcome::Placed(1)),
                    (player(2), PlayerOutcome::Placed(3)),
                ],
            })
            .build(),
    )
}

#[test]
fn t_scr_6_invalid_reports_are_skipped() {
    // REQ: RND-9, FLR-5, T-SCR-6
    let participants = [player(1), player(2)];
    let cases: [(&str, GameFactory); 7] = [
        ("scored in a Placement game", placement_scored_factory),
        ("placed in a HigherIsBetter game", higher_placed_factory),
        ("non-participant", non_participant_factory),
        ("duplicate player", duplicate_factory),
        ("Placed(0)", placed_zero_factory),
        ("Placed greater than n", placed_too_large_factory),
        ("finished but omits a participant", finished_omits_factory),
    ];

    for (case, factory) in cases {
        let registry = TestRegistry::from_factories(&[factory]);
        let mut runner =
            RoundRunner::start(&pick_round(&registry, 0), &participants, 0, 60_000, false)
                .expect("initialize succeeds");
        // Every case finishes on its first tick, so the finished rule applies.
        assert!(
            runner
                .tick(0, GameInput::TimeAdvanced)
                .expect("tick")
                .finished,
            "{case}"
        );
        let mut states = states(&participants);
        let record = runner.finish(&mut states, 0, Vec::new(), &[], false);
        assert_eq!(
            record.status,
            RoundStatus::Skipped(SkipReason::InvalidReport),
            "{case}"
        );
        assert!(record.placements.is_empty(), "{case}");
        let total: u32 = states.iter().map(|state| state.total_points).sum();
        assert_eq!(total, 0, "{case}");
    }
}

static RNG4_CALLS: AtomicUsize = AtomicUsize::new(0);

fn rng4_factory() -> Box<dyn Game> {
    // The first two attempts fail; the third plays (T-RNG-4).
    scripted_factory("rng4", "Rng4", 1..=2, &RNG4_CALLS)
}

#[test]
fn t_rng_4_retries_reuse_the_pick_and_the_game_seed() {
    // REQ: RNG-6, RND-11, RND-12, T-RNG-4
    RNG4_CALLS.store(0, Ordering::SeqCst);
    let registry = TestRegistry::from_factories(&[rng4_factory]);
    let participants = [player(1), player(2)];

    let run = pick_round(&registry, 0);
    let chosen = run.game.id().clone();
    let seed = run.game_seed;

    let runner = RoundRunner::start(&run, &participants, 1, 60_000, true)
        .expect("the third attempt succeeds");
    assert_eq!(
        RNG4_CALLS.load(Ordering::SeqCst),
        4,
        "one prototype plus three attempts, each a fresh instance (RND-12)"
    );

    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &[], true);
    assert_eq!(record.attempts, 3);
    assert_eq!(record.game, Some(chosen.clone()));
    let log = record.replay.expect("record_replays was on");
    assert_eq!(log.game, chosen, "the played game is the picked one");
    assert_eq!(
        log.game_seed, seed,
        "every attempt reused the pick's seed (RNG-6)"
    );
    // RND-11: seat 1 was the first mover, so it is passed first.
    assert_eq!(log.players, vec![player(2), player(1)]);
}

// ---------------------------------------------------------------------------
// Stage 2: the in-round loop, timers, presence, lag and receipt time.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn t_flr_4_a_fatal_initialize_is_not_retried() {
    // REQ: FLR-4, RND-5, FLR-6, T-FLR-3
    FATAL_INIT_CALLS.store(0, Ordering::SeqCst);
    let registry = TestRegistry::from_factories(&[fatal_init_factory]);
    let participants = [player(1), player(2)];

    let failed = match RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        60_000,
        false,
        Instant::now(),
    ) {
        Ok(_) => panic!("a Fatal initialize must not reach play"),
        Err(failed) => failed,
    };
    assert_eq!(
        failed.status(),
        RoundStatus::Skipped(SkipReason::GameAborted),
        "FLR-4 maps Fatal to GameAborted"
    );
    assert_eq!(failed.attempts, 1, "Fatal is never retried (FLR-4)");
    // One prototype (registration) plus one playing attempt (RND-12).
    assert_eq!(FATAL_INIT_CALLS.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn t_tmr_1_result_is_collected_exactly_once() {
    // REQ: RND-8, TMR-4, T-TMR-1
    RESULT_CALLS.store(0, Ordering::SeqCst);
    let registry = TestRegistry::from_factories(&[count_result_factory]);
    let participants = [player(1), player(2)];

    let runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        1_000,
        false,
        Instant::now(),
    )
    .expect("initialize succeeds");
    assert_eq!(
        RESULT_CALLS.load(Ordering::SeqCst),
        0,
        "result() is not called before the round ends"
    );

    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &[], false);
    assert_eq!(
        RESULT_CALLS.load(Ordering::SeqCst),
        1,
        "finish calls result() exactly once (RND-8)"
    );
    assert_eq!(record.status, RoundStatus::Skipped(SkipReason::NoResults));
}

#[tokio::test(start_paused = true)]
async fn t_tmr_2_inputs_after_the_deadline_are_rejected() {
    // REQ: TMR-2, TMR-4, T-TMR-2
    let registry = TestRegistry::from_factories(&[endless_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        1_000,
        false,
        round_start,
    )
    .expect("initialize succeeds");
    assert_eq!(
        runner.deadline(),
        round_start + Duration::from_millis(1_000)
    );

    // Before the deadline the input is fed to the game.
    let before = runner
        .submit_input(
            round_start + Duration::from_millis(999),
            player(1),
            action(1),
            0,
        )
        .expect("an input before the deadline");
    assert!(!before.finished);

    // At and after the deadline it is rejected and never ticked (TMR-4).
    advance(Duration::from_millis(1_000)).await;
    let now = Instant::now();
    assert_eq!(
        runner.submit_input(now, player(2), action(2), 0),
        Err(RoundInputError::NotInRound)
    );
    assert_eq!(
        runner.submit_input(
            round_start + Duration::from_millis(5_000),
            player(2),
            action(2),
            0
        ),
        Err(RoundInputError::NotInRound)
    );

    // The actor caps at the deadline and collects the result once.
    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &[], false);
    assert_eq!(record.status, RoundStatus::Skipped(SkipReason::NoResults));
    assert_eq!(
        record.round_disqualified,
        vec![
            (player(1), DisqualificationReason::TimerExpired),
            (player(2), DisqualificationReason::TimerExpired),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn t_tmr_3_a_finishing_tick_ends_the_round() {
    // REQ: TMR-3, GAM-8, T-TMR-3
    let registry = TestRegistry::from_factories(&[finish_once_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        60_000,
        false,
        round_start,
    )
    .expect("initialize succeeds");

    let step = runner
        .submit_input(round_start, player(1), action(1), 0)
        .expect("input");
    assert!(step.finished, "the first tick finishes the game (TMR-3)");
    assert!(runner.ended(), "the round ends at that tick");
    assert!(!runner.all_left(), "the deadline never fired");

    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &[], false);
    assert_eq!(record.status, RoundStatus::Scored);
}

#[tokio::test(start_paused = true)]
async fn t_tmr_4_replay_keeps_the_dequeue_order() {
    // REQ: TMR-6, REP-1, T-TMR-4
    let registry = TestRegistry::from_factories(&[endless_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        60_000,
        true,
        round_start,
    )
    .expect("initialize succeeds");

    runner
        .submit_input(round_start, player(1), action(1), 0)
        .expect("I1");
    runner
        .submit_input(
            round_start + Duration::from_millis(10),
            player(2),
            action(2),
            0,
        )
        .expect("I2");

    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &[], true);
    let log = record.replay.expect("recording was on");
    let inputs: Vec<GameInput> = log.inputs.iter().map(|(_, input)| input.clone()).collect();
    assert_eq!(
        inputs,
        vec![
            GameInput::Player {
                player: player(1),
                action: action(1),
                lag_credit_ms: 0,
            },
            GameInput::Player {
                player: player(2),
                action: action(2),
                lag_credit_ms: 0,
            },
        ],
        "ticks run in dequeue order (TMR-6)"
    );
}

#[tokio::test(start_paused = true)]
async fn t_tmr_5_deadline_only_game_scores_everyone() {
    // REQ: GAM-7, TMR-9, PRS-11, T-TMR-5
    let registry = TestRegistry::from_factories(&[scored_factory]);
    let participants = [player(1), player(2)];
    let runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        1_000,
        false,
        Instant::now(),
    )
    .expect("initialize succeeds");

    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &[], false);
    assert_eq!(record.status, RoundStatus::Scored);
    assert_eq!(
        record.placements,
        vec![(player(1), 2, 0), (player(2), 1, 12)]
    );
    assert!(
        record.round_disqualified.is_empty(),
        "a RoundDeadlineOnly game ranks everyone, so nobody times out (TMR-9)"
    );
    assert!(
        states
            .iter()
            .all(|state| state.standing == Standing::InMatch),
        "a game outcome is never reported as NoResult (PRS-11)"
    );
}

#[tokio::test(start_paused = true)]
async fn t_tmr_6_wakeups_tick_time_advanced_at_round_start_plus_t() {
    // REQ: TMR-2, GAM-6, T-TMR-6
    let registry = TestRegistry::from_factories(&[wakeup_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        60_000,
        false,
        round_start,
    )
    .expect("initialize succeeds");

    // The actor arms round_start + 100 ms, then re-reads after the tick.
    assert_eq!(runner.next_wakeup_ms(), Some(100));
    let step = runner.tick_advanced(round_start + Duration::from_millis(100));
    assert!(!step.aborted);
    assert_eq!(runner.next_wakeup_ms(), Some(200));
    runner.tick_advanced(round_start + Duration::from_millis(200));
    assert_eq!(runner.next_wakeup_ms(), None, "None past the last entry");
}

#[tokio::test(start_paused = true)]
async fn t_tmr_7_lag_credit_is_clamped_per_input_and_in_total() {
    // REQ: TMR-11, T-TMR-7, REP-1
    let registry = TestRegistry::from_factories(&[endless_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        60_000,
        true,
        round_start,
    )
    .expect("initialize succeeds");

    // The per-input cap bites first: 1_500 becomes LAG_CAP_MS.
    runner
        .submit_input(round_start, player(1), action(1), 1_500)
        .expect("input 1");
    // Ten full-cap inputs exhaust the 10_000 total; the eleventh gets nothing.
    for n in 1..=10u64 {
        runner
            .submit_input(
                round_start + Duration::from_millis(n * 10),
                player(1),
                action(1),
                LAG_CAP_MS,
            )
            .expect("input");
    }

    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &[], true);
    let log = record.replay.expect("recording was on");
    let credits: Vec<u32> = log
        .inputs
        .iter()
        .map(|(_, input)| match input {
            GameInput::Player { lag_credit_ms, .. } => *lag_credit_ms,
            other => panic!("only Player inputs are recorded, got {other:?}"),
        })
        .collect();
    assert_eq!(log.inputs.len(), 11);
    assert_eq!(credits[0], LAG_CAP_MS, "the per-input cap clamps 1_500");
    assert_eq!(
        credits.iter().sum::<u32>(),
        LAG_TOTAL_CAP_MS,
        "the round total is capped at 10_000"
    );
    assert_eq!(
        credits[10], 0,
        "the eleventh input gets no remaining credit"
    );
}

#[tokio::test(start_paused = true)]
async fn t_tmr_8_at_ms_comes_from_receipt_time_and_never_decreases() {
    // REQ: TMR-1, TMR-8, REP-1
    let registry = TestRegistry::from_factories(&[endless_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        60_000,
        true,
        round_start,
    )
    .expect("initialize succeeds");

    runner
        .submit_input(
            round_start + Duration::from_millis(150),
            player(1),
            action(1),
            0,
        )
        .expect("I1");
    // I2's receipt time is earlier (an out-of-order dequeue): at_ms must not fall.
    runner
        .submit_input(
            round_start + Duration::from_millis(100),
            player(2),
            action(2),
            0,
        )
        .expect("I2");

    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &[], true);
    let log = record.replay.expect("recording was on");
    assert_eq!(log.inputs[0].0, 150);
    assert_eq!(log.inputs[1].0, 150, "at_ms is clamped monotonic (TMR-1)");
    assert_eq!(
        log.inputs[1].1,
        GameInput::Player {
            player: player(2),
            action: action(2),
            lag_credit_ms: 0,
        }
    );
}

#[tokio::test(start_paused = true)]
async fn t_flr_7_a_panicking_result_is_skipped_invalid_report() {
    // REQ: FLR-7, RND-9
    let registry = TestRegistry::from_factories(&[panic_result_factory]);
    let participants = [player(1), player(2)];
    let runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        60_000,
        false,
        Instant::now(),
    )
    .expect("initialize succeeds");

    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &[], false);
    assert_eq!(
        record.status,
        RoundStatus::Skipped(SkipReason::InvalidReport)
    );
    assert!(record.replay.is_none());
}

#[tokio::test(start_paused = true)]
async fn t_prs_5_non_participant_inputs_are_rejected() {
    // REQ: PRS-5, RND-3
    let registry = TestRegistry::from_factories(&[endless_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        60_000,
        false,
        round_start,
    )
    .expect("initialize succeeds");
    assert_eq!(runner.participants(), &participants[..]);
    assert_eq!(
        runner.submit_input(round_start, player(3), action(1), 0),
        Err(RoundInputError::NotParticipant)
    );
}

#[tokio::test(start_paused = true)]
async fn t_prs_6_disconnect_and_reconnect_emit_away_and_present() {
    // REQ: PRS-6, PRS-11, T-PRS-11
    let registry = TestRegistry::from_factories(&[endless_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        60_000,
        false,
        round_start,
    )
    .expect("initialize succeeds");

    let away = runner.on_connection(player(1), false, round_start);
    assert_eq!(
        away,
        vec![RoundEvent::PlayerStatus {
            player: player(1),
            status: PlayerStatus::Away,
        }]
    );
    assert_eq!(
        runner.next_grace_deadline(),
        Some(round_start + RECONNECT_GRACE),
        "the actor can arm the grace timer (TMR-2)"
    );
    // A flap does not report a second Away (PRS-6).
    assert!(
        runner
            .on_connection(player(1), false, round_start + Duration::from_secs(5))
            .is_empty()
    );
    // Back inside the grace: Present, and the player keeps playing.
    let back = runner.on_connection(player(1), true, round_start + Duration::from_secs(29));
    assert_eq!(
        back,
        vec![RoundEvent::PlayerStatus {
            player: player(1),
            status: PlayerStatus::Present,
        }]
    );
    assert!(
        runner
            .submit_input(
                round_start + Duration::from_secs(29),
                player(1),
                action(1),
                0
            )
            .is_ok()
    );
}

#[tokio::test(start_paused = true)]
async fn t_prs_6_grace_expiry_ticks_playerleft_once_and_rejects_inputs() {
    // REQ: PRS-6, PRS-7, T-PRS-1, T-PRS-8
    let registry = TestRegistry::from_factories(&[endless_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        600_000,
        true,
        round_start,
    )
    .expect("initialize succeeds");

    runner.on_connection(player(1), false, round_start);
    // Before the grace ends nothing is ticked and inputs still work.
    assert!(
        runner
            .expire(round_start + Duration::from_secs(29))
            .events
            .is_empty()
    );
    assert!(
        runner
            .submit_input(
                round_start + Duration::from_secs(29),
                player(1),
                action(1),
                0
            )
            .is_ok()
    );

    // At the boundary the player is left exactly once (PRS-6).
    runner.expire(round_start + RECONNECT_GRACE);
    assert_eq!(runner.left(), &[player(1)]);
    runner.expire(round_start + RECONNECT_GRACE + Duration::from_secs(1));
    assert_eq!(runner.left(), &[player(1)], "left is reported once");

    // Afterwards their inputs are rejected for the rest of the round (PRS-7).
    assert_eq!(
        runner.submit_input(
            round_start + RECONNECT_GRACE + Duration::from_millis(1),
            player(1),
            action(2),
            0
        ),
        Err(RoundInputError::PlayerLeftRound)
    );

    // The PlayerLeft tick was recorded exactly once (REP-1).
    let left = runner.left().to_vec();
    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &left, true);
    let log = record.replay.expect("recording was on");
    let player_lefts: Vec<GameInput> = log
        .inputs
        .iter()
        .filter(|(_, input)| matches!(input, GameInput::PlayerLeft(_)))
        .map(|(_, input)| input.clone())
        .collect();
    assert_eq!(player_lefts, vec![GameInput::PlayerLeft(player(1))]);
}

#[tokio::test(start_paused = true)]
async fn t_prs_6_a_fatal_on_player_left_aborts_the_round() {
    // REQ: PRS-6, FLR-4
    let registry = TestRegistry::from_factories(&[fatal_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        600_000,
        false,
        round_start,
    )
    .expect("initialize succeeds");

    runner.on_connection(player(1), false, round_start);
    let step = runner.expire(round_start + RECONNECT_GRACE);
    assert!(step.aborted, "the PlayerLeft tick returned Fatal (FLR-4)");
    assert!(runner.aborted());

    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &[], false);
    assert_eq!(record.status, RoundStatus::Skipped(SkipReason::GameAborted));
}

#[tokio::test(start_paused = true)]
async fn t_prs_7_reconnect_after_the_grace_does_not_rejoin() {
    // REQ: PRS-7, PRS-6
    let registry = TestRegistry::from_factories(&[endless_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        600_000,
        false,
        round_start,
    )
    .expect("initialize succeeds");

    runner.on_connection(player(1), false, round_start);
    runner.expire(round_start + RECONNECT_GRACE);
    // A late reconnect neither reports Present nor re-enters the round.
    assert!(
        runner
            .on_connection(
                player(1),
                true,
                round_start + RECONNECT_GRACE + Duration::from_secs(5)
            )
            .is_empty()
    );
    assert_eq!(
        runner.submit_input(
            round_start + RECONNECT_GRACE + Duration::from_secs(6),
            player(1),
            action(1),
            0
        ),
        Err(RoundInputError::PlayerLeftRound)
    );

    // They are a participant again from the next round start (PRS-5).
    let next = RoundRunner::start_at(
        &pick_round(&registry, 1),
        &participants,
        0,
        600_000,
        false,
        round_start,
    )
    .expect("initialize succeeds");
    assert!(next.participants().contains(&player(1)));
}

#[test]
fn t_prs_8_a_round_start_still_updates_the_streak() {
    // REQ: PRS-8, TMR-5, PRS-2
    let mut match_states = states(&[player(1), player(2)]);
    match_states[0].connected = false;
    let start = apply_round_start(&mut match_states, 3);
    assert_eq!(start.participants, vec![player(2)]);
    assert_eq!(
        match_states[0].absent_streak, 1,
        "only a round start changes the streak (PRS-2, PRS-8)"
    );
}

#[tokio::test(start_paused = true)]
async fn t_rnd_13_a_disconnected_participant_is_seeded_at_zero() {
    // REQ: RND-13, PRS-6, T-PRS-9
    let registry = TestRegistry::from_factories(&[endless_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        600_000,
        false,
        round_start,
    )
    .expect("initialize succeeds");

    // Between the presence snapshot and play, player 1 dropped: the runner
    // seeds their grace itself at t=0 (RND-13).
    let seeded = runner.seed_disconnected(&[player(1)]);
    assert_eq!(
        seeded,
        vec![RoundEvent::PlayerStatus {
            player: player(1),
            status: PlayerStatus::Away,
        }]
    );
    assert_eq!(
        runner.next_grace_deadline(),
        Some(round_start + RECONNECT_GRACE),
        "grace starts at round_start, so it ends exactly one grace later (RND-13)"
    );
    assert_eq!(runner.next_wakeup_ms(), None);

    // Back inside the grace, so no PlayerLeft fires (T-PRS-9).
    let back = runner.on_connection(player(1), true, round_start + Duration::from_secs(10));
    assert_eq!(
        back,
        vec![RoundEvent::PlayerStatus {
            player: player(1),
            status: PlayerStatus::Present,
        }]
    );
    runner.expire(round_start + RECONNECT_GRACE);
    assert!(runner.left().is_empty());
}

#[tokio::test(start_paused = true)]
async fn t_rnd_13_the_round_ends_when_every_grace_expires() {
    // REQ: RND-13, PRS-6, T-PRS-10
    let registry = TestRegistry::from_factories(&[endless_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        600_000,
        false,
        round_start,
    )
    .expect("initialize succeeds");

    runner.on_connection(player(1), false, round_start);
    runner.on_connection(player(2), false, round_start);
    assert!(!runner.all_left());
    runner.expire(round_start + RECONNECT_GRACE);
    assert!(
        runner.all_left(),
        "every grace expired, so the round ends now (RND-13)"
    );
    assert!(runner.ended(), "the actor stops waiting for the deadline");
}

#[tokio::test(start_paused = true)]
async fn t_rnd_13_finish_reports_removed_this_round_and_left_fills_left_round() {
    // REQ: RND-13, RND-9, PRS-3, PRS-6, PRS-8, PRS-9
    let registry = TestRegistry::from_factories(&[endless_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        600_000,
        false,
        round_start,
    )
    .expect("initialize succeeds");
    runner.on_connection(player(1), false, round_start);
    runner.expire(round_start + RECONNECT_GRACE);

    // A player absent three starts is removed at that start (PRS-3).
    let mut match_states = states(&participants);
    match_states[1].connected = false;
    let start = apply_round_start(&mut match_states, 1);
    assert_eq!(start.removed, vec![player(2)]);

    let left = runner.left().to_vec();
    let mut scored_states = states(&participants);
    let record = runner.finish(&mut scored_states, 0, start.removed, &left, false);
    assert_eq!(record.removed_this_round, vec![player(2)]);
    assert_eq!(
        record.round_disqualified,
        vec![
            (player(1), DisqualificationReason::LeftRound),
            (player(2), DisqualificationReason::TimerExpired),
        ]
    );
    // PRS-8, PRS-9: a round disqualification changes neither Standing nor streak,
    // so the player still counts as InMatch for the match end condition.
    for state in &scored_states {
        assert_eq!(state.standing, Standing::InMatch);
        assert_eq!(state.absent_streak, 0);
    }
}

#[tokio::test(start_paused = true)]
async fn t_rep_1_replay_is_disabled_when_requested() {
    // REQ: REP-1, REP-3
    let registry = TestRegistry::from_factories(&[endless_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        600_000,
        false,
        round_start,
    )
    .expect("initialize succeeds");
    runner
        .submit_input(round_start, player(1), action(1), 0)
        .expect("input");

    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &[], false);
    assert!(record.replay.is_none(), "record_replays was off (REP-3)");
}

#[tokio::test(start_paused = true)]
async fn t_rep_1_replay_header_keeps_the_seat_order() {
    // REQ: REP-1, RND-11
    let registry = TestRegistry::from_factories(&[endless_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        1,
        600_000,
        true,
        round_start,
    )
    .expect("initialize succeeds");

    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &[], true);
    let log = record.replay.expect("recording was on");
    assert_eq!(
        log.players,
        vec![player(2), player(1)],
        "the first mover is seat 1 (RND-11)"
    );
    assert_eq!(log.time_limit_ms, 600_000);
}

// ---------------------------------------------------------------------------
// Stage 2 review fixes: automatic RND-13 seeding, the hard-deadline guards,
// post-init clock capture, cached finished flag and the tracked-left merge.
// ---------------------------------------------------------------------------

/// A game whose `tick` succeeds but whose `is_finished()` panics (FLR-7).
struct PanicFinishedGame;

impl Game for PanicFinishedGame {
    fn id(&self) -> GameId {
        GameId("panicfinished".to_string())
    }

    fn name(&self) -> &'static str {
        "PanicFinished"
    }

    fn scoring_mode(&self) -> ScoringMode {
        ScoringMode::Placement
    }

    fn time_model(&self) -> TimeModel {
        TimeModel::RoundDeadlineOnly
    }

    fn supports_players(&self, _count: usize) -> bool {
        true
    }

    fn initialize(
        &mut self,
        _seed: u64,
        _players: &[PlayerId],
        _time_limit_ms: u64,
    ) -> Result<(), GameError> {
        Ok(())
    }

    fn tick(&mut self, _at_ms: u64, _input: GameInput) -> Result<Vec<GameEvent>, GameError> {
        Ok(Vec::new())
    }

    fn next_wakeup_ms(&self) -> Option<u64> {
        None
    }

    fn is_finished(&self) -> bool {
        panic!("PanicFinishedGame: scripted is_finished panic")
    }

    fn result(&self) -> GameResult {
        GameResult {
            outcomes: Vec::new(),
        }
    }
}

fn panic_finished_factory() -> Box<dyn Game> {
    Box::new(PanicFinishedGame)
}

static CLOCK_FAIL_CALLS: AtomicUsize = AtomicUsize::new(0);

fn clock_retry_factory() -> Box<dyn Game> {
    // The prototype is call 0; attempts 1..=2 fail, attempt 3 succeeds.
    scripted_factory("clockretry", "ClockRetry", 1..=2, &CLOCK_FAIL_CALLS)
}

#[tokio::test(start_paused = true)]
async fn t_tmr_1_the_clock_is_read_only_after_a_successful_initialize() {
    // REQ: TMR-1, FLR-2
    CLOCK_FAIL_CALLS.store(0, Ordering::SeqCst);
    let registry = TestRegistry::from_factories(&[clock_retry_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let clock_calls = Cell::new(0u32);
    let runner = RoundRunner::start_with_clock(
        &pick_round(&registry, 0),
        &participants,
        0,
        60_000,
        false,
        || {
            clock_calls.set(clock_calls.get() + 1);
            round_start
        },
    )
    .expect("the third attempt succeeds");

    assert_eq!(
        clock_calls.get(),
        1,
        "two failed attempts must not read the clock (TMR-1, FLR-2)"
    );
    assert_eq!(runner.round_start(), round_start);
    assert_eq!(
        runner.deadline(),
        round_start + Duration::from_millis(60_000)
    );
}

#[tokio::test(start_paused = true)]
async fn t_tmr_4_a_wakeup_at_the_deadline_is_not_ticked() {
    // REQ: TMR-4, TMR-2
    let registry = TestRegistry::from_factories(&[finish_once_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        100,
        false,
        round_start,
    )
    .expect("initialize succeeds");

    let step = runner.tick_advanced(round_start + Duration::from_millis(100));
    assert_eq!(
        step,
        RoundStep::default(),
        "no TimeAdvanced tick at the deadline (TMR-4)"
    );
    // Past the deadline, a late wake-up must not tick either.
    assert_eq!(
        runner.tick_advanced(round_start + Duration::from_millis(150)),
        RoundStep::default(),
        "no TimeAdvanced tick past the deadline (TMR-4)"
    );
    assert!(
        !runner.ended(),
        "the actor, not the runner, ends the round at the deadline"
    );
}

#[tokio::test(start_paused = true)]
async fn t_tmr_4_a_grace_expiry_past_the_deadline_does_not_tick_player_left() {
    // REQ: TMR-4, PRS-6
    let registry = TestRegistry::from_factories(&[endless_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        100,
        false,
        round_start,
    )
    .expect("initialize succeeds");
    runner.seed_disconnected(&[player(1)]);

    let step = runner.expire(round_start + RECONNECT_GRACE);
    assert_eq!(
        step,
        RoundStep::default(),
        "no PlayerLeft tick at or after the deadline (TMR-4)"
    );
    assert!(
        runner.left().is_empty(),
        "the grace cannot expire past the hard cap (TMR-4)"
    );
}

#[tokio::test(start_paused = true)]
async fn t_tmr_4_presence_changes_past_the_deadline_are_ignored() {
    // REQ: TMR-4, PRS-6
    let registry = TestRegistry::from_factories(&[endless_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        100,
        false,
        round_start,
    )
    .expect("initialize succeeds");

    let events = runner.on_connection(player(1), false, round_start + Duration::from_millis(100));
    assert!(
        events.is_empty(),
        "no Away once the deadline has passed (TMR-4)"
    );
    assert_eq!(
        runner.next_grace_deadline(),
        None,
        "no grace started past the hard cap (TMR-4)"
    );
}

#[tokio::test(start_paused = true)]
async fn t_rnd_13_seed_disconnected_is_unique_and_ignores_non_participants() {
    // REQ: RND-13, PRS-6
    let registry = TestRegistry::from_factories(&[endless_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        600_000,
        false,
        round_start,
    )
    .expect("initialize succeeds");

    let seeded = runner.seed_disconnected(&[player(1), player(1), player(3)]);
    assert_eq!(
        seeded,
        vec![RoundEvent::PlayerStatus {
            player: player(1),
            status: PlayerStatus::Away,
        }],
        "one Away for the participant, nothing for the stranger or the repeat"
    );
    assert_eq!(
        runner.next_grace_deadline(),
        Some(round_start + RECONNECT_GRACE)
    );
}

#[tokio::test(start_paused = true)]
async fn t_rnd_9_tracked_left_fills_left_round_with_an_empty_caller_list() {
    // REQ: RND-9, PRS-6
    let registry = TestRegistry::from_factories(&[omitting_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        600_000,
        false,
        round_start,
    )
    .expect("initialize succeeds");

    runner.seed_disconnected(&[player(2)]);
    let step = runner.expire(round_start + RECONNECT_GRACE);
    assert!(!step.aborted);
    assert_eq!(runner.left(), &[player(2)]);

    // The caller passes `&[]`, but the tracked PlayerLeft still fills the outcome.
    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &[], false);
    assert_eq!(record.status, RoundStatus::Scored);
    assert_eq!(
        record.round_disqualified,
        vec![(player(2), DisqualificationReason::LeftRound)]
    );
}

#[tokio::test(start_paused = true)]
async fn t_rep_1_a_tick_whose_is_finished_panics_is_still_recorded() {
    // REQ: REP-1, FLR-7
    let registry = TestRegistry::from_factories(&[panic_finished_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        60_000,
        true,
        round_start,
    )
    .expect("initialize succeeds");

    let input = GameInput::Player {
        player: player(1),
        action: action(1),
        lag_credit_ms: 0,
    };
    assert_eq!(
        runner.tick(0, input.clone()),
        Err(SkipReason::GameAborted),
        "the is_finished panic aborts the round (FLR-7)"
    );
    assert!(runner.aborted());

    let mut states = states(&participants);
    let record = runner.finish(&mut states, 0, Vec::new(), &[], true);
    assert_eq!(record.status, RoundStatus::Skipped(SkipReason::GameAborted));
    let log = record.replay.expect("recording was on");
    assert_eq!(
        log.inputs,
        vec![(0, input)],
        "the tick ran, so it is recorded before the abort (REP-1)"
    );
}

#[tokio::test(start_paused = true)]
async fn t_tmr_4_a_grace_expiring_at_the_deadline_is_not_ticked() {
    // REQ: TMR-4, PRS-6
    let registry = TestRegistry::from_factories(&[endless_factory]);
    let participants = [player(1), player(2)];
    let round_start = Instant::now();
    let mut runner = RoundRunner::start_at(
        &pick_round(&registry, 0),
        &participants,
        0,
        100,
        false,
        round_start,
    )
    .expect("initialize succeeds");
    runner.on_connection(player(1), false, round_start);

    // The grace is longer than the round, so it can only expire at or after the
    // deadline. Either way no PlayerLeft is fed to the game (TMR-4).
    assert_eq!(
        runner.expire(round_start + Duration::from_millis(100)),
        RoundStep::default(),
        "no PlayerLeft tick at the deadline"
    );
    assert!(runner.left().is_empty());
    assert!(
        runner
            .on_connection(player(2), false, round_start + Duration::from_millis(120))
            .is_empty(),
        "presence cannot change past the deadline"
    );
}
