//! Round-start presence rules and the final ranking (PRS-1..PRS-8, FIN-1).
//!
//! Everything here is pure and synchronous: time is passed in as
//! `tokio::time::Instant` values, nothing reads the clock and no task is
//! spawned (ARC-4). The actor owns the state and calls these functions at a
//! round start and when a reconnect grace expires.

use std::time::Duration;

use superminigames_core::ids::PlayerId;
use superminigames_logic::ranking::rank_players;
use superminigames_logic::streak::next_streak;
use tokio::time::Instant;

use crate::report::{PlayerState, RankEntry, Standing};

/// The reconnect grace a participant gets after disconnecting mid-round
/// (PRS-6). The game and its clocks keep running during the grace; it is not a
/// pause.
pub const RECONNECT_GRACE: Duration = Duration::from_secs(30);

/// What one round-start presence check produced (PRS-1..PRS-5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoundStart {
    /// InMatch and Present at this start, in seat order (PRS-5, RND-3).
    pub participants: Vec<PlayerId>,
    /// InMatch players whose streak reached `K` and are now Removed (PRS-3).
    pub removed: Vec<PlayerId>,
}

/// Applies the round-start presence rules to every seated player (PRS-1..PRS-5).
///
/// PRS-1: each `connected` flag is read once, before any change, so the whole
/// check uses a single snapshot. PRS-2: Present resets `absent_streak` to 0
/// and Absent adds 1 (saturating). PRS-3: a player whose new streak reaches `k`
/// becomes `Removed` at this start and is reported in [`RoundStart::removed`].
/// PRS-5: an InMatch player who is Present is a participant. A Removed player
/// is terminal, so it is skipped here and is never a participant (PRS-4).
pub fn apply_round_start(states: &mut [PlayerState], k: u32) -> RoundStart {
    // PRS-1: the one snapshot per round start, taken before the updates below.
    let present: Vec<bool> = states.iter().map(|state| state.connected).collect();

    let mut participants = Vec::new();
    let mut removed = Vec::new();
    for (state, &present) in states.iter_mut().zip(&present) {
        if state.standing != Standing::InMatch {
            continue; // PRS-4: Removed is permanent; it scores and changes nothing.
        }
        let (streak, is_removed) = next_streak(state.absent_streak, present, k);
        state.absent_streak = streak;
        if is_removed {
            state.standing = Standing::Removed;
            removed.push(state.player.id);
        } else if present {
            participants.push(state.player.id);
        }
    }
    RoundStart {
        participants,
        removed,
    }
}

/// Reconnect-grace bookkeeping for one round (PRS-6, PRS-7).
///
/// Holds only `(player, deadline)` pairs. It has no access to [`PlayerState`],
/// so a grace expiring can never touch `standing` or `absent_streak` (PRS-8,
/// TMR-5): only [`apply_round_start`] changes those.
#[derive(Clone, Debug, Default)]
pub struct ReconnectGrace {
    away: Vec<(PlayerId, Instant)>,
}

impl ReconnectGrace {
    /// An empty grace set.
    pub fn new() -> Self {
        Self::default()
    }

    /// The earliest deadline for an away participant, for the actor's TMR-2 timer.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.away.iter().map(|(_, deadline)| *deadline).min()
    }

    /// A participant disconnected mid-round: start their grace at `now` (PRS-6).
    ///
    /// `now` is supplied by the caller, keeping this a pure bookkeeping step.
    /// The grace runs from the first disconnect only: calling this again for a
    /// player who is already away keeps their first deadline, so a repeated
    /// disconnect cannot push the expiry later (PRS-6).
    pub fn away(&mut self, player: PlayerId, now: Instant) {
        if self.away.iter().any(|(away, _)| *away == player) {
            return;
        }
        self.away.push((player, now + RECONNECT_GRACE));
    }

    /// A participant reconnected at `now` (PRS-6, PRS-7).
    ///
    /// Returns `true` only when they reconnected inside the grace and may
    /// continue the same round. Once the grace has expired the entry is gone,
    /// so a later reconnect returns `false` and never rejoins the round (PRS-7).
    pub fn returned(&mut self, player: PlayerId, now: Instant) -> bool {
        let inside = self
            .away
            .iter()
            .position(|(away, deadline)| *away == player && now < *deadline);
        match inside {
            Some(index) => {
                self.away.remove(index);
                true
            }
            None => false,
        }
    }

    /// Removes and returns the players whose grace has expired at `now` (PRS-6).
    ///
    /// Each player is returned exactly once: the entry is dropped as it is
    /// yielded, so a later `expire` cannot repeat it.
    pub fn expire(&mut self, now: Instant) -> Vec<PlayerId> {
        let mut expired = Vec::new();
        let mut index = 0;
        while index < self.away.len() {
            if now >= self.away[index].1 {
                expired.push(self.away.remove(index).0);
            } else {
                index += 1;
            }
        }
        expired
    }
}

/// The final ranking over every seated player, in seat order (FIN-1, T-SCR-9).
///
/// Points and ranks both come from each state's `total_points`, so they cannot
/// disagree. [`rank_players`] orders Removed players below every remaining
/// player whatever the totals and applies competition ranking, so a Removed
/// player keeps their points but ranks last (PRS-4, T-SCR-9). Because the
/// output follows the input order, ties keep seat order (FIN-1).
pub fn final_ranking(states: &[PlayerState]) -> Vec<RankEntry> {
    let totals: Vec<u32> = states.iter().map(|state| state.total_points).collect();
    let removed: Vec<bool> = states
        .iter()
        .map(|state| state.standing == Standing::Removed)
        .collect();
    let ranks = rank_players(&removed, &totals);
    states
        .iter()
        .zip(&ranks)
        .map(|(state, &rank)| RankEntry {
            player: state.player.id,
            rank,
            total_points: state.total_points,
            standing: state.standing,
        })
        .collect()
}
