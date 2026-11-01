//! `nc-net`: libp2p transport, local discovery, pairing and relay client.
//!
//! Phase 0 skeleton (PLAN.md §4). Real functionality arrives in later tasks.

/// Name of this crate, used by the skeleton test and diagnostics.
pub const CRATE_NAME: &str = "nc-net";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_matches() {
        assert_eq!(CRATE_NAME, "nc-net");
    }
}
