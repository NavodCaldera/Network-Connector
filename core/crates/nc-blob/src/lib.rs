//! `nc-blob`: Chunked, hashed, resumable and encrypted blob cache.
//!
//! Phase 0 skeleton (PLAN.md §4). Real functionality arrives in later tasks.

/// Name of this crate, used by the skeleton test and diagnostics.
pub const CRATE_NAME: &str = "nc-blob";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_matches() {
        assert_eq!(CRATE_NAME, "nc-blob");
    }
}
