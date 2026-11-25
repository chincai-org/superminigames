//! T2.3: round-start presence, removal and the reconnect grace
//! (PRS-1..PRS-8, T-SCR-9, FIN-1).
//!
//! These are pure functions: time is passed in, so the tests need no runtime
//! and no paused clock. A scripted match harness belongs to T2.4.

use std::time::Duration;

use superminigames_core::ids::{Player, PlayerId};
use superminigames_engine::presence::{
    RECONNECT_GRACE, ReconnectGrace, apply_round_start, final_ranking,
};
use superminigames_engine::report::{PlayerState, Standing};
use tokio::time::Instant;

/// A deterministic `PlayerId` from a fixed 32-hex literal (CI-2, TYP-3).
///
/// `uuid` is not an engine dependency and the engine never generates ids, so
/// the tests decode the literal through `PlayerId`'s serde impl instead.
fn player(n: u64) -> PlayerId {
    serde_json::from_str(&format!("\"{n:032x}\"")).expect("a 32-hex uuid literal")
}

/// A seat with a fresh `PlayerState`.
fn state(n: u64, connected: bool) -> PlayerState {
    PlayerState {
        player: Player {
            id: player(n),
            name: format!("p{n}"),
        },
        ready: true,
        connected,
        standing: Standing::InMatch,
        absent_streak: 0,
        total_points: 0,
    }
}

#[test]
fn t_prs_1_grace_expires_once_and_the_player_returns_next_round() {
    // REQ: T-PRS-1, PRS-1, PRS-5, PRS-6
    let start = Instant::now();
    let mut states = vec![state(0, true), state(1, true)];
    let first = apply_round_start(&mut states, 3);
    assert_eq!(first.participants, vec![player(0), player(1)]);
    assert!(first.removed.is_empty());

    // The player disconnects mid-round and does not return within the grace.
    let mut grace = ReconnectGrace::new();
    grace.away(player(0), start);
    assert_eq!(grace.expire(start + RECONNECT_GRACE), vec![player(0)]);
    assert!(
        grace
            .expire(start + RECONNECT_GRACE + Duration::from_secs(1))
            .is_empty(),
        "the grace expiry must yield each player exactly once"
    );

    // Connected at the start of the next round, they are a participant again
    // (PRS-5): the round disqualification lasted for one round only.
    states[0].connected = true;
    let next = apply_round_start(&mut states, 3);
    assert_eq!(next.participants, vec![player(0), player(1)]);
    assert_eq!(states[0].absent_streak, 0);
    assert_eq!(states[0].standing, Standing::InMatch);
}

#[test]
fn t_prs_2_absence_reaches_k_at_the_third_absent_start() {
    // REQ: T-PRS-2, PRS-2, PRS-3
    let mut states = vec![state(0, true), state(1, true)];
    let k = 3;

    // Rounds 0..=4: present, so the streak stays 0.
    for round in 0..5 {
        let start = apply_round_start(&mut states, k);
        assert!(start.removed.is_empty(), "round {round} removed early");
        assert_eq!(states[0].absent_streak, 0, "round {round}");
    }

    // Rounds 5 and 6: absent, not yet at K.
    states[0].connected = false;
    for (round, expected) in [(5u32, 1u32), (6, 2)] {
        let start = apply_round_start(&mut states, k);
        assert!(start.removed.is_empty(), "round {round} removed early");
        assert_eq!(states[0].standing, Standing::InMatch, "round {round}");
        assert_eq!(states[0].absent_streak, expected, "round {round}");
        assert!(!start.participants.contains(&player(0)), "round {round}");
    }

    // Round 7: the third absent start reaches K and removes (PRS-3).
    let start = apply_round_start(&mut states, k);
    assert_eq!(start.removed, vec![player(0)]);
    assert_eq!(states[0].standing, Standing::Removed);
    assert_eq!(states[0].absent_streak, 3);
}

#[test]
fn t_prs_3_present_at_the_next_start_resets_the_streak() {
    // REQ: T-PRS-3, PRS-2
    let mut states = vec![state(0, true)];
    let k = 3;

    // Absent at two round starts: streak 2.
    states[0].connected = false;
    apply_round_start(&mut states, k);
    apply_round_start(&mut states, k);
    assert_eq!(states[0].absent_streak, 2);

    // Connected at the next start resets the streak and returns the player.
    states[0].connected = true;
    let start = apply_round_start(&mut states, k);
    assert_eq!(states[0].absent_streak, 0);
    assert_eq!(start.participants, vec![player(0)]);

    // Absent again at the next two starts: not removed, because the streak
    // restarted.
    states[0].connected = false;
    apply_round_start(&mut states, k);
    apply_round_start(&mut states, k);
    assert_eq!(states[0].absent_streak, 2);
    assert_eq!(states[0].standing, Standing::InMatch);
}

#[test]
fn t_prs_4_a_removed_player_never_returns_even_when_connected() {
    // REQ: T-PRS-4, PRS-4
    // K = 1 so the first absent start removes.
    let mut states = vec![state(0, false), state(1, true)];
    let start = apply_round_start(&mut states, 1);
    assert_eq!(start.removed, vec![player(0)]);
    assert_eq!(states[0].standing, Standing::Removed);

    // The player reconnects at a later round start but stays out.
    states[0].connected = true;
    let start = apply_round_start(&mut states, 1);
    assert_eq!(start.participants, vec![player(1)]);
    assert!(!start.participants.contains(&player(0)));
    assert!(start.removed.is_empty(), "removal must not repeat");
    assert_eq!(states[0].standing, Standing::Removed);
}

#[test]
fn t_prs_6_return_within_the_grace_continues_the_round() {
    // REQ: PRS-6, PRS-7
    let start = Instant::now();
    let mut grace = ReconnectGrace::new();
    grace.away(player(0), start);

    // Back inside the grace: they continue the same round.
    assert!(grace.returned(player(0), start + Duration::from_secs(29)));
    assert!(
        !grace.returned(player(0), start + Duration::from_secs(29)),
        "a second return has nothing to cancel"
    );

    // At the grace boundary the player is already out (PRS-7).
    grace.away(player(1), start);
    assert!(!grace.returned(player(1), start + RECONNECT_GRACE));
    assert_eq!(grace.expire(start + RECONNECT_GRACE), vec![player(1)]);
    assert!(!grace.returned(player(1), start + RECONNECT_GRACE));
}

#[test]
fn t_prs_6_a_second_away_does_not_extend_the_grace() {
    // REQ: PRS-6, PRS-7
    let start = Instant::now();
    let mut grace = ReconnectGrace::new();
    grace.away(player(0), start);

    // The player flaps: a second disconnect must not move the deadline.
    grace.away(player(0), start + Duration::from_secs(10));
    assert!(
        !grace.returned(player(0), start + RECONNECT_GRACE),
        "the grace still ends at the first deadline (PRS-6)"
    );
    assert_eq!(
        grace.expire(start + RECONNECT_GRACE),
        vec![player(0)],
        "a second away at a later instant must not push the expiry later"
    );
}

#[test]
fn t_prs_8_grace_expiry_keeps_standing_and_streak() {
    // REQ: PRS-8, TMR-5
    let start = Instant::now();
    let mut states = vec![state(0, true), state(1, true)];
    apply_round_start(&mut states, 3);
    let before = (states[0].standing, states[0].absent_streak);

    // Expiring the grace round-disqualifies the player for that round only.
    // The grace holds no PlayerState, so it cannot change either field.
    let mut grace = ReconnectGrace::new();
    grace.away(player(0), start);
    assert_eq!(grace.expire(start + RECONNECT_GRACE), vec![player(0)]);
    assert_eq!(
        (states[0].standing, states[0].absent_streak),
        before,
        "round disqualification must not touch Standing or absent_streak"
    );
}

#[test]
fn t_scr_9_a_removed_player_keeps_points_but_ranks_last() {
    // REQ: T-SCR-9, FIN-1, PRS-4
    let mut removed = state(0, false);
    removed.standing = Standing::Removed;
    removed.total_points = 9;
    let mut remaining = state(1, true);
    remaining.total_points = 3;

    let ranking = final_ranking(&[removed, remaining]);
    assert_eq!(ranking.len(), 2);

    assert_eq!(ranking[0].player, player(0));
    assert_eq!(ranking[0].rank, 2);
    assert_eq!(
        ranking[0].total_points, 9,
        "a Removed player keeps the points"
    );
    assert_eq!(ranking[0].standing, Standing::Removed);

    assert_eq!(ranking[1].player, player(1));
    assert_eq!(ranking[1].rank, 1);
    assert_eq!(ranking[1].total_points, 3);
    assert_eq!(ranking[1].standing, Standing::InMatch);
}

#[test]
fn t_fin_1_ties_share_a_rank_and_keep_seat_order() {
    // REQ: FIN-1, T-SCR-8
    let mut a = state(0, true);
    a.total_points = 15;
    let mut b = state(1, true);
    b.total_points = 15;
    let mut c = state(2, true);
    c.total_points = 9;
    let mut d = state(3, true);
    d.total_points = 3;

    let ranking = final_ranking(&[a, b, c, d]);
    let line: Vec<(PlayerId, u32, u32)> = ranking
        .iter()
        .map(|entry| (entry.player, entry.rank, entry.total_points))
        .collect();
    assert_eq!(
        line,
        vec![
            (player(0), 1, 15),
            (player(1), 1, 15),
            (player(2), 3, 9),
            (player(3), 4, 3),
        ],
        "ties share a rank, skip the next, and stay in seat order"
    );
}

#[test]
fn t_fin_1_removed_players_rank_below_every_remaining_player() {
    // REQ: FIN-1, PRS-4
    let mut lead = state(0, true);
    lead.total_points = 99;
    let mut gone = state(1, true);
    gone.standing = Standing::Removed;
    gone.total_points = 0;
    let mut second = state(2, true);
    second.total_points = 3;

    let ranking = final_ranking(&[lead, gone, second]);
    let line: Vec<(PlayerId, u32)> = ranking
        .iter()
        .map(|entry| (entry.player, entry.rank))
        .collect();
    assert_eq!(
        line,
        vec![(player(0), 1), (player(1), 3), (player(2), 2)],
        "a Removed player ranks below every remaining player whatever the totals"
    );
}

#[test]
fn t_prs_6_next_deadline_is_earliest_away_expiry() {
    // REQ: TMR-2, PRS-6
    let start = Instant::now();
    let mut grace = ReconnectGrace::new();
    assert_eq!(grace.next_deadline(), None);
    grace.away(player(1), start + Duration::from_secs(4));
    grace.away(player(2), start);
    assert_eq!(grace.next_deadline(), Some(start + RECONNECT_GRACE));
}

#[test]
fn t_prs_6_next_deadline_is_the_earliest_grace() {
    // REQ: PRS-6, TMR-2
    let start = Instant::now();
    let mut grace = ReconnectGrace::new();
    assert_eq!(grace.next_deadline(), None, "no participant is away");

    grace.away(player(0), start + Duration::from_secs(10));
    grace.away(player(1), start);
    assert_eq!(
        grace.next_deadline(),
        Some(start + RECONNECT_GRACE),
        "the actor arms the earliest grace expiry (TMR-2)"
    );

    grace.expire(start + RECONNECT_GRACE);
    assert_eq!(
        grace.next_deadline(),
        Some(start + Duration::from_secs(10) + RECONNECT_GRACE),
        "the deadline moves to the next away participant"
    );

    grace.expire(start + Duration::from_secs(10) + RECONNECT_GRACE);
    assert_eq!(
        grace.next_deadline(),
        None,
        "no away participant is left to wait for"
    );
}
