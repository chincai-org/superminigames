//! Place and group computation (SCR-3, SCR-4).

/// Returns `(place, group)` for each key, in input order (VER-13).
///
/// `place` is one plus the number of strictly smaller keys and `group` is the
/// number of keys equal to this one, so tied players share a place and the
/// following places are skipped (1, 1, 3, 4). Comparing the `(u8, i128)` tuples
/// is the lexicographic comparison SCR-3 requires.
pub fn places_from_keys(keys: &[(u8, i128)]) -> Vec<(usize, usize)> {
    keys.iter()
        .map(|&key| {
            let mut place = 1;
            let mut group = 0;
            for &other in keys {
                if other < key {
                    place += 1;
                } else if other == key {
                    group += 1;
                }
            }
            (place, group)
        })
        .collect()
}
