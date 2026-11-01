//! `nc-agent`: Headless supervisor agent that runs on each device.
//!
//! Phase 0 skeleton (PLAN.md §4). Real functionality arrives in later tasks.

/// Text printed by the skeleton binary.
fn banner() -> String {
    format!("nc-agent {} (skeleton)", env!("CARGO_PKG_VERSION"))
}

fn main() {
    println!("{}", banner());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_names_binary() {
        assert!(banner().starts_with("nc-agent "));
    }
}
