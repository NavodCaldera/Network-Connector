//! Declares the `frb_expand` cfg that the `#[frb]` macro emits, so `unexpected_cfgs` stays an error elsewhere.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(frb_expand)");
}
