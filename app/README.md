# app

Flutter app (owner: App agent). Phase 0 task 0.8: one screen that starts and stops a helper session in the Rust
agent through `flutter_rust_bridge` 2.13.0 (pinned in `pubspec.yaml`, `core/crates/nc-bridge/Cargo.toml` and the
codegen CLI).

- `lib/src/rust/` and `core/crates/nc-bridge/src/frb_generated.rs` are generated. Regenerate after changing
  `core/crates/nc-bridge/src/api.rs`:
  `cargo install flutter_rust_bridge_codegen --version =2.13.0 --locked`, then in `app/`:
  `flutter_rust_bridge_codegen generate`.
- `flutter build windows --release` also runs `cargo build --release -p nc-bridge` (see `windows/CMakeLists.txt`)
  and puts `nc_bridge.dll` next to the exe. Honours `CARGO_TARGET_DIR`.
- `flutter test`: widget tests use a fake agent. `test/rust_bridge_test.dart` runs the real Dart -> Rust round trip
  when `NC_BRIDGE_DLL` points at a built `nc_bridge.dll`; otherwise it is skipped.
- UI text lives in `lib/src/strings.dart`, ready to move to ARB files for Sinhala and Tamil.
- Android is scaffolded only; it does not build the Rust library yet (task 0.9).
