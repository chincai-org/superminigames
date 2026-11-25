//! B1 match-slot integration tests (MTH-1..MTH-3, PRS-10, TMR-4).
#[path = "support/mod.rs"]
mod support;

use superminigames_core::config::{GameFilterMode, MatchConfig};
use superminigames_core::game::{Game, GameAction, GameResult, ScoringMode, TimeModel};
use superminigames_core::ids::{Player, PlayerId, Visibility};
use superminigames_core::outcome::{DisqualificationReason, PlayerOutcome};
use superminigames_engine::actor::{Command, EngineEvent, EngineInit, EngineOptions, spawn_engine};
use superminigames_engine::error::{EngineError, InputError, LobbyError};
use superminigames_engine::registry::GameRegistry;
use superminigames_engine::report::{MatchReport, PlayerStatus, RoundStatus, SkipReason, Standing};
use superminigames_engine::selection::{first_mover_seat, game_seed, opening_bit};
use support::{FakeGame, player};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::dispatcher::{Dispatch, set_default};
use tracing::subscriber::NoSubscriber;
fn game() -> Box<dyn Game> {
    Box::new(
        FakeGame::builder("test", "Test")
            .finish_after_ticks(1)
            .build(),
    )
}
fn player_value(id: u64) -> Player {
    Player {
        id: player(id),
        name: format!("P{id}"),
    }
}
/// Section 16's worked example as one scripted placement game (T-MTH-2).
///
/// The pool holds one game id, so every round runs this fake; the round's game
/// seed (RNG-10) selects the scripted outcome. Section 16's four outcomes are a
/// placement win, a draw (SCR-6 splits the points), a win for the player who is
/// not the first mover, and a leave (RND-9 records `NoResult(LeftRound)`).
///
/// The match seed puts player 1 (A) at the opening bit, so A is the first mover
/// of rounds 0 and 2 and the scripted rounds match the example's narrative.
const SECTION_SIXTEEN_SEED: u64 = 11;

fn section_sixteen_game() -> Box<dyn Game> {
    let (a, b) = (player(1), player(2));
    let opening = opening_bit(SECTION_SIXTEEN_SEED);
    let mut built = FakeGame::builder("test", "Test").finish_after_ticks(1);
    for round in 0..4usize {
        // RND-11: the participants are rotated so the first mover is seat 0.
        let mover = if first_mover_seat(opening, round) == 0 {
            a
        } else {
            b
        };
        let other = if mover == a { b } else { a };
        let outcomes = match round {
            // Round 0: the first mover wins.
            0 => vec![
                (mover, PlayerOutcome::Placed(1)),
                (other, PlayerOutcome::Placed(2)),
            ],
            // Round 1: a draw; both take place 1 and split the points.
            1 => vec![(a, PlayerOutcome::Placed(1)), (b, PlayerOutcome::Placed(1))],
            // Round 2: the player who is not the first mover wins.
            2 => vec![
                (other, PlayerOutcome::Placed(1)),
                (mover, PlayerOutcome::Placed(2)),
            ],
            // Round 3: player 1 wins and player 2 left mid-round, so the game
            // reports the leaver itself (GME-8, RND-9).
            _ => vec![
                (a, PlayerOutcome::Placed(1)),
                (
                    b,
                    PlayerOutcome::NoResult(DisqualificationReason::LeftRound),
                ),
            ],
        };
        built = built.result_for_seed(
            game_seed(SECTION_SIXTEEN_SEED, round as u64),
            GameResult { outcomes },
        );
    }
    Box::new(built.build())
}

/// The section 16 example's config: 4 rounds and a round limit longer than the
/// reconnect grace, so round 3's leave is decided by the grace (T-MTH-2).
fn section_sixteen_config() -> MatchConfig {
    MatchConfig {
        rounds: 4,
        round_time_limit_secs: 60,
        seed: SECTION_SIXTEEN_SEED,
        max_absent_round_starts: 3,
        filter: GameFilterMode::BanList(Vec::new()),
    }
}
fn make_config(rounds: u32, max_absent_round_starts: u32) -> MatchConfig {
    MatchConfig {
        rounds,
        round_time_limit_secs: 1,
        seed: 5,
        max_absent_round_starts,
        filter: GameFilterMode::BanList(Vec::new()),
    }
}
fn spawn(
    rounds: u32,
    absent: u32,
) -> (
    superminigames_engine::actor::EngineHandle,
    mpsc::Receiver<EngineEvent>,
) {
    let mut registry = GameRegistry::new();
    registry.register(game).expect("unique fake game");
    spawn_with_registry(registry, make_config(rounds, absent))
}
fn spawn_with_registry(
    registry: GameRegistry,
    config: MatchConfig,
) -> (
    superminigames_engine::actor::EngineHandle,
    mpsc::Receiver<EngineEvent>,
) {
    let mut handle = spawn_engine(EngineInit {
        lobby_id: serde_json::from_str("\"00000000000000000000000000000007\"").expect("id"),
        match_id: serde_json::from_str("\"00000000000000000000000000000008\"").expect("id"),
        host: player_value(1),
        config,
        visibility: Visibility::Public,
        registry,
        options: EngineOptions {
            record_replays: true,
            inbox_capacity: 256,
        },
    })
    .expect("valid actor");
    let events = handle.subscribe();
    (handle, events)
}
async fn ready_and_start(handle: &superminigames_engine::actor::EngineHandle) {
    handle
        .send(Command::Join {
            player: player_value(2),
            code: None,
        })
        .await
        .expect("join");
    for id in [1, 2] {
        handle
            .send(Command::SetReady {
                player: player(id),
                ready: true,
            })
            .await
            .expect("ready");
    }
    handle
        .send(Command::StartMatch {
            player: player(1),
            seed: 5,
        })
        .await
        .expect("start");
    tokio::task::yield_now().await;
}
#[tokio::test(start_paused = true)]
async fn t_arc_2_emitted_round_records_equal_match_report() {
    // REQ: ARC-3, T-ARC-2
    let (handle, mut events) = spawn(2, 3);
    ready_and_start(&handle).await;
    let result = complete_report(&handle, &mut events).await;
    assert_eq!(result.rounds.len(), 2);
    assert_eq!(
        result
            .rounds
            .iter()
            .map(|r| r.round_index)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert!(
        result
            .rounds
            .iter()
            .all(|round| round.placements.len() == 2)
    );
}

#[tokio::test(start_paused = true)]
async fn t_arc_2_the_report_is_rebuilt_from_the_events() {
    // REQ: ARC-3, T-ARC-2
    // Round 0 is played, round 1 is skipped and player 2 is Removed at round
    // 2's start, so every pick, skip, removal and point is exercised.
    let (handle, mut events) = spawn(3, 2);
    ready_and_start(&handle).await;
    let mut starts = Vec::new();
    let mut finished = Vec::new();
    let mut removed = Vec::new();
    let report = loop {
        match events.recv().await {
            Some(EngineEvent::RoundStarting {
                round_index,
                game,
                participants,
                ..
            }) => {
                starts.push((round_index, game, participants.clone()));
                if round_index == 0 {
                    handle
                        .send(Command::Input {
                            player: participants[0],
                            action: GameAction(Vec::new()),
                            lag_credit_ms: 0,
                            received_at: Instant::now(),
                        })
                        .await
                        .expect("finish round 0");
                }
            }
            Some(EngineEvent::RoundFinished(record)) => {
                if record.round_index == 0 {
                    handle
                        .send(Command::Connection {
                            player: player(2),
                            connected: false,
                        })
                        .await
                        .expect("away from round 1");
                }
                finished.push(record);
            }
            Some(EngineEvent::PlayerStatus {
                player: flagged,
                status: PlayerStatus::Removed,
            }) => removed.push(flagged),
            Some(EngineEvent::MatchFinished(report)) => break report,
            Some(_) => {}
            None => panic!("event channel closed before the report"),
        }
    };

    // The round records in the events are the report's rounds, verbatim.
    assert_eq!(finished, report.rounds);
    // Each pick and its participant list come from the `RoundStarting` event.
    for (round_index, game, participants) in &starts {
        let record = &report.rounds[*round_index as usize];
        assert_eq!(record.game.as_ref(), Some(game));
        assert_eq!(&record.participants, participants);
    }
    // The points in those records add up to the final ranking.
    for entry in &report.final_ranking {
        let rebuilt: u32 = finished
            .iter()
            .flat_map(|record| record.placements.iter())
            .filter(|(player, _, _)| *player == entry.player)
            .map(|(_, _, points)| *points)
            .sum();
        assert_eq!(rebuilt, entry.total_points);
    }
    // The removal at round 2's start is recoverable too (PRS-3).
    assert_eq!(removed, vec![player(2)]);
    assert!(
        report
            .final_ranking
            .iter()
            .any(|entry| entry.player == player(2) && entry.standing == Standing::Removed)
    );
    assert!(report.ended_early);
}

#[tokio::test(start_paused = true)]
async fn t_arc_3_forbidden_transitions_are_typed_errors() {
    // REQ: ARC-5, T-ARC-3
    let (handle, mut events) = spawn(1, 3);
    ready_and_start(&handle).await;
    let participants = round_participants(&mut events).await;
    // Section 12.1 has no `InRound` -> `Starting` edge, so a second `StartMatch`
    // is refused with a typed error instead of panicking.
    let refused = handle
        .send(Command::StartMatch {
            player: player(1),
            seed: 5,
        })
        .await
        .expect_err("StartMatch is not valid while a round runs");
    assert_eq!(refused, EngineError::Lobby(LobbyError::WrongState));
    // An in-round command from a non-participant is refused by the round (RND-3).
    let refused = handle
        .send(Command::Input {
            player: player(9),
            action: GameAction(Vec::new()),
            lag_credit_ms: 0,
            received_at: Instant::now(),
        })
        .await
        .expect_err("a non-participant input is refused");
    assert_eq!(refused, EngineError::Input(InputError::NotParticipant));
    // Both refusals were harmless: the round still runs to its scripted end.
    handle
        .send(Command::Input {
            player: participants[0],
            action: GameAction(Vec::new()),
            lag_credit_ms: 0,
            received_at: Instant::now(),
        })
        .await
        .expect("the round is still live");
    let result = report(&mut events).await;
    assert_eq!(result.rounds.len(), 1);
    // `MatchFinished` is terminal, so a later command is a typed error too.
    let refused = handle
        .send(Command::Connection {
            player: player(2),
            connected: false,
        })
        .await
        .expect_err("the match is over");
    assert_eq!(refused, EngineError::Lobby(LobbyError::WrongState));
}

#[tokio::test(start_paused = true)]
async fn t_arc_4_tracing_does_not_change_the_match_report() {
    // REQ: ARC-7, T-ARC-4
    async fn run_a_match() -> MatchReport {
        let (handle, mut events) = spawn(2, 3);
        ready_and_start(&handle).await;
        complete_report(&handle, &mut events).await
    }

    let without_subscriber = run_a_match().await;
    let dispatch = Dispatch::new(NoSubscriber::new());
    let guard = set_default(&dispatch);
    let with_subscriber = run_a_match().await;
    drop(guard);
    assert_eq!(with_subscriber, without_subscriber);
}

#[tokio::test(start_paused = true)]
async fn t_mth_3_first_mover_advances_only_when_game_starts() {
    // REQ: RNG-11, RND-11, T-MTH-3
    // Round 0 starts, round 1 is skipped (player 2 is away) and round 2 starts
    // again: the skipped round must not consume an alternation step.
    let (handle, mut events) = spawn(3, 3);
    ready_and_start(&handle).await;
    // RND-11: `RoundStarting.participants` is seat order, so the first mover is
    // read from the replay, whose players are in first-mover order.
    let mut first_movers = Vec::new();
    let mut skipped = Vec::new();
    while first_movers.len() < 2 {
        match events.recv().await {
            Some(EngineEvent::RoundStarting { participants, .. }) => {
                handle
                    .send(Command::Input {
                        player: participants[0],
                        action: GameAction(Vec::new()),
                        lag_credit_ms: 0,
                        received_at: Instant::now(),
                    })
                    .await
                    .expect("complete game");
            }
            Some(EngineEvent::RoundFinished(record)) => {
                if record.status == RoundStatus::Skipped(SkipReason::NotEnoughPlayers) {
                    skipped.push(record.round_index);
                    // (Re)connect for the next start.
                    handle
                        .send(Command::Connection {
                            player: player(2),
                            connected: true,
                        })
                        .await
                        .expect("reconnect");
                    continue;
                }
                let replay = record.replay.as_ref().expect("record_replays is on");
                first_movers.push(replay.players[0]);
                if first_movers.len() == 1 {
                    handle
                        .send(Command::Connection {
                            player: player(2),
                            connected: false,
                        })
                        .await
                        .expect("away for round 1");
                }
            }
            Some(_) => {}
            None => break,
        }
    }
    let expected_first = if opening_bit(5) == 0 {
        player(1)
    } else {
        player(2)
    };
    assert_eq!(
        first_movers[0], expected_first,
        "the first game starts at the opening bit"
    );
    let expected_second = if expected_first == player(1) {
        player(2)
    } else {
        player(1)
    };
    assert_eq!(
        first_movers[1], expected_second,
        "the role alternates per started game"
    );
    assert_eq!(
        skipped,
        vec![1],
        "the skipped round must not consume an alternation step"
    );
}

#[tokio::test(start_paused = true)]
async fn t_mth_2_section_sixteen_totals_and_ranking() {
    // REQ: MTH-2, MTH-3, SCR-6, FIN-1, RND-9, T-MTH-2
    let mut registry = GameRegistry::new();
    registry
        .register(section_sixteen_game)
        .expect("unique fake game");
    let (handle, mut events) = spawn_with_registry(registry, section_sixteen_config());
    // Start with the section 16 seed (`ready_and_start` uses seed 5).
    handle
        .send(Command::Join {
            player: player_value(2),
            code: None,
        })
        .await
        .expect("join");
    for id in [1, 2] {
        handle
            .send(Command::SetReady {
                player: player(id),
                ready: true,
            })
            .await
            .expect("ready");
    }
    handle
        .send(Command::StartMatch {
            player: player(1),
            seed: SECTION_SIXTEEN_SEED,
        })
        .await
        .expect("start");
    tokio::task::yield_now().await;
    let mut result = None;
    while result.is_none() {
        match events.recv().await {
            Some(EngineEvent::RoundStarting {
                round_index,
                participants,
                ..
            }) => {
                if round_index < 3 {
                    handle
                        .send(Command::Input {
                            player: participants[0],
                            action: GameAction(Vec::new()),
                            lag_credit_ms: 0,
                            received_at: Instant::now(),
                        })
                        .await
                        .expect("finish the round");
                } else {
                    // Section 16, round 3: player 2 leaves and does not return,
                    // so the grace expires 30s into the round.
                    handle
                        .send(Command::Connection {
                            player: player(2),
                            connected: false,
                        })
                        .await
                        .expect("leave round 3");
                    tokio::time::advance(std::time::Duration::from_secs(30)).await;
                }
            }
            Some(EngineEvent::MatchFinished(report)) => result = Some(report),
            Some(_) => {}
            None => panic!("event channel closed before the report"),
        }
    }
    let result = result.expect("report");

    // Section 16: four slots, A 30 (rank 1), B 18 (rank 2).
    assert_eq!(result.rounds.len(), 4);
    // Section 16's per-round progression: A 12, 6, 0, 12 and B 0, 6, 12, 0.
    let points: Vec<[u32; 2]> = result
        .rounds
        .iter()
        .map(|record| {
            let mut per_player = [0; 2];
            for (scorer, _, scored) in &record.placements {
                per_player[usize::from(*scorer == player(2))] = *scored;
            }
            per_player
        })
        .collect();
    assert_eq!(points, [[12, 0], [6, 6], [0, 12], [12, 0]]);
    // RND-9: the scripted game reports the leaver as `NoResult(LeftRound)`
    // (GME-8), so round 3 is valid and player 2 is disqualified for it.
    assert_eq!(
        result.rounds[3].round_disqualified,
        vec![(player(2), DisqualificationReason::LeftRound)]
    );
    assert!(!result.ended_early);
    assert_eq!(result.final_ranking[0].player, player(1));
    assert_eq!(result.final_ranking[0].total_points, 30);
    assert_eq!(result.final_ranking[0].rank, 1);
    assert_eq!(result.final_ranking[1].player, player(2));
    assert_eq!(result.final_ranking[1].total_points, 18);
    assert_eq!(result.final_ranking[1].rank, 2);
}

#[tokio::test(start_paused = true)]
async fn t_mth_2_section_sixteen_higher_is_better_scores_at_deadline() {
    // REQ: MTH-2, SCR-1, SCR-3, SCR-6, T-MTH-2
    // Section 16 round 2 is a RoundDeadlineOnly HigherIsBetter game: A scores
    // 4, B scores 9, and the deadline chooses B as first place. Keep this as a
    // separate test so the example's outcome mode and deadline path are explicit.
    let mut registry = GameRegistry::new();
    registry
        .register(scored_deadline_game)
        .expect("unique fake game");
    let (handle, mut events) = spawn_with_registry(
        registry,
        MatchConfig {
            seed: SECTION_SIXTEEN_SEED,
            ..make_config(1, 3)
        },
    );
    handle
        .send(Command::Join {
            player: player_value(2),
            code: None,
        })
        .await
        .expect("join");
    for id in [1, 2] {
        handle
            .send(Command::SetReady {
                player: player(id),
                ready: true,
            })
            .await
            .expect("ready");
    }
    handle
        .send(Command::StartMatch {
            player: player(1),
            seed: SECTION_SIXTEEN_SEED,
        })
        .await
        .expect("start");
    tokio::task::yield_now().await;

    let record = loop {
        match events.recv().await {
            Some(EngineEvent::RoundStarting { .. }) => {
                tokio::time::advance(std::time::Duration::from_secs(1)).await;
            }
            Some(EngineEvent::RoundFinished(record)) => break record,
            Some(_) => {}
            None => panic!("event channel closed before RoundFinished"),
        }
    };
    assert_eq!(record.round_index, 0);
    assert_eq!(record.status, RoundStatus::Scored);
    assert_eq!(
        record.placements,
        vec![(player(1), 2, 0), (player(2), 1, 12)],
        "HigherIsBetter: B's 9 beats A's 4 at the deadline"
    );
    let report = report(&mut events).await;
    assert_eq!(report.rounds, vec![record]);
    // FIN-1/T-SCR-9: the ranking lists every seated player in seat order, and the
    // rank comes from the totals, so B holds rank 1 despite sitting at index 1.
    let ranked: Vec<(PlayerId, u32, u32)> = report
        .final_ranking
        .iter()
        .map(|entry| (entry.player, entry.rank, entry.total_points))
        .collect();
    assert_eq!(
        ranked,
        vec![(player(1), 2, 0), (player(2), 1, 12)],
        "SCR-1/SCR-3: the higher score is place 1 and earns the round's 12 points"
    );
}

fn scored_deadline_game() -> Box<dyn Game> {
    Box::new(
        FakeGame::builder("scored-deadline", "Scored deadline")
            .scoring_mode(ScoringMode::HigherIsBetter)
            .time_model(TimeModel::RoundDeadlineOnly)
            // Scores index the game participant order; seed 11 makes seat A the first mover here.
            .scored(vec![4, 9])
            .build(),
    )
}

#[tokio::test(start_paused = true)]
async fn t_mth_1_five_slots_keep_indices_when_two_are_skipped() {
    // REQ: MTH-1, T-MTH-1
    let (handle, mut events) = spawn(5, 10);
    ready_and_start(&handle).await;
    handle
        .send(Command::Connection {
            player: player(2),
            connected: false,
        })
        .await
        .expect("disconnect");
    let mut records = Vec::new();
    while records.len() < 5 {
        match events.recv().await {
            Some(EngineEvent::RoundFinished(record)) => {
                if record.round_index == 1 {
                    handle
                        .send(Command::Connection {
                            player: player(2),
                            connected: true,
                        })
                        .await
                        .expect("reconnect");
                }
                records.push(record);
            }
            Some(EngineEvent::RoundStarting { participants, .. }) => {
                handle
                    .send(Command::Input {
                        player: participants[0],
                        action: GameAction(Vec::new()),
                        lag_credit_ms: 0,
                        received_at: Instant::now(),
                    })
                    .await
                    .expect("finish round");
            }
            Some(_) => {}
            None => break,
        }
    }
    let result = report(&mut events).await;
    assert_eq!(result.rounds.len(), 5);
    assert_eq!(
        result
            .rounds
            .iter()
            .map(|r| r.round_index)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4]
    );
    assert!(
        result.rounds[..2]
            .iter()
            .all(|r| r.status == RoundStatus::Skipped(SkipReason::NotEnoughPlayers))
    );
    assert!(
        result.rounds[2..]
            .iter()
            .all(|r| r.status == RoundStatus::Scored)
    );
}

#[tokio::test(start_paused = true)]
async fn t_prs_7_deadline_timeout_does_not_change_absence_streak() {
    // REQ: PRS-2, PRS-8, PRS-9, TMR-5, T-PRS-7
    // K = 2. Round 0 times out with both players present, so its start records
    // no absence. Player 2 then goes away: the streak must start at 1, so round
    // 1 is only skipped and the removal waits for round 2.
    let (handle, mut events) = spawn(3, 2);
    ready_and_start(&handle).await;
    let mut finished = Vec::new();
    let result = loop {
        match events.recv().await {
            Some(EngineEvent::RoundStarting { round_index: 0, .. }) => {
                // The 1s round limit passes with no input: SCR-5 skips round 0
                // and PRS-8 keeps this timeout out of the streak.
                tokio::time::advance(std::time::Duration::from_secs(1)).await;
            }
            Some(EngineEvent::RoundFinished(record)) => {
                finished.push(record.status);
                if finished.len() == 1 {
                    handle
                        .send(Command::Connection {
                            player: player(2),
                            connected: false,
                        })
                        .await
                        .expect("away from round 1");
                }
            }
            Some(EngineEvent::MatchFinished(report)) => break report,
            Some(_) => {}
            None => panic!("event channel closed before the report"),
        }
    };
    assert_eq!(finished[0], RoundStatus::Skipped(SkipReason::NoResults));
    // The timeout must not have counted as an absence: the removal only fires
    // at round 2's start, so round 1 still ran.
    assert_eq!(finished.len(), 2);
    assert_eq!(
        finished[1],
        RoundStatus::Skipped(SkipReason::NotEnoughPlayers)
    );
    assert_eq!(result.rounds.len(), 2);
    assert!(result.ended_early);
    assert_eq!(result.final_ranking[1].standing, Standing::Removed);
}

#[tokio::test(start_paused = true)]
async fn t_prs_6_one_connected_skips_and_advances_absence_streak() {
    // REQ: RND-3, PRS-2, PRS-3, T-PRS-6
    // K = 2. Player 2 is away from the very first presence snapshot, so round 0
    // is skipped and the streak reaches 1; at round 1's start it reaches 2 and
    // player 2 is Removed, ending the match early.
    let (handle, mut events) = spawn(4, 2);
    ready_and_start(&handle).await;
    // Force the second player absent before the slot presence snapshot.
    handle
        .send(Command::Connection {
            player: player(2),
            connected: false,
        })
        .await
        .expect("disconnect");
    let result = report(&mut events).await;
    assert_eq!(result.rounds.len(), 1);
    assert_eq!(
        result.rounds[0].status,
        RoundStatus::Skipped(SkipReason::NotEnoughPlayers)
    );
    assert!(result.rounds[0].placements.is_empty());
    assert_eq!(result.final_ranking[1].total_points, 0);
    // The skipped start advanced the streak to 1, so the next start removes
    // player 2 (PRS-2, PRS-3).
    assert!(result.ended_early);
    assert_eq!(result.final_ranking[1].standing, Standing::Removed);
}

#[tokio::test(start_paused = true)]
async fn t_prs_5_removal_at_start_finishes_match_early() {
    // REQ: PRS-10, FIN-1, T-PRS-5
    let (handle, mut events) = spawn(4, 1);
    ready_and_start(&handle).await;
    while !matches!(
        events.recv().await,
        Some(EngineEvent::RoundStarting { round_index: 0, .. })
    ) {}
    handle
        .send(Command::Connection {
            player: player(2),
            connected: false,
        })
        .await
        .expect("disconnect");
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    let result = report(&mut events).await;
    assert!(result.ended_early);
    assert_eq!(result.rounds.len(), 1);
    assert_eq!(result.final_ranking[0].player, player(1));
    assert_eq!(result.final_ranking[0].rank, 1);
    assert_eq!(result.final_ranking[1].standing, Standing::Removed);
}

async fn complete_report(
    handle: &superminigames_engine::actor::EngineHandle,
    events: &mut mpsc::Receiver<EngineEvent>,
) -> superminigames_engine::report::MatchReport {
    while let Some(event) = events.recv().await {
        if let EngineEvent::RoundStarting { participants, .. } = &event {
            handle
                .send(Command::Input {
                    player: participants[0],
                    action: GameAction(Vec::new()),
                    lag_credit_ms: 0,
                    received_at: Instant::now(),
                })
                .await
                .expect("input completes scripted fake");
        }
        if let EngineEvent::MatchFinished(report) = event {
            return report;
        }
    }
    panic!("match event channel closed before report")
}

async fn report(
    events: &mut mpsc::Receiver<EngineEvent>,
) -> superminigames_engine::report::MatchReport {
    while let Some(event) = events.recv().await {
        if let EngineEvent::MatchFinished(report) = event {
            return report;
        }
    }
    panic!("match event channel closed before report")
}

/// The blocking-initialize gate consumed by [`blocking_game`] (T-PRS-9). A
/// registry factory is a plain `fn`, so the test parks one gate here before
/// starting the match and the factory reads it back by reference.
static INIT_GATE: std::sync::OnceLock<support::InitGate> = std::sync::OnceLock::new();

/// A fake game whose `initialize` blocks on [`INIT_GATE`] until the test
/// releases it (T-PRS-9).
fn blocking_game() -> Box<dyn Game> {
    let gate = INIT_GATE.get();
    Box::new(
        FakeGame::builder("test", "Test")
            // RND-9: player 1 is the only reported outcome, so a player who left
            // is filled in as `NoResult(LeftRound)`.
            .result(GameResult {
                outcomes: vec![(player(1), PlayerOutcome::Placed(1))],
            })
            .block_init(gate)
            .build(),
    )
}

/// A config whose round limit is longer than the reconnect grace, so the grace
/// expires before the hard deadline (T-PRS-9, TMR-4).
fn prs9_config() -> MatchConfig {
    MatchConfig {
        rounds: 1,
        round_time_limit_secs: 60,
        seed: 5,
        max_absent_round_starts: 3,
        filter: GameFilterMode::BanList(Vec::new()),
    }
}

#[tokio::test(start_paused = true)]
async fn t_prs_9_disconnect_during_initialize_starts_grace_at_zero() {
    // REQ: T-PRS-9, PRS-6, PRS-7
    // A player disconnects after the presence snapshot but before the spawned
    // `initialize` returns. The actor must still see the command and seed that
    // player's grace at `at_ms = 0` the moment the runner exists.
    let (init_started_tx, mut init_started_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let release = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut registry = GameRegistry::new();
    // `register` calls the factory once for a metadata prototype; arm the gate
    // after that so only the slot's own instance blocks (T-PRS-9).
    registry.register(blocking_game).expect("unique fake game");
    if INIT_GATE
        .set(support::InitGate::new(
            init_started_tx,
            std::sync::Arc::clone(&release),
        ))
        .is_err()
    {
        panic!("INIT_GATE is set once per test process");
    }
    let (handle, mut events) = spawn_with_registry(registry, prs9_config());
    ready_and_start(&handle).await;
    // The slot start runs on a blocking thread and signals here before it
    // returns, so the test can act while `initialize` is still in flight.
    init_started_rx.recv().await.expect("initialize started");
    handle
        .send(Command::Connection {
            player: player(2),
            connected: false,
        })
        .await
        .expect("disconnect mid-initialize");
    let _ = release.wait();
    // T-PRS-9: the disconnect is applied to the runner's presence as `Away`.
    let mut saw_away = false;
    while !saw_away {
        match events.recv().await {
            Some(EngineEvent::PlayerStatus {
                player: flagged,
                status,
            }) => {
                if flagged == player(2) {
                    assert_eq!(status, PlayerStatus::Away);
                    saw_away = true;
                }
            }
            Some(_) => {}
            None => panic!("event channel closed before the presence event"),
        }
    }
    // T-PRS-9: `round_start` is read once `initialize` returns, so wait for the
    // actor to reach `InRound` (`RoundStarting`) before advancing the clock.
    // Otherwise the grace would start at the advanced instant, not at 0.
    let _ = round_participants(&mut events).await;
    // PRS-6: grace started at 0, so 30s of paused time expires it. The round's
    // own 60s deadline is not reached, so player 2 is left for this round only
    // (T-PRS-9, TMR-4).
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    let rejected = handle
        .send(Command::Input {
            player: player(2),
            action: GameAction(Vec::new()),
            lag_credit_ms: 0,
            received_at: Instant::now(),
        })
        .await
        .expect_err("a left player's input is rejected");
    assert_eq!(rejected, EngineError::Input(InputError::PlayerLeftRound));
    // Finish at the hard deadline; only the player who left is disqualified.
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    let result = report(&mut events).await;
    assert_eq!(result.rounds.len(), 1);
    assert_eq!(result.rounds[0].round_disqualified.len(), 1);
    assert_eq!(result.rounds[0].round_disqualified[0].0, player(2));
    assert_eq!(result.final_ranking[0].player, player(1));
}

/// A fake that never finishes on its own, so a round only ends by timer
/// (T-PRS-10).
fn quiet_game() -> Box<dyn Game> {
    Box::new(FakeGame::builder("test", "Test").build())
}

#[tokio::test(start_paused = true)]
async fn t_prs_11_mid_round_disconnect_emits_away_then_present() {
    // REQ: PRS-6, PRS-7, T-PRS-8, T-PRS-11
    let (handle, mut events) = spawn(1, 3);
    ready_and_start(&handle).await;
    let participants = round_participants(&mut events).await;
    let away = participants[1];
    handle
        .send(Command::Connection {
            player: away,
            connected: false,
        })
        .await
        .expect("mid-round disconnect");
    await_status(&mut events, away, PlayerStatus::Away).await;
    handle
        .send(Command::Connection {
            player: away,
            connected: true,
        })
        .await
        .expect("reconnect inside the grace");
    await_status(&mut events, away, PlayerStatus::Present).await;
    // T-PRS-8: a reconnect inside the grace keeps the player in the round, so
    // their input is still accepted and the clock never paused.
    handle
        .send(Command::Input {
            player: away,
            action: GameAction(Vec::new()),
            lag_credit_ms: 0,
            received_at: Instant::now(),
        })
        .await
        .expect("a reconnected player's input is accepted");
    // That accepted input is the fake's single finishing tick, so the round
    // ends normally with both players placed.
    let result = report(&mut events).await;
    assert!(result.rounds[0].round_disqualified.is_empty());
    assert_eq!(result.rounds[0].placements.len(), 2);
}

#[tokio::test(start_paused = true)]
async fn t_prs_10_all_graces_expired_end_the_round_immediately() {
    // REQ: RND-13, PRS-6, T-PRS-10
    let mut registry = GameRegistry::new();
    registry.register(quiet_game).expect("unique fake game");
    let (handle, mut events) = spawn_with_registry(registry, prs9_config());
    ready_and_start(&handle).await;
    let participants = round_participants(&mut events).await;
    for &participant in &participants {
        handle
            .send(Command::Connection {
                player: participant,
                connected: false,
            })
            .await
            .expect("mid-round disconnect");
        await_status(&mut events, participant, PlayerStatus::Away).await;
    }
    // RND-13: with every grace expired the round ends at 30s, not at the 60s
    // hard deadline.
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    let result = report(&mut events).await;
    assert_eq!(result.rounds.len(), 1);
    assert_eq!(result.rounds[0].round_disqualified.len(), 2);
}

#[tokio::test(start_paused = true)]
async fn t_tmr_2_a_game_wakeup_ticks_once_and_never_spins() {
    // REQ: GAM-6, TMR-2, T-TMR-6
    // The scripted game asks to be ticked at `round_start` (t = 0) and reports
    // the same instant again after that tick. A repeated instant must not be
    // re-armed, or the actor would spin instead of waiting.
    fn repeating_wakeup_game() -> Box<dyn Game> {
        Box::new(
            FakeGame::builder("test", "Test")
                .wakeups(vec![0, 0])
                .build(),
        )
    }
    let mut registry = GameRegistry::new();
    registry
        .register(repeating_wakeup_game)
        .expect("unique fake game");
    let (handle, mut events) = spawn_with_registry(registry, make_config(1, 3));
    ready_and_start(&handle).await;
    let record = loop {
        match events.recv().await {
            Some(EngineEvent::RoundStarting { .. }) => {
                // The 1s hard deadline ends the round; the game's own wake-up
                // fired at `round_start`.
                tokio::time::advance(std::time::Duration::from_secs(1)).await;
            }
            Some(EngineEvent::RoundFinished(record)) => break record,
            Some(_) => {}
            None => panic!("event channel closed before the round record"),
        }
    };
    assert_eq!(record.status, RoundStatus::Skipped(SkipReason::NoResults));
    let replay = record.replay.expect("record_replays is on");
    assert_eq!(
        replay.inputs,
        vec![(0, superminigames_core::game::GameInput::TimeAdvanced)],
        "the repeated wake-up was ticked exactly once, at `round_start`"
    );
}

/// The seat-order participants of the next `RoundStarting` (RND-11).
async fn round_participants(events: &mut mpsc::Receiver<EngineEvent>) -> Vec<PlayerId> {
    while let Some(event) = events.recv().await {
        if let EngineEvent::RoundStarting { participants, .. } = event {
            return participants;
        }
    }
    panic!("event channel closed before RoundStarting")
}

/// Waits for the next `PlayerStatus` for `player` and asserts its value.
async fn await_status(
    events: &mut mpsc::Receiver<EngineEvent>,
    player: PlayerId,
    expected: PlayerStatus,
) {
    while let Some(event) = events.recv().await {
        if let EngineEvent::PlayerStatus {
            player: flagged,
            status,
        } = event
            && flagged == player
        {
            assert_eq!(status, expected);
            return;
        }
    }
    panic!("event channel closed before the PlayerStatus event")
}
