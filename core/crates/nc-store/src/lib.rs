//! `nc-store`: SQLite-backed task queue, leases, attempts and schema migrations.
//!
//! Phase 0 skeleton (PLAN.md §4). Real functionality arrives in later tasks.

/// Name of this crate, used by the skeleton test and diagnostics.
pub const CRATE_NAME: &str = "nc-store";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_matches() {
        assert_eq!(CRATE_NAME, "nc-store");
    }
}
