//! `nc-coordinator`: Axum control plane: accounts, registry, scheduler, leases and verifier.
//!
//! Phase 0 skeleton (PLAN.md §4). Real functionality arrives in later tasks.

/// Text printed by the skeleton binary.
fn banner() -> String {
    format!("nc-coordinator {} (skeleton)", env!("CARGO_PKG_VERSION"))
}

fn main() {
    println!("{}", banner());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_names_binary() {
        assert!(banner().starts_with("nc-coordinator "));
    }
}
