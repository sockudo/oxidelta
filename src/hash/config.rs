// Matcher profiles matching xdelta3's `xdelta3-cfgs.h`.
//
// Each profile defines tuning parameters for the hash/match engine.

/// Minimum COPY length (VCDIFF code table minimum).
pub const MIN_MATCH: usize = 4;

/// Minimum run worth encoding.
pub const MIN_RUN: usize = 8;

/// Default input window size (8 MiB).
pub const DEFAULT_WINSIZE: usize = 1 << 23;

/// Default prev-chain array size (256 KiB).
pub const DEFAULT_SPREVSZ: usize = 1 << 18;

/// Default source window size (64 MiB).
pub const DEFAULT_SRCWINSZ: usize = 1 << 26;

/// Maximum LRU block cache entries.
pub const MAX_LRU_SIZE: usize = 32;

/// Matcher profile configuration.
///
/// Field names match xdelta3's `xd3_smatcher` structure.
#[derive(Debug, Clone, Copy)]
pub struct MatcherConfig {
    /// Name for display purposes.
    pub name: &'static str,
    /// Large (source) hash window width.
    pub large_look: usize,
    /// Large hash step size (bytes between indexed positions).
    pub large_step: usize,
    /// Small (target) hash window width (always 4).
    pub small_look: usize,
    /// Maximum chain length for small-match search.
    pub small_chain: usize,
    /// Maximum chain length for small-match lazy search.
    pub small_lchain: usize,
    /// Maximum match length before lazy matching is skipped.
    pub max_lazy: usize,
    /// Match length considered "long enough" to stop searching.
    pub long_enough: usize,
}

/// Compression levels mapping to profiles (matches xdelta3-main.h).
///
/// - Level 0: NOCOMPRESS + fastest
/// - Level 1: fastest
/// - Level 2: faster
/// - Levels 3-5: fast
/// - Level 6: default
/// - Levels 7-9: slow
pub fn config_for_level(level: u32) -> MatcherConfig {
    match level {
        0 | 1 => FASTEST,
        2 => FASTER,
        3..=5 => FAST,
        6 => DEFAULT,
        _ => SLOW,
    }
}

// ---------------------------------------------------------------------------
// Profile definitions (exact match of xdelta3-cfgs.h)
// ---------------------------------------------------------------------------

pub const FASTEST: MatcherConfig = MatcherConfig {
    name: "fastest",
    large_look: 9,
    large_step: 26,
    small_look: 4,
    small_chain: 1,
    small_lchain: 1,
    max_lazy: 6,
    long_enough: 6,
};

pub const FASTER: MatcherConfig = MatcherConfig {
    name: "faster",
    large_look: 9,
    large_step: 15,
    small_look: 4,
    small_chain: 1,
    small_lchain: 1,
    max_lazy: 18,
    long_enough: 18,
};

pub const FAST: MatcherConfig = MatcherConfig {
    name: "fast",
    large_look: 9,
    large_step: 8,
    small_look: 4,
    small_chain: 4,
    small_lchain: 1,
    max_lazy: 18,
    long_enough: 35,
};

pub const DEFAULT: MatcherConfig = MatcherConfig {
    name: "default",
    large_look: 9,
    large_step: 3,
    small_look: 4,
    small_chain: 8,
    small_lchain: 2,
    max_lazy: 36,
    long_enough: 70,
};

pub const SLOW: MatcherConfig = MatcherConfig {
    name: "slow",
    large_look: 9,
    large_step: 2,
    small_look: 4,
    small_chain: 44,
    small_lchain: 13,
    max_lazy: 90,
    long_enough: 70,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_profiles_have_slook_4() {
        for p in [FASTEST, FASTER, FAST, DEFAULT, SLOW] {
            assert_eq!(
                p.small_look, MIN_MATCH,
                "profile {} has wrong small_look",
                p.name
            );
        }
    }

    #[test]
    fn all_profiles_have_llook_9() {
        for p in [FASTEST, FASTER, FAST, DEFAULT, SLOW] {
            assert_eq!(p.large_look, 9, "profile {} has wrong large_look", p.name);
        }
    }

    #[test]
    fn level_mapping() {
        assert_eq!(config_for_level(0).name, "fastest");
        assert_eq!(config_for_level(1).name, "fastest");
        assert_eq!(config_for_level(2).name, "faster");
        assert_eq!(config_for_level(3).name, "fast");
        assert_eq!(config_for_level(5).name, "fast");
        assert_eq!(config_for_level(6).name, "default");
        assert_eq!(config_for_level(7).name, "slow");
        assert_eq!(config_for_level(9).name, "slow");
    }
}
