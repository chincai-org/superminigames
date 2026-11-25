//! T2.3: the engine report value types (TYP-1) and the final report shape
//! (MTH-3, FIN-1).
//!
//! The types are engine-internal and derive no serde traits (TYP-1, PRO-4), so
//! these tests construct and compare them directly.

use superminigames_core::config::{GameFilterMode, MatchConfig};
use superminigames_core::ids::{GameId, LobbyId, MatchId, Player, PlayerId};
use superminigames_core::outcome::DisqualificationReason;
use superminigames_engine::presence::final_ranking;
use superminigames_engine::report::{
    LobbySnapshot, MatchReport, PlayerState, PlayerStatus, RankEntry, RoundRecord, RoundStatus,
    SkipReason, Standing,
};

/// A deterministic id from a fixed 32-hex literal (CI-2, TYP-3).
fn from_hex<T: serde::de::DeserializeOwned>(n: u64) -> T {
    serde_json::from_str(&format!("\"{n:032x}\"")).expect("a 32-hex uuid literal")
}

fn player(n: u64) -> PlayerId {
    from_hex(n)
}

fn seat(n: u64, name: &str) -> Player {
    Player {
        id: player(n),
        name: name.to_owned(),
    }
}

fn config() -> MatchConfig {
    MatchConfig {
        rounds: 3,
        round_time_limit_secs: 60,
        seed: 0,
        max_absent_round_starts: 3,
        filter: GameFilterMode::BanList(Vec::new()),
    }
}

fn state(n: u64, standing: Standing, total_points: u32) -> PlayerState {
    PlayerState {
        player: seat(n, "p"),
        ready: true,
        connected: standing == Standing::InMatch,
        standing,
        absent_streak: 0,
        total_points,
    }
}

#[test]
fn t_typ_1_lobby_snapshot_keeps_seat_order_and_hides_the_code() {
    // REQ: TYP-1, PRO-4, PRO-15
    let snapshot = LobbySnapshot {
        lobby_id: from_hex(7),
        host: player(0),
        seats: vec![(seat(0, "ana"), true), (seat(1, "bo"), false)],
        config: config(),
        private: true,
        settings_locked: true,
    };
    assert_eq!(snapshot.seats[0].0.id, player(0), "seats keep seat order");
    assert_eq!(snapshot.seats[1].0.id, player(1));
    assert_eq!(snapshot.host, player(0));
    assert!(snapshot.private, "only the flag is exposed, never the code");
}

#[test]
fn t_typ_1_player_status_has_the_three_presence_states() {
    // REQ: TYP-1, PRS-6
    let statuses = [
        PlayerStatus::Present,
        PlayerStatus::Away,
        PlayerStatus::Removed,
    ];
    assert_ne!(statuses[0], statuses[1]);
    assert_ne!(statuses[1], statuses[2]);
}

#[test]
fn t_typ_1_round_record_holds_a_scored_and_a_skipped_round() {
    // REQ: TYP-1, MTH-3, FLR-6, SCR-9
    let scored = RoundRecord {
        round_index: 0,
        game: Some(GameId("chess".to_owned())),
        participants: vec![player(0), player(1)],
        attempts: 1,
        status: RoundStatus::Scored,
        placements: vec![(player(0), 1, 12), (player(1), 2, 0)],
        round_disqualified: Vec::new(),
        removed_this_round: Vec::new(),
        replay: None,
    };
    assert_eq!(scored.status, RoundStatus::Scored);
    assert_eq!(scored.placements.len(), 2);

    let skipped = RoundRecord {
        round_index: 1,
        game: None,
        participants: vec![player(0)],
        attempts: 0,
        status: RoundStatus::Skipped(SkipReason::NotEnoughPlayers),
        placements: Vec::new(),
        round_disqualified: vec![(player(1), DisqualificationReason::LeftRound)],
        removed_this_round: vec![player(1)],
        replay: None,
    };
    assert_eq!(
        skipped.status,
        RoundStatus::Skipped(SkipReason::NotEnoughPlayers)
    );
    assert!(
        skipped.placements.is_empty(),
        "a skipped round awards no points (SCR-9)"
    );
    assert_eq!(
        skipped.round_disqualified,
        vec![(player(1), DisqualificationReason::LeftRound)],
        "round disqualification lives in the record, not in Standing (section 12.3)"
    );
}

#[test]
fn t_mth_3_the_report_ranks_every_seated_player_in_seat_order() {
    // REQ: MTH-3, FIN-1, T-SCR-9
    let states = vec![
        state(0, Standing::InMatch, 3),
        state(1, Standing::Removed, 9),
    ];
    let ranking: Vec<RankEntry> = final_ranking(&states);
    let report = MatchReport {
        match_id: from_hex::<MatchId>(1),
        lobby_id: from_hex::<LobbyId>(2),
        config: config(),
        rounds: Vec::new(),
        final_ranking: ranking,
        ended_early: true,
    };

    assert!(report.ended_early, "the match ended before all slots");
    assert_eq!(report.final_ranking.len(), 2);
    assert_eq!(report.final_ranking[0].player, player(0));
    assert_eq!(report.final_ranking[0].rank, 1);
    assert_eq!(report.final_ranking[0].standing, Standing::InMatch);
    assert_eq!(report.final_ranking[1].player, player(1));
    assert_eq!(report.final_ranking[1].rank, 2);
    assert_eq!(
        report.final_ranking[1].total_points, 9,
        "a Removed player keeps the points but ranks below a remaining player"
    );
}
