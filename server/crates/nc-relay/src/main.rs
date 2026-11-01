//! `nc-relay`: libp2p relay v2 with short-lived tokens and quotas.
//!
//! Phase 0 skeleton (PLAN.md §4). Real functionality arrives in later tasks.

/// Text printed by the skeleton binary.
fn banner() -> String {
    format!("nc-relay {} (skeleton)", env!("CARGO_PKG_VERSION"))
}

fn main() {
    println!("{}", banner());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_names_binary() {
        assert!(banner().starts_with("nc-relay "));
    }
}
