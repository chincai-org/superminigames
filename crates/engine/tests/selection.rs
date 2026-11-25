//! T2.1: the game registry, the per-round pool and deterministic selection
//! (TYP-2, PLL-1..3, RNG-3 to RNG-6, RNG-10, RNG-11).
//!
//! These tests use a minimal fake [`Game`]; the scripted fakes and the full
//! engine harness belong to T2.4. T-RNG-4 is covered here at selection level:
//! T2.5 owns the retry loop.

use std::sync::atomic::{AtomicUsize, Ordering};

use superminigames_core::config::GameFilterMode;
use superminigames_core::error::ConfigError;
use superminigames_core::game::{
    Game, GameError, GameEvent, GameFactory, GameInput, GameResult, ScoringMode, TimeModel,
};
use superminigames_core::ids::{GameId, PlayerId};
use superminigames_engine::registry::{GameRegistry, RegisteredGame};
use superminigames_engine::selection::{first_mover_seat, game_seed, opening_bit, pick};

/// A minimal game: it only reports metadata and player support. `supports` is
/// the exact player count it accepts (PLL-1).
struct FakeGame {
    id: GameId,
    name: &'static str,
    supports: usize,
    scoring_mode: ScoringMode,
}

impl FakeGame {
    fn new(id: &'static str, supports: usize, scoring_mode: ScoringMode) -> Self {
        Self {
            id: GameId(id.to_owned()),
            name: id,
            supports,
            scoring_mode,
        }
    }
}

impl Game for FakeGame {
    fn id(&self) -> GameId {
        self.id.clone()
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn scoring_mode(&self) -> ScoringMode {
        self.scoring_mode
    }

    fn time_model(&self) -> TimeModel {
        TimeModel::RoundDeadlineOnly
    }

    fn supports_players(&self, count: usize) -> bool {
        count == self.supports
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
        false
    }

    fn result(&self) -> GameResult {
        GameResult {
            outcomes: Vec::new(),
        }
    }
}

/// A game that counts `initialize`/`tick` calls, so a test can prove the
/// registry keeps its prototype out of play (TYP-2, T-POOL-3).
///
/// Only one test touches this static, so parallel test execution cannot race it.
static LIFECYCLE_CALLS: AtomicUsize = AtomicUsize::new(0);

struct LifecycleGame;

impl Game for LifecycleGame {
    fn id(&self) -> GameId {
        GameId("L".to_owned())
    }

    fn name(&self) -> &'static str {
        "lifecycle"
    }

    fn scoring_mode(&self) -> ScoringMode {
        ScoringMode::Placement
    }

    fn time_model(&self) -> TimeModel {
        TimeModel::RoundDeadlineOnly
    }

    fn supports_players(&self, count: usize) -> bool {
        count == 2
    }

    fn initialize(
        &mut self,
        _seed: u64,
        _players: &[PlayerId],
        _time_limit_ms: u64,
    ) -> Result<(), GameError> {
        LIFECYCLE_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn tick(&mut self, _at_ms: u64, _input: GameInput) -> Result<Vec<GameEvent>, GameError> {
        LIFECYCLE_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }

    fn next_wakeup_ms(&self) -> Option<u64> {
        None
    }

    fn is_finished(&self) -> bool {
        false
    }

    fn result(&self) -> GameResult {
        GameResult {
            outcomes: Vec::new(),
        }
    }
}

macro_rules! placement_factory {
    ($name:ident, $id:literal, $supports:literal) => {
        fn $name() -> Box<dyn Game> {
            Box::new(FakeGame::new($id, $supports, ScoringMode::Placement))
        }
    };
}

placement_factory!(game_a, "A", 2);
placement_factory!(game_b, "B", 2);
placement_factory!(game_c, "C", 2);
placement_factory!(game_d, "D", 2);
placement_factory!(game_e, "E", 2);
placement_factory!(two_player_x, "X", 2);
placement_factory!(three_player_y, "Y", 3);

fn lifecycle_factory() -> Box<dyn Game> {
    Box::new(LifecycleGame)
}

/// A game that reports `HigherIsBetter`, to check the cached scoring mode.
fn higher_is_better_factory() -> Box<dyn Game> {
    Box::new(FakeGame::new("H", 2, ScoringMode::HigherIsBetter))
}

/// The `[A, B, C, D, E]` registry used by the selection tests.
const ALL_FACTORIES: [GameFactory; 5] = [game_a, game_b, game_c, game_d, game_e];

/// A registry holding `factories` in the given order.
fn registry_from(factories: &[GameFactory]) -> GameRegistry {
    let mut registry = GameRegistry::new();
    for factory in factories {
        registry.register(*factory).expect("register");
    }
    registry
}

/// No exclusions; keeps the whole registry.
fn no_filter() -> GameFilterMode {
    GameFilterMode::BanList(Vec::new())
}

/// The ids of a pool, in pool order.
fn pool_ids<'a>(pool: &'a [&'a RegisteredGame]) -> Vec<&'a str> {
    pool.iter().map(|game| game.id().0.as_str()).collect()
}

#[test]
fn t_pool_3_duplicate_game_id_is_rejected() {
    // REQ: TYP-2, T-POOL-3
    let mut registry = GameRegistry::new();
    registry.register(game_a).expect("first registration");
    assert_eq!(
        registry.register(game_a),
        Err(ConfigError::DuplicateGameId(GameId("A".to_owned())))
    );
}

#[test]
fn t_pool_3_registry_and_pool_are_sorted_by_game_id() {
    // REQ: TYP-2, PLL-1, T-POOL-3
    let registry = registry_from(&[game_e, game_a, game_c, game_b, game_d]);
    let registered: Vec<&str> = registry
        .games()
        .iter()
        .map(|game| game.id().0.as_str())
        .collect();
    assert_eq!(registered, ["A", "B", "C", "D", "E"]);

    let pool = registry.pool(&no_filter(), 2);
    assert_eq!(pool_ids(&pool), ["A", "B", "C", "D", "E"]);
}

#[test]
fn t_pool_3_prototype_is_never_initialized_or_ticked() {
    // REQ: TYP-2, PLL-1, T-POOL-3
    LIFECYCLE_CALLS.store(0, Ordering::SeqCst);
    let mut registry = GameRegistry::new();
    registry.register(lifecycle_factory).expect("register");

    let pool = registry.pool(&no_filter(), 2);
    assert_eq!(pool.len(), 1, "the prototype answers supports_players");
    let _ = registry.get(&GameId("L".to_owned())).expect("registered");
    assert_eq!(
        LIFECYCLE_CALLS.load(Ordering::SeqCst),
        0,
        "the prototype must not be initialized or ticked"
    );

    // create() makes a fresh instance from the factory (RND-12).
    let mut fresh = registry
        .get(&GameId("L".to_owned()))
        .expect("registered")
        .create();
    fresh.initialize(7, &[], 1_000).expect("initialize");
    fresh.tick(0, GameInput::TimeAdvanced).expect("tick");
    assert_eq!(LIFECYCLE_CALLS.load(Ordering::SeqCst), 2);
}

#[test]
fn t_typ_2_register_caches_the_prototype_metadata() {
    // REQ: TYP-2
    let mut registry = GameRegistry::new();
    registry
        .register(higher_is_better_factory)
        .expect("register");
    let game = registry.get(&GameId("H".to_owned())).expect("registered");
    assert_eq!(game.id().0, "H");
    assert_eq!(game.name(), "H");
    assert_eq!(game.scoring_mode(), ScoringMode::HigherIsBetter);
}

#[test]
fn t_pool_1_allow_list_keeps_only_listed_games() {
    // REQ: PLL-1, T-POOL-1
    let registry = registry_from(&ALL_FACTORIES);
    let filter = GameFilterMode::AllowList(vec![GameId("A".to_owned()), GameId("C".to_owned())]);
    let pool = registry.pool(&filter, 2);
    assert_eq!(pool_ids(&pool), ["A", "C"]);
    for round in 0..=99u64 {
        let picked = pick(&pool, 0, round).expect("non-empty pool");
        assert!(
            matches!(picked.game.id().0.as_str(), "A" | "C"),
            "round {round} picked {}",
            picked.game.id()
        );
    }
}

#[test]
fn t_pool_1_ban_list_removes_listed_games() {
    // REQ: PLL-1, T-POOL-1
    let registry = registry_from(&ALL_FACTORIES);
    let filter = GameFilterMode::BanList(vec![GameId("B".to_owned())]);
    let pool = registry.pool(&filter, 2);
    assert_eq!(pool_ids(&pool), ["A", "C", "D", "E"]);
    for round in 0..=99u64 {
        let picked = pick(&pool, 0, round).expect("non-empty pool");
        assert_ne!(picked.game.id().0, "B", "round {round} picked B");
    }
}

#[test]
fn t_pool_2_only_games_supporting_the_participant_count_remain() {
    // REQ: PLL-1, T-POOL-2
    let mut registry = GameRegistry::new();
    registry.register(two_player_x).expect("register X");
    registry.register(three_player_y).expect("register Y");

    let pool = registry.pool(&no_filter(), 2);
    assert_eq!(pool_ids(&pool), ["X"], "only X supports 2 players");
    for round in 0..=99u64 {
        assert_eq!(
            pick(&pool, 0, round).expect("non-empty pool").game.id().0,
            "X"
        );
    }
}

#[test]
fn t_pool_2_no_supported_game_gives_an_empty_pool() {
    // REQ: PLL-1, T-POOL-2
    let mut registry = GameRegistry::new();
    registry.register(three_player_y).expect("register Y");
    let pool = registry.pool(&no_filter(), 2);
    assert!(pool.is_empty());
    assert!(
        pick(&pool, 0, 0).is_none(),
        "an empty pool is Skipped(EmptyPool) at the round level"
    );
}

#[test]
fn t_rng_3_same_games_in_different_order_pick_identically() {
    // REQ: PLL-1, T-RNG-3
    let forward = registry_from(&ALL_FACTORIES);
    let reverse = registry_from(&[game_e, game_d, game_c, game_b, game_a]);
    let forward_pool = forward.pool(&no_filter(), 2);
    let reverse_pool = reverse.pool(&no_filter(), 2);

    for round in 0..=9u64 {
        let left = pick(&forward_pool, 0, round).expect("non-empty pool");
        let right = pick(&reverse_pool, 0, round).expect("non-empty pool");
        assert_eq!(left.game.id(), right.game.id(), "round {round}");
        assert_eq!(left.game_seed, right.game_seed, "round {round}");
    }
}

#[test]
fn t_rng_4_a_pick_and_its_seed_are_pure_functions_of_seed_round_pool() {
    // REQ: RNG-3, RNG-6, T-RNG-4
    let registry = registry_from(&ALL_FACTORIES);
    let pool = registry.pool(&no_filter(), 2);

    for round in 0..=9u64 {
        let first = pick(&pool, 0, round).expect("non-empty pool");
        // Repeated calls stand in for failed or repeated attempts: retries reuse
        // the same game and game seed and never touch the RNG (RNG-6).
        for _ in 0..3 {
            let again = pick(&pool, 0, round).expect("non-empty pool");
            assert_eq!(first.game.id(), again.game.id(), "round {round}");
            assert_eq!(first.game_seed, again.game_seed, "round {round}");
        }
    }

    // A different registry holding the same games picks identically.
    let shuffled = registry_from(&[game_c, game_a, game_e, game_b, game_d]);
    let shuffled_pool = shuffled.pool(&no_filter(), 2);
    for round in 0..=9u64 {
        assert_eq!(
            pick(&pool, 0, round).expect("non-empty pool").game.id(),
            pick(&shuffled_pool, 0, round)
                .expect("non-empty pool")
                .game
                .id(),
            "round {round}"
        );
    }
}

#[test]
fn t_rng_5_round_five_does_not_depend_on_earlier_rounds() {
    // REQ: RNG-3, RNG-4, T-RNG-5
    let registry = registry_from(&ALL_FACTORIES);
    let pool = registry.pool(&no_filter(), 2);
    let expected = pick(&pool, 0, 5).expect("non-empty pool");

    // Rounds 0-4 played, skipped or failed in any way must not change round 5.
    let _played: Vec<u64> = (0..5u64)
        .map(|round| pick(&pool, 0, round).expect("non-empty pool").game_seed)
        .collect();
    let _repeated = pick(&pool, 0, 3).expect("non-empty pool");
    let empty: Vec<&RegisteredGame> = Vec::new();
    assert!(
        pick(&empty, 0, 2).is_none(),
        "a skipped round makes no draw"
    );

    let again = pick(&pool, 0, 5).expect("non-empty pool");
    assert_eq!(expected.game.id(), again.game.id());
    assert_eq!(expected.game_seed, again.game_seed);
}

/// The `key=value` rows of an embedded golden file, comments and blank lines
/// dropped (RNG-9). The engine checks itself against the same committed files
/// that `superminigames-core` owns, so a wrong stream or bit fails here too.
fn golden_rows(text: &str) -> Vec<(&str, &str)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            line.split_once('=')
                .unwrap_or_else(|| panic!("RNG-9: golden row is not key=value: {line}"))
        })
        .collect()
}

#[test]
fn t_rng_10_game_seed_matches_the_committed_golden_values() {
    // REQ: RNG-10
    let mut checked = 0;
    for (key, expected) in golden_rows(include_str!("../../core/tests/golden_game_seed0.txt")) {
        let Some(index) = key.strip_prefix("round_") else {
            continue;
        };
        let round: u64 = index
            .parse()
            .expect("RNG-9: a round_ key holds a decimal index");
        assert_eq!(
            game_seed(0, round).to_string(),
            expected,
            "T-RNG-6: seed 0 round {round}"
        );
        checked += 1;
    }
    assert_eq!(checked, 5, "T-RNG-6: the file holds rounds 0..=4");
}

#[test]
fn t_rng_11_opening_bit_matches_the_committed_golden_value() {
    // REQ: RNG-11
    let committed = golden_rows(include_str!("../../core/tests/golden_seed0.txt"))
        .into_iter()
        .find(|(key, _)| *key == "opening_bit")
        .map(|(_, value)| value)
        .expect("T-RNG-7: golden_seed0.txt holds an opening_bit row");

    let opening = opening_bit(0);
    assert_eq!(
        opening.to_string(),
        committed,
        "T-RNG-7: the opening bit must come from chacha_from_seed(0, 1 << 63)"
    );
    assert!(opening <= 1, "the opening bit is 0 or 1");
    let first = first_mover_seat(opening, 0);
    assert_eq!(first, usize::from(opening), "the first game uses the bit");
    assert_eq!(first_mover_seat(opening, 1), 1 - first);
    assert_eq!(first_mover_seat(opening, 2), first);

    // The alternation has period 2 for every seed, not just seed 0.
    for seed in [1u64, 42, u64::MAX] {
        let opening = opening_bit(seed);
        let first = first_mover_seat(opening, 0);
        assert_eq!(first, usize::from(opening), "seed {seed}");
        assert_eq!(first_mover_seat(opening, 1), 1 - first, "seed {seed}");
        assert_eq!(first_mover_seat(opening, 2), first, "seed {seed}");
    }
}
