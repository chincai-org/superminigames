//! Shared RNG helpers (RNG-1 to RNG-5, section 6).
//!
//! This is the single implementation of the seeded stream and of unbiased index
//! sampling. `core` and the game crates MUST use it instead of any other
//! randomness (RNG-7).

use rand_chacha::ChaCha8Rng;
use rand_core::{RngCore, SeedableRng};

/// The keyed RNG for `seed` and `stream` (RNG-1, RNG-2, RNG-3).
///
/// The key is 32 bytes with the seed's 8 little-endian bytes in positions 0..8
/// and zeros elsewhere, so seed 0 is an all-zero key. `from_seed` is used
/// deliberately (RNG-2), before `set_stream(stream)` picks the round's stream
/// (RNG-3).
pub fn chacha_from_seed(seed: u64, stream: u64) -> ChaCha8Rng {
    let mut key = [0u8; 32];
    key[..8].copy_from_slice(&seed.to_le_bytes());
    let mut rng = ChaCha8Rng::from_seed(key);
    rng.set_stream(stream);
    rng
}

/// Unbiased index in `0..n` via rejection sampling (RNG-5).
///
/// `gen_range` and every `Distribution` are forbidden here: their output may
/// change between crate versions.
pub fn uniform_index(rng: &mut ChaCha8Rng, n: usize) -> usize {
    assert!(n > 0);
    let n = n as u64;
    let limit = u64::MAX - (u64::MAX % n); // largest multiple of n that fits
    loop {
        let x = rng.next_u64();
        if x < limit {
            return (x % n) as usize;
        }
    }
}
