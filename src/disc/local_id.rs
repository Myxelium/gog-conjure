//! Synthetic IDs for download-root folders that are not GOG library games.
//!
//! Local IDs set the high bit so they never collide with real GOG product IDs.
//! The lower 63 bits are a stable FNV-1a hash of the lowercased folder name.

/// High bit reserved for non-GOG / local folder identities.
pub const LOCAL_ID_FLAG: u64 = 1 << 63;

/// True when `id` was assigned to a local (non-GOG) download folder.
pub fn is_local_game_id(id: u64) -> bool {
    id & LOCAL_ID_FLAG != 0
}

/// Stable synthetic id for an unmatched download-root folder name.
pub fn local_game_id(folder_name: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325; // FNV-1a 64-bit offset basis
    for b in folder_name.to_lowercase().bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (hash & !LOCAL_ID_FLAG) | LOCAL_ID_FLAG
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_ids_set_high_bit() {
        let id = local_game_id("Some Local Folder");
        assert!(is_local_game_id(id));
        assert!(!is_local_game_id(1207658691));
        assert!(!is_local_game_id(0));
    }

    #[test]
    fn local_ids_are_stable_and_case_insensitive() {
        assert_eq!(local_game_id("Foo"), local_game_id("foo"));
        assert_ne!(local_game_id("Foo"), local_game_id("Bar"));
    }
}
