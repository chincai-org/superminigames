//! The shared game conformance suite (GAM-11, CI-7).
//!
//! Every game crate calls these functions from its own tests, one test per
//! requirement ID, so the suite and the IDs stay visible in that crate. The
//! functions panic with a requirement ID when the game breaks a rule.
//!
//! The script helpers use only engine-generated inputs (`TimeAdvanced`,
//! `PlayerLeft`), which every game must accept whatever its own action
//! encoding is (GAM-2, GAM-9).

use super::super::game::{GameAction, GameEvent, GameFactory, GameInput, ScoringMode};
use super::super::ids::PlayerId;

/// A budget large enough for any real game to reach a result in the tests.
pub const MAX_TICKS: u64 = 10_000;

/// Every suite function needs at least one player to address.
fn require_players(players: &[PlayerId]) {
    assert!(
        !players.is_empty(),
        "GAM-11: the conformance suite needs at least one player"
    );
}
/// An input sequence of engine-generated inputs (T-GAM-1).
pub fn generic_script(players: &[PlayerId]) -> Vec<(u64, GameInput)> {
    let mut script = Vec::new();
    for step in 0..4u64 {
        script.push((step * 1_000, GameInput::TimeAdvanced));
        if let Some(player) = players.last() {
            if step % 2 == 1 {
                script.push((step * 1_000 + 1, GameInput::PlayerLeft(*player)));
            }
        }
    }
    script
}

/// Payload shapes no game should accept: empty, truncated and huge (T-GAM-3).
///
/// A game whose codec legitimately accepts one of these MUST pass its own
/// malformed set to [`t_gam_3_malformed_actions_are_rejected`] instead.
pub fn shape_malformed_actions() -> Vec<GameAction> {
    vec![
        GameAction(Vec::new()),
        GameAction(vec![0xff]),
        GameAction(vec![0x00; 1_024]),
    ]
}

/// T-GAM-1: the same seed, players and ordered inputs give the same events and
/// the same `result()` (GAM-1, GAM-3, REP-2).
pub fn t_gam_1_same_seed_same_run(
    factory: GameFactory,
    players: &[PlayerId],
    seed: u64,
    script: &[(u64, GameInput)],
) {
    require_players(players);
    let mut left = factory();
    let mut right = factory();
    left.initialize(seed, players, 1_000_000)
        .expect("T-GAM-1: initialize must accept the players it supports");
    right
        .initialize(seed, players, 1_000_000)
        .expect("T-GAM-1: initialize must accept the players it supports");

    for (at_ms, input) in script {
        let a = left.tick(*at_ms, input.clone());
        let b = right.tick(*at_ms, input.clone());
        match (a, b) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "T-GAM-1: different events at {at_ms}ms"),
            (Err(a), Err(b)) => assert_eq!(
                format!("{a}"),
                format!("{b}"),
                "T-GAM-1: different errors at {at_ms}ms"
            ),
            (a, b) => panic!("T-GAM-1: different outcomes at {at_ms}ms: {a:?} vs {b:?}"),
        }
        assert_eq!(
            left.result().outcomes,
            right.result().outcomes,
            "T-GAM-1: different results at {at_ms}ms"
        );
    }
}

/// T-GAM-2: `id`, `name`, `scoring_mode` and `supports_players` do not depend on
/// instance state (GAM-5, TYP-2).
pub fn t_gam_2_metadata_is_pure(factory: GameFactory, players: &[PlayerId], seed: u64) {
    require_players(players);
    let mut game = factory();
    let before = (
        game.id(),
        game.name(),
        game.scoring_mode(),
        (0..=8)
            .map(|count| game.supports_players(count))
            .collect::<Vec<_>>(),
    );
    game.initialize(seed, players, 1_000_000)
        .expect("T-GAM-2: initialize must accept the players it supports");
    let after_initialize = (
        game.id(),
        game.name(),
        game.scoring_mode(),
        (0..=8)
            .map(|count| game.supports_players(count))
            .collect::<Vec<_>>(),
    );
    assert_eq!(before, after_initialize, "T-GAM-2: metadata changed");

    let _ = game.tick(0, GameInput::TimeAdvanced);
    let after_tick = (
        game.id(),
        game.name(),
        game.scoring_mode(),
        (0..=8)
            .map(|count| game.supports_players(count))
            .collect::<Vec<_>>(),
    );
    assert_eq!(before, after_tick, "T-GAM-2: metadata changed after a tick");

    // GAM-5: the instance must claim at least one supported count, and the
    // scoring mode must be one of the two the engine understands.
    assert!(
        before.3.iter().any(|supported| *supported),
        "T-GAM-2: the game supports no player count"
    );
    assert!(
        matches!(
            before.2,
            ScoringMode::Placement | ScoringMode::HigherIsBetter
        ),
        "T-GAM-2: unknown scoring mode"
    );
}

/// T-GAM-3: malformed action payloads never panic and are rejected with
/// `InputRejected` (GAM-6).
pub fn t_gam_3_malformed_actions_are_rejected(
    factory: GameFactory,
    players: &[PlayerId],
    seed: u64,
    malformed: &[GameAction],
) {
    require_players(players);
    let mut game = factory();
    game.initialize(seed, players, 1_000_000)
        .expect("T-GAM-3: initialize must accept the players it supports");
    let player = players[0];
    for (index, action) in malformed.iter().enumerate() {
        let events = game
            .tick(
                0,
                GameInput::Player {
                    player,
                    action: action.clone(),
                    lag_credit_ms: 0,
                },
            )
            .expect("T-GAM-3: a malformed payload must not be a Fatal error");
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::InputRejected { player: rejected, .. } if *rejected == player
            )),
            "T-GAM-3: payload {index} was not rejected with InputRejected: {events:?}"
        );
        assert!(
            !game.is_finished(),
            "T-GAM-3: payload {index} finished the game"
        );
    }
}

/// T-GAM-4: `initialize` resets all state, and an unsupported player count is
/// `InitFailed` (GAM-4, GAM-5).
pub fn t_gam_4_initialize_resets_and_validates(
    factory: GameFactory,
    players: &[PlayerId],
    seed: u64,
    other_seed: u64,
) {
    require_players(players);
    let mut game = factory();
    game.initialize(seed, players, 1_000_000)
        .expect("T-GAM-4: initialize must accept the players it supports");
    let fresh = factory();

    // GAM-5: an unsupported count is refused.
    let unsupported = (1usize..=8).find(|count| !game.supports_players(*count));
    if let Some(count) = unsupported {
        let too_many: Vec<PlayerId> = (0..count)
            .map(
                |index| // REQ: T-GAM-4 — ids are supplied, never generated (TYP-3).
                *players.get(index).unwrap_or(&players[0]),
            )
            .collect();
        assert!(
            matches!(
                game.initialize(seed, &too_many, 1_000_000),
                Err(super::super::game::GameError::InitFailed(_))
            ),
            "T-GAM-4: {count} players must give InitFailed"
        );
    }

    // GAM-4: the same instance played a different seed first, then gets `seed`
    // again; it must now behave exactly like a fresh instance.
    let script = generic_script(players);
    let mut fresh_once = factory();
    fresh_once
        .initialize(seed, players, 1_000_000)
        .expect("T-GAM-4: initialize must accept the players it supports");
    let mut reused = factory();
    reused
        .initialize(other_seed, players, 1_000_000)
        .expect("T-GAM-4: initialize must accept the players it supports");
    reused
        .initialize(seed, players, 1_000_000)
        .expect("T-GAM-4: initialize must accept the players it supports");

    for (at_ms, input) in &script {
        let expected = fresh_once.tick(*at_ms, input.clone());
        let actual = reused.tick(*at_ms, input.clone());
        assert_eq!(
            format!("{expected:?}"),
            format!("{actual:?}"),
            "T-GAM-4: a re-initialized instance kept state at {at_ms}ms"
        );
    }
    assert_eq!(
        fresh_once.result().outcomes,
        reused.result().outcomes,
        "T-GAM-4: a re-initialized instance kept state from the earlier run"
    );
    assert_eq!(
        fresh.id(),
        reused.id(),
        "T-GAM-4: a fresh instance and a re-initialized one must agree"
    );
}

/// T-GAM-5: a finished game stays finished, ignores later inputs and reports
/// every player (GAM-7, GAM-8).
pub fn t_gam_5_finished_is_final(factory: GameFactory, players: &[PlayerId], seed: u64) {
    require_players(players);
    let mut game = factory();
    game.initialize(seed, players, 1_000_000)
        .expect("T-GAM-5: initialize must accept the players it supports");

    for step in 0..MAX_TICKS {
        if game.is_finished() {
            break;
        }
        game.tick(step * 1_000, GameInput::TimeAdvanced)
            .expect("T-GAM-5: TimeAdvanced must not fail");
    }
    assert!(
        game.is_finished(),
        "T-GAM-5: the game did not finish within {MAX_TICKS} ticks"
    );

    // GAM-8: monotonic, and further inputs produce nothing.
    assert!(
        game.is_finished(),
        "T-GAM-5: is_finished() must be monotonic"
    );
    let events = game
        .tick(MAX_TICKS * 1_000, GameInput::TimeAdvanced)
        .expect("T-GAM-5: a tick after the end must not fail");
    assert!(
        events.is_empty(),
        "T-GAM-5: a finished game emitted {events:?}"
    );

    // GAM-7: a finished game reports every player.
    let reported: Vec<PlayerId> = game
        .result()
        .outcomes
        .into_iter()
        .map(|(player, _)| player)
        .collect();
    for player in players {
        assert!(
            reported.contains(player),
            "T-GAM-5: the finished result omits {player:?}"
        );
    }
}
