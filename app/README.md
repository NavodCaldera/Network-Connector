# app

Flutter app (owner: App agent). Owner-facing UI over the Rust agent through `flutter_rust_bridge` 2.13.0 (pinned in
`pubspec.yaml`, `core/crates/nc-bridge/Cargo.toml` and the codegen CLI).

- Screens: Home (status, today's summary, rules), Limits (nc-policy `OwnerPolicy`), Night data (nc-tariff), About
  (versions, licences). Stop sits above the navigation bar on every screen and is always enabled.
- All validation and tariff maths run in Rust (`nc-bridge` -> nc-policy / nc-tariff). Dart only shows the results.
- Settings: one versioned JSON file, `settings.json`, in the app-support folder (`%APPDATA%\NetworkConnector` on
  Windows). It is written atomically (temp file + rename). A bad file is kept as `settings.json.bad` and safe
  defaults are used. It holds no secrets. Mobile has no settings folder yet (settings stay in memory there).
- Night-data presets are starting points, not ISP facts. The owner checks them against their own package.
- `lib/src/rust/` and `core/crates/nc-bridge/src/frb_generated.rs` are generated. Regenerate after changing
  `core/crates/nc-bridge/src/api.rs`:
  `cargo install flutter_rust_bridge_codegen --version =2.13.0 --locked`, then in `app/`:
  `flutter_rust_bridge_codegen generate`.
- `flutter build windows --release` also runs `cargo build --release -p nc-bridge` (see `windows/CMakeLists.txt`)
  and puts `nc_bridge.dll` next to the exe. Honours `CARGO_TARGET_DIR`.
- `flutter test`: widget tests use a fake agent (`test/fake_agent.dart`). `test/rust_bridge_test.dart` runs the real
  Dart -> Rust round trip (session, validation, tariff status, settings save/load/corrupt) when `NC_BRIDGE_DLL`
  points at a built `nc_bridge.dll`; otherwise it is skipped.
- No plugin packages: Flutter plugins need symlinks (Windows Developer Mode), so the settings folder comes from Rust.
- UI text lives in `lib/src/strings.dart`, ready to move to ARB files for Sinhala and Tamil.
- Android is scaffolded only; it does not build the Rust library yet (task 0.9).
