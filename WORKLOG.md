# Work log

Every person or agent who changes this project adds an entry here **in the same commit as the change**
(rule: [AGENT.md §1 "Work log"](AGENT.md#work-log-required)). Newest entry first.

Each entry has:
- **Date, who, branch** and the commits it covers;
- **Asked:** what the request was;
- **Done:** what changed, by file or feature;
- **Decisions:** choices made and who made them (owner decisions are named as such);
- **Verified:** commands run and their real results, including failures;
- **Not done / open:** what is left, what could not be checked, and why.

Never write secrets, tokens, passphrases, pairing codes or job contents into this file.

---

## 2026-11-02 - Claude (coordinator) - `main`

- **Commits:** this merge of `feat/app-ui` (f5020d6).
- **Asked:** review app-agent task 1.A1 (Limits, Night data, settings file, navigation).
- **Done:** merged; TASKS.md 1.A1 set to done.
- **Decisions:** the presets stay labelled "check your own package", not ISP facts. Settings validation lives only in
  Rust (nc-policy/nc-tariff).
- **Verified:** relied on the agent's real results: nc-bridge 22 tests, fmt/clippy/deny clean, flutter analyze
  clean, 12 widget tests, real-bridge test (Stop 4.5 ms), release build, corrupt settings file kept as .bad.
  CI run 41349837273: flutter, Docker stack, deny and server jobs green; the core jobs fail only on the existing
  nc-identity Linux test and the 11 nc-exec Windows sandbox tests (Codex is fixing them in 1.P7).
- **Not done / open:** the owner click-through (save limits, restart, Stop) has not been done. nc-agent still needs
  `update_network_budget` and today's counters. The Android 0.9a build is paused while the owner copies files.

## 2026-11-02 - Claude (app-agent) - `feat/app-ui`

- **Commits:** this commit (task 1.A1).
- **Asked:** build the real owner UI on the 0.8 shell: a Limits screen (nc-policy), a Night data screen (nc-tariff), a
  persisted settings file, a Home summary with Stop always on, and Home/Limits/Night data/About navigation. Tests
  in Dart and Rust.
- **Done:**
  - nc-bridge: `api.rs` gains `TimeWindow`, extended `PolicySummary` (idle minutes, time zone, hours),
    `NightDataSettings`, `TariffPreset`, `AppSettings`, `LoadedSettings`, `NightDataStatus`, `TodaySummary`, and the
    functions `default_settings`, `validate_policy`, `validate_night_data`, `tariff_presets`, `night_data_status`,
    `settings_dir`, `load_settings`, `save_settings`, `today_summary` and `core_version`. Logic lives in the new
    `settings.rs` (OwnerPolicy conversion and validation, versioned file, atomic write, corrupt file moved to `.bad`,
    256 KiB cap, `deny_unknown_fields`), `night.rs` (TariffProfile built from the settings; next window and budget
    left from `nc_tariff::decide` and `BudgetLedger`) and `today.rs` (helping time today, in memory). frb glue
    regenerated (2.13.0).
  - app: `AppModel` (state, save then apply to a running session), `AppShell` (NavigationBar, with the Stop bar on
    every screen), `home_page`, `limits_page`, `night_data_page`, `about_page` (versions, `showLicensePage`),
    `widgets.dart`. All text is in `strings.dart`. The tests use `test/fake_agent.dart`.
  - TASKS.md row 1.A1 (review), app/README.md.
- **Decisions (app-agent):**
  - Night-data presets (midnight-8 am, midnight-6 am, 10 pm-6 am, every day, Asia/Colombo) are starting points that
    the owner confirms, not ISP facts (PLAN.md §6). Labels are in strings.dart, not Rust.
  - Default time zone is Asia/Colombo; every other default is nc-policy's. No hours set means no work (the
    nc-policy safe default), and Home says so.
  - The "never use paid data" switch on means strict zero outside the window. Off allows only Control traffic.
    Cellular fallback is always off. The boundary guard is 60 s. There is no monthly cap yet.
  - The connection binding is a placeholder (`owner-confirmation-pending`) until a platform adapter confirms the
    network, so the profile authorises no real transfer.
  - I dropped `path_provider`: Flutter plugins need symlinks (Windows Developer Mode, which is off). Rust's
    `settings_dir()` gives the folder instead (`%APPDATA%\NetworkConnector`; macOS and Linux paths too). Mobile
    returns an error, and the app then keeps settings in memory only.
- **Verified (real results):**
  - From core/: `cargo test -j 2 -p nc-bridge` reported 22 passed, 0 failed. `cargo fmt --all --check` exit 0.
    `cargo clippy -p nc-bridge --all-targets -- -D warnings` exit 0. `cargo deny check` reported "advisories ok,
    bans ok, licenses ok, sources ok".
  - From app/: `flutter analyze` reported "No issues found!". `flutter test` reported 12 passed and 1 skipped (the
    real bridge test). With `NC_BRIDGE_DLL` set to the release `nc_bridge.dll`, `flutter test
    test/rust_bridge_test.dart` passed: start, Stop (4465 us), validation, tariff status, and settings save, load
    and corrupt-to-.bad through real Rust.
  - `flutter build windows --release`: built `network_connector.exe` and `nc_bridge.dll` (2.1 MB).
  - Manual: launched the release exe with a corrupt `%APPDATA%\NetworkConnector\settings.json`. It was still
    running after 8 s, and the file had become `settings.json.bad`. I then removed the test folder.
- **Failures recorded:** the first `cargo test` died with rustc out-of-memory (STATUS_STACK_BUFFER_OVERRUN). C: was
  at about 1 GB, so I stopped as instructed and removed my target dir. Resumed at 5.85 GB with `-j 2`.
  `flutter pub get` with path_provider failed (it needs symlinks), so the dependency was removed.
- **Not done / open:** I did not do the click-through manual run (change limits in the window, restart, see them
  persist, press Stop): I cannot drive the GUI. Persistence is covered by the Rust tests and the real-bridge test.
  Owner steps are in the hand-back. Job counts on Home stay 0 until nc-agent reports jobs. Helping time resets when
  the app restarts. There is no settings folder on mobile yet.

## 2026-11-02 - Claude (coordinator) - `main`

- **Commits:** this merge of `feat/codex-ci-stack` (4fdfaa2).
- **Asked:** review Codex 1.P5 (CI Docker stack) and Codex's stop report on 1.P6.
- **Done:** merged 1.P5 and set it to done. 1.P6 set to blocked; added 1.P6a (real job contract in proto and server) as its prerequisite.
- **Decisions:** Codex was right to stop 1.P6. The coordinator only hands out dummy leases, with no manifest, blob or
  failure endpoints, and the agent must not invent wire semantics. The contract comes first.
- **Verified:** Review result for 1.P5: PASS. Actions pinned by SHA, `contents: read`, secrets from `openssl rand` and masked
  with `::add-mask::`, EXIT trap cleanup. Real Actions run 41341787288: stack job passed (healthz ok, uid 999, lease
  cycle 1 passed, cleanup done). Overall CI is still red because of the existing nc-identity Linux test.
- **Not done / open:** 1.P6a, then 1.P6; fix the CI failures on main (1.P7).

## 2026-11-02 - Codex - `feat/codex-ci-stack`

- **Commits:** 505942f (implementation), b1a6ec7 (workflow-context correction), this verification follow-up.
- **Asked:** add an Ubuntu full-stack CI check on main pushes/PRs with GHA build cache, runtime-masked
  credentials, health/non-root checks, the existing lease cycle and unconditional teardown.
- **Done:** added Docker stack job to existing CI (also runs on feature pushes for real verification).
  SHA-pinned official Docker Buildx/Bake actions build the Compose targets with separate GHA cache
  scopes and load their shared image. Added executable LF-only infra/verify.sh with process-only
  CSPRNG credentials, pre-use add-mask commands, health/UID checks, lease cycle and EXIT/signal cleanup.
  Added a separate always() cleanup step. Task 1.P5 set to review; no Rust or dependency edits.
- **Decisions:** contents: read only; checkout credentials not persisted; no image/build record published.
  Build/teardown parsing uses non-secret placeholders. Real credentials are generated only inside the
  verifier, never written to .env/GITHUB_ENV or echoed. Existing loopback bindings unchanged. Existing
  workflow events already cover main pushes/PRs and feature branches. All actions use full official SHAs.
- **Verified:** actual Ubuntu stack job PASSED at
  https://github.com/NavodCaldera/Network-Connector/actions/runs/41341787288/job/124953168299
  (run https://github.com/NavodCaldera/Network-Connector/actions/runs/41341787288, commit b1a6ec7).
  Build: release profile finished in 1m 54s; both targets exported GitHub Actions cache. Runtime:
  /healthz ok; coordinator/relay UIDs both 999. Existing lease cycle: 1 passed, 0 failed, 0 ignored
  (0.03s). Trap teardown removed three containers, network and both volumes; independent always-cleanup
  step and all action post steps also passed. Both server matrix jobs and both cargo-deny jobs passed.
  bash -n, git diff --check, LF-byte/action-pin checks and local Bake --print validation also passed.
- **Failures recorded:** initial run 41341498628 failed before scheduling jobs because runner.temp was
  used in job-level env, where runner context is unavailable (official GitHub contexts reference).
  Corrected to a fixed Ubuntu /tmp target path; subsequent stack job passed. Local actionlint download
  failed because C: is full; no files deleted or caches moved. Logs were fetched through the API into
  memory and sanitized, not saved locally. No run logs or secret files committed.
- **Not done / open:** overall CI is not claimed green: the separate Ubuntu core matrix fails
  corrupt_key_file_is_refused_not_replaced in nc-identity. Verified the same failure existed in baseline
  run 41339242475 before this task. That Rust test is outside the allowed files.
- **Requests:** core owner should fix the existing identity-test failure; owner should free C: space
  before further local tool installs. This task's Docker stack job is verified successfully.

## 2026-11-02 - Claude (coordinator) - `main`

- **Commits:** this merge of `feat/app-shell` (a1146f3, 91dda78).
- **Asked:** review app-agent task 0.8 (Flutter shell + bridge).
- **Done:** merged; TASKS.md 0.8 set to done. Worklog conflict resolved by keeping both entries.
- **Decisions:** coordinator ruling on invariant 11: the generated `nc-bridge/src/frb_generated.rs` FFI glue is exempt,
  because it is generated code with `unsafe` allowed only in that module. Nobody may write `unsafe` by hand in nc-bridge.
  Codex 1.P6 builds on the `nc-agent` facade from 0.8 and does not replace it.
- **Verified:** relied on the agent's real results: nc-agent/nc-bridge tests pass, flutter analyze clean, flutter test
  +4 with the real DLL, release build and a manual Give/Stop run (Stop 0 ms on screen). CI run 41341586005: everything
  green except the core test step, which fails the same way main already does (nc-identity
  `corrupt_key_file_is_refused_not_replaced` on Linux, 11 nc-exec sandbox tests on windows-latest).
  Coordinator did not rebuild locally: C: had 0.7 GB free.
- **Not done / open:** fix those CI failures on main (core). Use a separate CARGO_TARGET_DIR per parallel builder.
  Dart stream cancel needs a Rust-side unsubscribe. The Android Rust build (0.9a) is not done.

## 2026-11-02 - Claude (coordinator) - `main`

- **Commits:** this commit.
- **Asked:** owner: give Codex larger tasks; Claude does simple work and reviews.
- **Done:** added TASKS.md row 1.P6 (nc-agent supervisor) for Codex.
- **Decisions:** owner decision: Codex gets large, detailed tasks; Claude reviews and corrects.
- **Verified:** documentation only.
- **Not done / open:** 1.P6 not started.

## 2026-11-02 - Claude (coordinator) - `main`

- **Commits:** this commit; fast-forward of `feat/codex-infra` (ef2b8a7).
- **Asked:** review Codex task 1.P4 (Docker build after the proto move).
- **Done:** merged. Replaced the stale Docker follow-up in `proto/README.md`. TASKS.md 1.P4 set to done.
- **Decisions:** none new.
- **Verified:** Review result: PASS. Only infra/, TASKS.md and WORKLOG.md changed. `infra/verify.ps1` creates
  credentials with the OS random generator, keeps them in the process environment only, prints none, and always tears down.
  Dockerignore admits only the toolchain file, core manifest, nc-protocol, proto/ and server/, and excludes keys and .env files.
  Codex's real run: image built, /healthz ok, both services uid 999, `dummy_lease_cycle_over_http` passed, cleanup left 0 resources.
  Coordinator did not re-run Docker itself (C: has 7.9 GB free). Earlier open item closed: `D:\BuildTools` was uninstalled (exit 0),
  and `flutter doctor` now passes the Visual Studio check with Build Tools 17.14.44 on C:.
- **Not done / open:** executor IPC still not on proto (core, later).

## 2026-11-02 - Codex - `feat/codex-infra`

- **Commits:** this commit (task 1.P4).
- **Asked:** repair server image inputs after the protocol migration, preserve cacheability and the
  minimal non-root runtime, and verify the real Compose/HTTP lease cycle without secret files or logs.
- **Done:** infra/Dockerfile copies workspace/crate manifests first, fetches the locked server graph,
  then copies nc-protocol, proto/ and server/ in their repository-relative layout. BuildKit caches
  registry/Git downloads and target artifacts; only two finished binaries enter the runtime stage.
  Dockerfile.dockerignore allows the required core/proto inputs and excludes targets/metadata/secrets.
  Compose's existing repo-root context and every functional setting/port binding are retained.
  Added infra/verify.ps1 and updated README with process-only credentials, an isolated Compose project,
  health/non-root checks, optional cache rebuild and finally-block cleanup. Added only the 1.P4 task row.
- **Decisions:** server/Cargo.lock already locks the full graph including nc-protocol, so the unused
  core/Cargo.lock is not copied. Cache-mounted binaries are exported to /out before the runtime COPY.
  Verification credentials are CSPRNG-generated environment values, never files, and are restored after
  the run. The unique project's disposable volumes are removed on teardown; build caches remain on C:.
  No dependency manifests/pins changed. No cloud resources or wider bindings introduced.
- **Verified:** Docker Desktop initially had no engine pipe; CLI start reported already running but
  status was unavailable. Launching the installed app brought engine 29.3.1 online. Ran
  powershell -NoProfile -File infra/verify.ps1 -CheckCache successfully (exit 0): actual Compose build
  succeeded (Linux release compile 2m 39s; context 249.52 kB), second build reused cached layers,
  up -d --wait reported all services healthy, /healthz returned ok, coordinator/relay UIDs both 999.
  Existing cargo test --manifest-path server/Cargo.toml -p nc-coordinator --test lease_cycle -- --ignored
  passed: 1 passed, 0 failed, 0 ignored (0.16s). down --volumes --remove-orphans removed all three
  containers, both volumes and the network. A separate label query confirmed zero remaining resources.
  PowerShell parser and git diff --check passed; a functional Compose comparison confirmed every setting
  and port binding unchanged. No secret files or run logs created. Rust workspaces were not edited;
  full fmt/clippy/workspace suites were not rerun for this infrastructure-only change.
- **Not done / open:** none within the assigned build/lease-cycle scope.
- **Requests:** coordinator can remove the now-resolved Docker packaging follow-up from proto/README.md
  (outside the allowed files for this task).

## 2026-11-02 - Claude (coordinator) - `main`

- **Commits:** this commit; fast-forward of `feat/codex-proto` (14aebb9).
- **Asked:** review Codex task 1.P3 (move all wire types into `proto/`).
- **Done:** reviewed and merged. TASKS.md 1.P3 set to done.
- **Decisions:** none new.
- **Verified:** Review result: PASS. Diff stays inside its allowed files (27 files); no `TODO(proto)` markers left.
  core: fmt 0, clippy 0, 183 tests pass, cargo deny ok. server: fmt 0, clippy 0, 17 tests pass, cargo deny ok.
  VS Build Tools on C: now has the C++ workload (modify exit 0).
- **Not done / open:** infra Dockerfile must COPY `core/Cargo.toml`, `core/crates/nc-protocol/` and `proto/` (net-agent);
  Docker lease cycle not re-run. Executor IPC still not on proto (core, later). D:\BuildTools uninstall not done yet.

## 2026-11-02 ? Codex ? `feat/codex-proto`

- **Commits:** this commit (task 1.P3 protocol migration).
- **Asked:** make proto/ the sole definition of coordinator, pairing and relay wire messages; preserve
  coordinator JSON/signing and existing validation, and pin binary encodings with tests.
- **Done:** coordinator.proto, pairing.proto, relay.proto; prost generation only in nc-protocol; generated
  serde JSON bodies with UUID compatibility; handwritten API/frame/claims copies removed; generated
  nested errors and health response. Binary pairing, relay auth and signed relay token; redacted Debug
  for secret-bearing messages. Added 22 protocol tests, two pairing boundary tests, three relay token
  tests and the fixed request-signing vector. Updated both dependency lockfiles; 1.P3 set to review.
- **Decisions:** HTTP JSON shapes and signing bytes unchanged. P2P is canonical protobuf, rejecting unknown,
  duplicate and noncanonical fields recursively. Token claims use explicit presence to retain required
  fields, including zero timestamps. HTTP token text wraps a binary versioned token; clients and servers
  must update together. No removed protobuf tags exist yet; schema comments/README require reservations
  for future removals. All five active source migration markers resolved; two historical work-log
  mentions retained. All build/temp/cache paths used C: and the shared C:\dev\nc-target.
- **Verified:** in BOTH core/ and server/: cargo fmt --all --check passed; cargo clippy --all-targets --
  -D warnings passed; cargo test passed; cargo deny check passed (advisories/bans/licenses/sources ok).
  Core: 180 tests plus three doctests passed, one existing launch-measurement test ignored. Server:
  17 tests passed, one Docker lease-cycle test ignored. Deny reported duplicate-version warnings only
  (21 core, 36 server). Golden offer/token bytes, all new message round trips, JSON shapes/unknown fields,
  pairing lockout/SAS/impostor/single-use, authenticated relay reservations and token timing all passed.
  Initial checks found a wrong server relative dependency path and deprecated prost error construction;
  both corrected. Clippy caught an unnecessary closure; fixed and all checks rerun. One correction script
  used the wrong working directory and failed before edits, then was rerun correctly. A Markdown write
  doubled CRLFs; after review rejected a broad repair, read-only checks proved only the row/entry content
  differed, and the files were reconstructed from unchanged Git bytes plus those narrow edits.
- **Not done / open:** Docker engine unavailable even outside the sandbox (dockerDesktopLinuxEngine pipe
  absent), so no compose resources were started and the ignored HTTP/database cycle was not run. The
  current Dockerfile/ignore file omit the newly required core/proto sources and need their owner's update.
  nc-exec's private NCX1/NCR1 process IPC remains handwritten: the task explicitly forbids editing it.
  Linux/macOS execution not performed; old JSON P2P encodings are intentionally unsupported after migration.
- **Requests:** infrastructure owner must include core/Cargo.toml, core/crates/nc-protocol/ and proto/ in
  Docker context/COPY steps, then rebuild and rerun the lease cycle. Executor owner should migrate private
  worker IPC separately. New local caret dependencies: nc-protocol serde 1, uuid 1 (serde), base64 0.22,
  test-only serde_json 1; server nc-relay prost 0.14. Existing core prost/hex dependencies use workspace
  inheritance; nc-protocol is referenced by path. No new exact pins and no workspace root manifests changed.
## 2026-11-02 · Claude Code (app-agent) · `feat/app-shell`

- **Asked:** Phase 0 task 0.8: a Flutter shell on Windows that calls the Rust agent through flutter_rust_bridge.
  Pass condition: start and stop a helper session from the UI.
- **Done:**
  - `app/`: created with `flutter create` (windows and android, org `lk.networkconnector`). One Material 3
    light/dark screen in `lib/src/home_page.dart` with the roles, policy summary, Accept jobs switch, live status
    and an always-visible Stop button. `lib/src/agent_api.dart` is the interface plus the real bridge
    implementation. All text is in `lib/src/strings.dart`.
  - Windows build: `windows/CMakeLists.txt` builds and bundles `nc_bridge.dll`; the window title is
    "Network Connector".
  - Tests: `test/widget_test.dart` (3 tests); `test/rust_bridge_test.dart` (real DLL round trip, skipped without
    `NC_BRIDGE_DLL`).
  - `core/crates/nc-agent`: new `src/lib.rs` `Agent` facade (start, stop, watch_status) over nc-policy, with a
    placeholder session loop and 7 tests, including stop latency.
  - `core/crates/nc-bridge`: `api.rs` wraps the facade 1:1, plus the generated `frb_generated.rs` and a `build.rs`
    for the `frb_expand` cfg.
  - `.github/workflows/ci.yml`: new `flutter` job (pub get with the lockfile enforced, analyze, test) on Flutter
    3.47.9, with subosito/flutter-action pinned by SHA (v2.23.0).
  - TASKS 0.8 is now review; REPORT 0.8 is written.
- **Decisions (app-agent):**
  - flutter_rust_bridge 2.13.0 pinned exactly (2.14 is beta only).
  - The Rust build is driven from CMake, not cargokit, because the crate lives in `core/`.
  - Status is forwarded on a dedicated thread so the bridge never holds a worker.
  - Changing "Accept jobs" while running restarts the session, so withdrawing consent applies at once.
- **Verified:**
  - Rust (nc-agent, nc-bridge):
    - `cargo test -p nc-agent -p nc-bridge` passed: 7 + 1 + 3 tests. Stop latency was 203.8 µs, worst of 20.
    - `cargo fmt --all --check` passed.
    - `cargo deny check`: advisories, bans, licenses and sources all ok.
    - `cargo clippy --all-targets -D warnings` passed on the workspace excluding nc-protocol, nc-exec and
      nc-identity.
  - Rust (whole workspace):
    - The full workspace clippy/test failed. Codex's parallel 1.P3 build overwrote nc-protocol's generated
      `OUT_DIR` in the shared `C:\dev\nc-target` (serde derives, E0433). Not caused by this change.
    - The retry in a private target dir hit a full C: (0 bytes free) and was deleted.
    - The final `cargo test` (excluding those three crates) failed linking nc-netprobe (LNK1140) because C: was
      full. So whole-workspace `cargo test` is **not verified locally**.
  - CI run 41341586005 on a1146f3:
    - Passed: flutter (analyze, test), cargo-deny core and server, rust server on both OSes.
    - rust core: fmt and clippy passed on Windows and Linux. The test step **failed** on failures main already
      has (main run 41341116045 fails the same way): nc-identity `corrupt_key_file_is_refused_not_replaced` on
      Linux, and 11 nc-exec sandbox tests on windows-latest.
    - nc-agent and nc-bridge tests, including `stop_latency_under_one_second`, passed on both OSes.
  - Flutter:
    - `flutter analyze`: no issues.
    - `flutter test`: 4 passed with the real DLL (real round trip stop 4.1 ms); 3 passed and 1 skipped without it.
    - `flutter doctor`: Visual Studio OK.
    - `flutter build windows --release` succeeded: exe 91,136 B, nc_bridge.dll 504,320 B, Release folder
      27,343,460 B.
  - Manual: real mouse clicks on the release app. Give showed "Running, but not accepting jobs."; Stop showed
    "Stopped. ... Stopped in 0 ms.".
- **Not done / open:**
  - The window-title change in `main.cpp` and README edits came after the last build and were not rebuilt.
  - Windows UI Automation sees only `FLUTTERVIEW`.
  - The `unsafe` in the generated bridge glue needs a coordinator ruling (rule 11).
  - Dart stream cancel only completes at the next status change.
  - No Android Rust build yet (0.9), no signing or installer.
  - Deleted my own `C:\dev\nc-target\release` (nc_bridge only) and the temporary target dir to free space.

---

## 2026-11-01 · Claude Code (coordinator) · `main`

- **Asked:** review Codex task 1.P2 (nc-blob); cut disk use without changing runtime speed.
- **Done:** merged `feat/codex-blob` (dfc6816). Conflicts: TASKS.md and Cargo.lock resolved with main's versions and the lockfile
  regenerated. Restored the TASKS.md "Phase 1 prep" section, which was lost in the net merge, and added 1.P3 for
  Codex. `core/` and `server/` Cargo.toml: `[profile.dev]`/`[profile.test]` `debug = "line-tables-only"`, dependencies
  `debug = false`. This affects debug info only, not optimisation; release builds are unchanged.
- **Review result: PASS.** Scope: `core/crates/nc-blob/**`, Cargo.lock, TASKS.md, WORKLOG.md. Deps use workspace
  versions or caret (chacha20poly1305 0.10.1, serde 1). After `cargo clean`: fmt 0; workspace clippy `-D warnings` 0;
  nc-blob 15 + doctest passed; deny ok. The build cache was **1.02 GB** after that (previously 9.2 GB with full debug info);
  C: has 19.3 GB free.
- **Not done / open:** power-loss durability (no fsync of the directory) and Linux/macOS runs are untested for nc-blob.

## 2026-11-01 · Codex · `feat/codex-blob`

- **Commits:** this commit (task 1.P2 implementation and hand-back).
- **Asked:** build a synchronous, chunked, content-addressed, resumable encrypted local blob cache.
- **Done:** nc-blob model/store modules, documented storage/API contract, 15 unit/property tests and one
  doctest; dependency lock update; TASKS.md 1.P2 set to review. SHA-256 manifests, XChaCha20-Poly1305,
  per-blob/index authentication, bounded reads, atomic publication, quota, root locking and GC.
- **Decisions:** put/has/missing take an explicit key so writes encrypt and resume authenticates. Quota
  includes staging and encryption overhead; replacement requires temporary headroom. Private local root
  required; manifests and keys are caller-managed. All filenames are hex hashes. C: used for tool caches,
  target and temp paths per owner rule. No network wire schema added.
- **Verified:** cargo fmt --all --check passed; whole-workspace cargo clippy --all-targets -- -D warnings
  passed; cargo test -p nc-blob passed (15 tests plus one doctest, including two 48-case properties);
  cargo deny check passed (10 duplicate-version warnings; advisories/bans/licenses/sources all ok).
  Initial 13-test suite passed. A subsequent plaintext-copy refactor introduced a tuple typo (E0599/E0308);
  corrected it and reran final Clippy and blob tests successfully. Full workspace cargo test passed (141 tests and 3 doctests; one existing launch-measurement test ignored).
- **Requests:** local caret dependencies not in workspace.dependencies: chacha20poly1305 0.10.1,
  serde 1 (derive), serde_json 1 (tests). Existing shared dependencies inherit workspace versions.
- **Not done / open:** Linux/macOS execution and real power-loss tests not performed. Atomic rename is not
  a power-loss durability guarantee. Owner policy/tariff enforcement and secure key/manifest persistence
  remain caller responsibilities. Kept-blob crash staging counts toward quota until blob removal.

---

## 2026-11-01 · Claude Code (coordinator) · `main`

- **Asked:** review and merge net-agent tasks 0.5–0.7; clean up disk use (owner: tools on C: only).
- **Done:** merged `feat/net-p2p` (7a62a82) and `feat/net-server` (82bb534). TASKS/WORKLOG/Cargo.lock conflicts
  resolved with main's versions and the lockfile regenerated. `deny.toml`: ignored RUSTSEC-2024-0436 (`paste`,
  unmaintained, no vulnerability, transitive via libp2p if-watch) with the reason recorded. Disk: deleted
  `D:
c-target` (18.3 GB), duplicate Rust copies on D: (2.2 GB) and `~/.gradle` (2.3 GB). User env
  `CARGO_TARGET_DIR=C:\dev
c-target` (one shared build folder).
- **Review result: PASS (local).** Coordinator ran it on the merged tree. core: fmt 0, clippy `-D warnings` 0, 30 test
  suites, exit 0, deny ok. server: fmt 0, clippy 0, 9 suites, exit 0, deny ok. The Docker lease cycle and probe
  numbers are the agent's; the coordinator did not re-run them.
- **Not done / open:** two-ISP test (owner, `spikes/phase0/net/RUN.md`) needs a relay reachable from both ISPs:
  a public-IP machine or a paid VM (owner decision). TLS/443 fallback rung not built. Relay token peer-id binding
  at registration not built. Wire types still local (TODO(proto)). D:\BuildTools still to be reinstalled on C:.

## 2026-11-01 · Claude Code (net-agent) · `feat/net-p2p` + `feat/net-server`

- **Asked:** Phase 0 tasks 0.5 (libp2p two-ISP test kit), 0.6 (mDNS + QR pairing in `nc-net`), 0.7 (minimal
  coordinator + relay, local Docker).
- **Done:**
  - `feat/net-p2p`: `core/crates/nc-net` on rust-libp2p 0.57.1 (pinned): QUIC + TCP/Noise/Yamux, relay client,
    AutoNAT, DCUtR, identify, ping, mDNS. QR pairing with nonces, HMAC proofs bound to both peer keys and a 6-digit
    SAS confirmed on both sides; single-use 10-min offers, lock after 5 bad proofs. Trust store keyed by peer id
    (optional file). Connection gate closes unpaired peers; mDNS only dials trusted peers. Stream API for Core keyed
    by peer id (`open_stream`/`accept`). Ladder rung per connection and direct/relayed byte counters. Probe binary
    `nc-netprobe` (`id`, `relay`, `listen`, `dial` + throughput) and `spikes/phase0/net/RUN.md` for the owner.
  - `feat/net-server`: `nc-coordinator` (Axum + sqlx/PostgreSQL, migrations; signed requests with replay check,
    enrolment secret, 16 KiB cap, schema validation, one error format; register, heartbeat, lease
    request/complete with idempotent attempt-checked commit, relay token). `nc-relay` (relay v2, coordinator
    tokens bound to peer id, per-peer and per-account quotas, refuses to start without a token key).
    `infra/docker-compose.yml`, `infra/Dockerfile`, `.env.example`, README.
  - REPORT.md sections 0.5 (local results, owner run pending), 0.6, 0.7; TASKS.md rows 0.5-0.7 set to review.
- **Decisions (net-agent):** coordinator wire structs live in `nc-coordinator/src/api.rs` with TODO(proto)
  (requested move to `proto/`). Phase 0 enrolment uses an owner secret instead of OIDC. Relay byte quota charges
  each circuit its full cap (libp2p does not report circuit bytes). A relay circuit is closed 3 s after a direct
  path exists, never during pairing. The probe binds 0.0.0.0 by default (needed for LAN/internet tests; `--bind`
  narrows it). Cargo home, target and TEMP were put on D: because C: was full.
- **Verified (real output):** core: `cargo fmt --check` ok, `cargo clippy --all-targets -D warnings` clean,
  `cargo test` all passed (nc-net: unit 6, loopback 9, relayed 1). server: fmt ok, clippy clean, `cargo test`
  13 passed + 1 ignored; compose stack up, `lease_cycle -- --ignored` passed, psql shows 1 committed task.
  Probe self-test on loopback: relay reservation, relayed connection, pairing, DCUtR ok, test over direct path.
  `cargo deny check`: bans/licenses/sources ok in both workspaces; **advisories FAILED** in both on
  RUSTSEC-2024-0436 (`paste` unmaintained, via libp2p `if-watch` -> `netlink-packet-core`, Linux only).
- **Not done / open:** two-ISP measurements (owner run, `spikes/phase0/net/RUN.md`; test 3 needs a reachable relay,
  and a cloud VM needs owner approval). mDNS between two physical PCs not run. TLS/443 fallback rung not
  implemented. No TLS cert tested on the coordinator. Relay not exercised from `nc-net` through the compose
  ports. `deny.toml` needs an ignore entry for RUSTSEC-2024-0436 (App agent owns it). During the work the PC ran
  out of memory and C: space several times (rustc and the linker crashed); builds used `-j 1`/`-j 2`.

## 2026-11-01 · Claude Code (coordinator) · `main`

- **Asked:** review Codex task 1.P1 (nc-policy).
- **Done:** merged `feat/codex-policy` (e143783), no conflicts. Replaced Codex's exact pins with workspace versions
  (same clash as nc-tariff). Moved the Android SDK to `C:\dev\Android\Sdk` and the gradle home to `C:\dev\gradle`
  (owner: tools on C: only); removed small leftovers on D:.
- **Review result: PASS.** Scope limited to `core/crates/nc-policy/**`, TASKS.md, WORKLOG.md. Coordinator checks on
  the merged tree (target `C:\dev
c-target`): fmt ok; workspace clippy `-D warnings` ok; nc-policy 28 + doctest
  passed, nc-tariff 26 passed; `cargo deny` ok. Read `admit`: blocked beats trusted, untrusted rejected, safe
  checked arithmetic, temporary restrictions last; property test covers 20 hard rules.
- **Not done / open:** OS-level enforcement belongs to nc-agent (Phase 1). Build Tools, Rust caches and the 15.8 GB
  build cache are still on D:, waiting for the net-agent.

## 2026-11-01 · Codex · `feat/codex-policy`

- **Commits:** this commit (task 1.P1 hand-back).
- **Asked:** implement the pure owner-policy engine in the separate nc-codex-b checkout, validate, commit and push; no merge, PR or committed run logs.
- **Done:** added versioned/default-closed serde OwnerPolicy, fixed-length hex PeerKey, validated working/resource/sensor/peer/runtime rules, offer and complete running-task snapshots, owner-readable Reason enums, admit/check_running/next_change APIs, shared nc-tariff working-hour/calendar calls and real transfer-decision gating. Added integration README, 28 unit/property tests (including two 256-case proptests) and one doctest. Set only task 1.P1 to review.
- **Decisions:** Codex: blocked beats trusted; hard revocations/critical thermal/expired tasks Stop before temporary Pause rules. Unknown thermal always blocks; battery is required when minimum > 0, including on AC (owner can choose 0 for batteryless devices). Thermal thresholds are inclusive. Running-task vector includes authenticated keys/profile/start times and counts paused tasks. Working hours reuse nc-tariff's conservative DST behavior through a private no-transport projection; real transfers independently require fresh tariff decisions/reservations. CPU percent is best effort; supervisor owns actual enforcement and one-second Stop.
- **Verified:** every shell set CARGO_TARGET_DIR=D:\nc-target\codex, TEMP=D:\tmp and TMP=D:\tmp; Cargo cache also on D:. From core/: cargo fmt --all --check exit 0; cargo clippy -p nc-policy --all-targets -- -D warnings exit 0; cargo test -p nc-policy: 28 passed, 0 failed plus one passing doctest; cargo deny check: advisories ok, bans ok, licenses ok, sources ok. AGENT.md workspace-wide strict Clippy and cargo test also passed. Production source scan found no unwrap/expect calls or unsafe code; all public items have enforced doc comments. Actual output will be pasted in the hand-back, not committed as a run log.
- **Failures corrected:** initial formatting rejected a PowerShell-encoded non-ASCII test fixture; replaced it with an ASCII Rust Unicode escape and UTF-8 source. Clippy required fixed-size as_chunks parsing instead of chunks_exact. All final checks passed. No os error 112 occurred. Git's global-ignore permission warning was nonfatal.
- **Requests:** exact local dependencies: nc-tariff path ../nc-tariff version 0.1.0; chrono 0.4.42, chrono-tz 0.10.4, serde 1.0.228, thiserror 2.0.17; dev serde_json 1.0.145 and proptest 1.9.0. Coordinator must regenerate/review core/Cargo.lock (only the nc-policy dependency list changed during validation); generated changes were restored to HEAD for ownership compliance. core/Cargo.toml is unchanged.
- **Not done / open:** no supervisor/platform/transport implementation, cryptographic authentication, persistence, atomic resource reservation, CPU throttling or executor teardown; those remain integration responsibilities. Stateless snapshots cannot detect every backward clock jump or observation staleness, so the supervisor must detect events and enforce a monotonic watchdog. No files outside nc-policy plus this log and the assigned task row are included; no run logs, merge or PR.

## 2026-11-01 · Claude Code (coordinator) · `main`

- **Asked:** review app-agent task 0.9a (Android spike); the owner freed C: and asked that all tools live on C: only.
- **Done:** merged `feat/app-android` (e3fa591, 17 files under `spikes/phase0/android/`, no conflicts). Added LF line
  endings for `*.sh` and `gradlew` in `.gitattributes`. TASKS.md 0.9a stays in progress.
- **Review result: PARTIAL.** The agent reported host tests passing (2/2 per runtime feature: Pulley, wasmi, Cranelift).
  No `.so` or APK was built (disk), nothing ran on a device, and fmt/clippy were not re-run on the final state. The
  coordinator did not re-run these spike tests.
- **Not done / open:** build the `.so` files and APKs after the tools move to C:; the owner runs `RUN.md` on an ARM64
  phone. Planned move of everything on D: (Android SDK, Build Tools, Rust caches, gradle) to C:, waiting for the
  net-agent to finish.

## 2026-11-01 · Claude Code (App agent) · `feat/app-android`

- **Asked:** task 0.9a, Android spike: Wasm job in a foreground service with a Stop button, three runtimes compared.
- **Done:** `spikes/phase0/android/`: `wasm-spike` (Rust cdylib, features `rt-pulley`, `rt-wasmi`, `rt-cranelift`;
  Pulley bytecode precompiled on the host in `build.rs`), `app-spike` (Kotlin app, `specialUse` foreground service,
  notification Stop, JNI bridge, one flavor per runtime), `build-so.sh`, and `RUN.md` (owner test steps on a phone).
- **Decisions:** Pulley (interpreter, no JIT on device) is the v1 candidate; Cranelift is built for size comparison only.
- **Verified:** on the Windows host, `cargo test --release` passes for all three features (2/2 each:
  `sums_match_native`, `stop_halts_within_one_second`), including loading host-precompiled Pulley bytecode.
- **Not done / open:** the Android `.so` files and APKs were **not** built in this run (no APK exists; disk has about
  4 GB free on D: and E:). No phone or emulator was available, so there are no on-device numbers (Stop latency,
  Doze, OEM killing, APK size). fmt/clippy were not re-run in this session. The owner needs to run `RUN.md`.

## 2026-11-01 · Claude Code (coordinator) · `main`

- **Asked:** review and merge core-agent tasks 0.2 (executor + Windows sandbox) and 0.3 (identity, manifests, store).
- **Done:** merged `feat/core-store` (147e9f7, which contains `feat/core-exec` 856040c). Conflicts: `core/Cargo.lock`
  took the branch version and was regenerated; TASKS.md kept main's board with 0.2/0.3 set to done. Fixed a
  dependency clash: Codex's exact pins in `nc-tariff` (`proptest =1.9.0`, `thiserror =2.0.17`) conflicted with the
  workspace, so `nc-tariff` now uses the workspace versions and caret ranges.
- **Review result: PASS.** On the merged tree in `core/`: fmt ok; `clippy --all-targets -D warnings` exit 0;
  `cargo test` exit 0 with all suites ok (incl. nc-exec runtime limits 12, Windows sandbox 12 + 1 ignored,
  refusals 9, nc-tariff 26); `cargo deny check` "advisories ok, bans ok, licenses ok, sources ok". The agent's
  sandbox escape and Stop-latency numbers are recorded in spikes/phase0/REPORT.md; the coordinator did not re-measure them.
- **Not done / open:** no Linux/macOS sandbox (jobs refused there); cold first worker launch 2.1–4.8 s (call
  `warm_up()` at start); `C:\Windows\win.ini`-style world-readable files are visible in the AppContainer; one
  unexplained 62 s watchdog kill under load. TPM Ed25519 support not checked.

## 2026-11-01 · Claude Code (core-agent) · `feat/core-store` (based on `feat/core-exec`)

- **Asked:** Phase 0 task 0.3: `nc-identity` (Ed25519 root key, software keystore + adapter trait, session certs),
  `proto/` TaskManifest with prost + vendored protoc, `nc-protocol` manifest sign/verify, `nc-store` SQLite lease
  state machine with property tests.
- **Done:** `proto/nc/v1/manifest.proto`, `proto/nc/v1/identity.proto`. `nc-protocol`: `build.rs` (prost-build +
  protoc-bin-vendored), `sign.rs` (domain-separated Ed25519, `verify_manifest`), tests incl. proptests.
  `nc-identity`: `DeviceKeyStore`, `RootSigner`, `SoftwareKeyStore` (DPAPI on Windows, 0600 elsewhere, the only
  `unsafe` is in `platform.rs` with SAFETY comments), session certs ≤ 24 h. `nc-store`: migrations, `Store` with
  enqueue/lease_next/start/renew/upload/verify/commit/fail/expire_leases, unit + property tests. `nc-exec`:
  `Ed25519Publishers` + `Job::from_manifest` (closes the 0.2 signature TODO). Workspace deps added. REPORT.md §0.3.
  TASKS.md 0.3 → review.
- **Decisions:** core-agent: signatures always cover the exact received bytes with a per-type domain prefix; nothing
  is re-encoded before verification. Commit of an already-committed task by the same attempt returns
  `AlreadyCommitted` (idempotent); any other attempt gets `StaleAttempt`. A corrupt key file is an error, never
  replaced with a new key. Time is a parameter (`now_ms`) in the store so the tests are deterministic.
- **Verified:** fmt, clippy `-D warnings`, `cargo test`, `cargo deny check`: see the hand-back and REPORT.md §0.3
  (manifest 11, identity 9, lease 10 incl. 2 proptests × 300 cases, refusals 9). `tpmtool getdeviceinformation`:
  TPM 2.0 (Intel) present and ready.
- **Not done / open:** no hardware keystore adapter yet (TPM, Android, Secure Enclave); not checked whether the TPM
  can hold Ed25519. Linux key file protection is only 0600. The unix code paths were not run on this PC (CI runs
  them).

## 2026-11-01 · Claude Code (core-agent) · `feat/core-exec`

- **Asked:** Phase 0 task 0.2: `nc-exec` Wasmtime executor (`cpu-core-v1`) with fuel, epoch, external watchdog,
  memory/stack/output caps, separate worker process inside AppContainer + Job Object on Windows, refuse if any layer
  fails, teardown + start-up sweep, Stop < 1 s, hash check, malicious-job tests.
- **Done:** `core/crates/nc-exec`: `runtime.rs` (Wasmtime 50.0.1 pinned; import allowlist `nc.*` only, no WASI;
  fuel, epoch tick thread, `ResourceLimiter`, `max_wasm_stack`, output/checkpoint caps), `ipc.rs` (length-capped
  stdin/stdout framing to the worker), `bin/nc-exec-worker.rs`, `executor.rs` (hash + `ModuleVerifier` checks before
  spawn, 5 ms watchdog/cancel loop, capped pipe reader, suspended start), `sandbox/windows.rs` (AppContainer with 0
  capabilities, Job Object with memory cap / 1 process / kill-on-close / UI limits / completion port,
  child-process policy, `JOB_LIST`, teardown in `Drop`, `sweep_leftovers()` via the registry, `warm_up()`),
  `sandbox/unsupported.rs` (non-Windows: refuse). Tests: `tests/runtime_limits.rs`, `tests/refusals.rs`,
  `tests/sandbox_windows.rs`, test-only `examples/sandbox_probe.rs`. `core/Cargo.toml` workspace deps added.
  REPORT.md §0.2 written. TASKS.md 0.2 → review.
- **Decisions:** core-agent: worker IPC is a private framing, not in `proto/` (same binary, never on the network).
  No permissive `ModuleVerifier` exists in the crate; Ed25519 verifier comes with 0.3. Worker starts suspended so a
  Stop during a slow `CreateProcessW` never runs guest code. Ported the legacy spike's launch recipe
  (`legacy/python-v0.3:spikes/0.3-sandbox`).
- **Verified:** on this PC: fmt ok; clippy `--all-targets -D warnings` clean; `cargo test` all green (nc-exec: 5 unit,
  5 refusals, 12 runtime, 12 sandbox + 1 ignored measurement); `cargo deny check` advisories/bans/licenses/sources ok
  (6 duplicate-version warnings). Cancel → return 7–150 ms; escape probe denied files, loopback, internet, DNS and
  child processes. Numbers in REPORT.md §0.2.
- **Not done / open:** no Linux/macOS sandbox (jobs refused there). Cold first launch of a new worker binary took
  2.1–4.8 s inside `CreateProcessW` (cause not proven); `run()` cannot return before that call does. One warm run under
  heavy machine load hung until the watchdog killed it at 62 s; cause not confirmed. C: had 0 bytes free and commit
  memory ran out several times (rustc "paging file too small"); builds used `CARGO_HOME`/`TEMP` on D: and `-j 1/2`.

## 2026-11-01 · Claude Code (coordinator) · `main`

- **Asked:** review Codex task 0.4 (nc-tariff).
- **Done:** merged `feat/codex-tariff` (90a8a11) into `main`. Removed the committed `VALIDATION.txt` (a run log,
  not source).
- **Review result: PASS.** Scope: only `core/crates/nc-tariff/**`, TASKS.md and WORKLOG.md changed. Coordinator re-ran
  the checks in a clean worktree: fmt ok; clippy `-D warnings` ok; `cargo test -p nc-tariff` gave 26 passed, and the
  doctest passed after setting TEMP to D: (it first failed with link error 1140 because C: had 0 MB free); the night
  simulator passed (90 nights, no window or budget breach). Read `decision.rs`: the network binding is checked first,
  a backwards clock is refused, the boundary guard is a hard exclusion, strict mode holds, and allowances are capped
  by budget, window and wire capacity, with saturating arithmetic.
- **Not done / open:** `core/Cargo.lock` was not regenerated, because cargo failed with os error 112 (C: full, the
  cargo registry is on C:). The coordinator's `cargo deny` output was lost; Codex reported it ok. **C: is at 0 MB free;
  this blocks builds.**

## 2026-11-01 · Codex · `feat/codex-tariff`

- **Commits:** this commit (Phase 0 task 0.4 hand-back).
- **Asked:** implement the pure nc-tariff library, ledger, window planner and synthetic night simulation in a separate checkout; validate, commit and push without merging or opening a PR.
- **Done:** only `core/crates/nc-tariff/**`, this log and TASKS.md row 0.4 changed. Added serde profile types; IANA/weekly cross-midnight calendar and conservative DST handling; class/binding/throughput/guard decisions; mutex-atomic reservations with actual-byte overage reports; monthly resets and backward-clock rejection; deterministic serial planning/jitter; pure simulator and `night` example; 26 tests (including two 128-case proptests), one doctest, integration README and real command transcript `core/crates/nc-tariff/VALIDATION.txt`. Task 0.4 is review.
- **Decisions:** Codex: windows reject overlap/equal endpoints; ambiguous starts use later time, ends earlier time, nonexistent endpoints skip occurrences. Reserve rounds upwards. Settle outstanding reservations before renewal; IDs never repeat. A profile's remaining allocation renews monthly (no provider query). Planner is provisional/serial, not cost-optimal; callers must reauthorize chunks on network/clock/throughput changes and enforce consent/power/thermal/Stop. No real ISP names/packages or performance claims.
- **Verified:** all shells set `CARGO_TARGET_DIR=D:\nc-target\codex`; Rust `1.99.1 (58cf8f9c3 2026-10-13)`, Cargo cache on D:. From `core/`: `cargo fmt --all --check` exit 0; `cargo clippy -p nc-tariff --all-targets -- -D warnings` exit 0; `cargo test -p nc-tariff` reports `26 passed; 0 failed` plus `1 passed; 0 failed` doctest; `cargo run -p nc-tariff --example night` reports `EXAMPLE ONLY: nights=90, completed=335, deferred=2545, committed_bytes=1399813443; window/budget assertions passed`; `cargo deny check` reports `advisories ok, bans ok, licenses ok, sources ok`. AGENT.md workspace-wide `cargo clippy --all-targets -- -D warnings` and `cargo test` also exit 0 (36 unit tests plus one doctest). `git diff --check` passed.
- **Failures corrected:** initial Clippy found unused TimeZone import and an eight-argument planner; grouped jitter configuration and fixed test call sites after E0061. First cargo-deny rejected serde_yaml_ng's unsafe-libyaml with RUSTSEC-2024-0449; replaced it with serde-saphyr 2.0.0, then all final checks passed. Git emitted a nonfatal global-ignore permission warning. No policy exceptions added.
- **Requests:** coordinator to regenerate/review `core/Cargo.lock` when integrating the exact crate-local dependency pins: chrono 0.4.42, chrono-tz 0.10.4, serde 1.0.228, thiserror 2.0.17; dev serde_json 1.0.145, serde-saphyr 2.0.0, proptest 1.9.0. Cargo generated the shared lockfile during checks; it was restored to HEAD to honor file ownership, so locked builds need the coordinator's update. `core/Cargo.toml` and deny.toml are unchanged.
- **Not done / open:** persistence/crash recovery, trusted platform observations, socket cancellation and real-provider billing reconciliation belong to integration; this library has no I/O/networking/unsafe and cannot enforce a socket Stop itself. No real-device/ISP measurements. Flutter/server were not touched. No merge or PR.

## 2026-11-01 · Claude Code (coordinator) · `main`

- **Asked:** review Codex's tasks 0.9b and 0.10 and plan Codex's next task.
- **Done:** reviewed `feat/codex-mobile-research` (c5345e3) and merged it into `main`. The TASKS.md conflict was
  resolved by keeping main's board and marking 0.9b and 0.10 done. Added `.claude/worktrees/` to `.gitignore`.
- **Review result: PASS.** Only the 4 allowed files changed. Every claim cites an official source with a check date,
  and unknowns are stated rather than guessed. Verdicts: iOS requester **GO**, bounded helper **GO-WITH-LIMITS**
  (bundled kernels only), overnight helper **NO-GO**; Android helper sessions **GO-WITH-LIMITS** (`specialUse`
  FGS, interpreted Wasm only, no mining). These match PLAN.md §1. No links were opened to re-verify them.
- **Not done / open:** the research does not replace real-device checks (0.9a is still running).

## 2026-11-01 · Codex · `feat/codex-mobile-research`

- **Commits:** this commit (Phase 0 mobile research hand-back).
- **Asked:** research tasks 0.9b and 0.10 only in a separate checkout; commit and push without a PR or merge.
- **Done:** added `spikes/phase0/ios-feasibility.md` and `spikes/phase0/mobile-policy.md` with linked, dated official evidence, explicit unknowns, policy/runtime distinctions and conditional role decisions. Set only TASKS.md rows 0.9b/0.10 to review.
- **Decisions:** Codex recommendation: iPhone requester/monitor GO, bounded bundled-kernel helper GO-WITH-LIMITS, unattended overnight helper NO-GO; Android sessions GO-WITH-LIMITS pending runtime containment and Play service review. No security invariant relaxed.
- **Verified:** cloned into `E:\Projects\New\nc-codex`, created the requested branch, read AGENT.md/PLAN.md/TASKS.md, checked current official Apple/Android/Play/Wasmtime/WAMR sources via web tools on 2026-11-01. Don't Kill My App used only as the requested secondary OEM source. `git diff --check` passed; TASKS.md diff contains only the two status edits. Git emitted a nonfatal permission warning reading the user's global ignore file.
- **Not done / open:** no code/workspace/app touched, so cargo fmt/clippy/test and Flutter checks are not applicable. No phone/Mac builds, benchmarks, sandbox validation or store approvals; wasm3 current feasibility unknown under the permitted sources. Task 0.9a remains with app-agent. Coordinator must review before merge.

## 2026-11-01 · Claude Code (coordinator) · `main`

- **Asked:** review and merge task 0.1; start the parallel Phase 0 tasks.
- **Done:** reviewed app-agent hand-back for 0.1 (40 files, skeleton only), merged `feat/app-skeleton` into `main`
  (053ab24; TASKS.md/WORKLOG.md conflicts resolved with the branch versions, which already contained the
  coordinator commit). Set user env `CARGO_TARGET_DIR=D:
c-target` because C: has 1.4 GB and E: 3.8 GB free.
  TASKS.md: 0.1 done; 0.4 ready for Codex; 0.2, 0.3, 0.5–0.7, 0.9a in progress; 0.8 blocked.
- **Verified:** relied on the agent's CI run 41292475565 (all 6 jobs green) and its local results; did not re-run.
- **Not done / open:** 0.8 blocked until the owner frees ≥3 GB on C: and the VS "C++ CMake tools" component is added.

## 2026-11-01 · Claude Code (app-agent) · `feat/app-skeleton`

- **Asked:** Phase 0 task 0.1: install toolchains, create the `core/` and `server/` Cargo workspaces, add
  `rust-toolchain.toml`, `deny.toml` and CI for Windows and Linux.
- **Done:** installed rustup 1.29.1, Rust 1.99.1 (MSVC), MSVC Build Tools 2022 17.14 (minimal: x64 tools + Win11 SDK,
  at `D:\BuildTools`), cargo-deny 0.20.2, Flutter 3.47.9 (`C:\src\flutter`, added to user PATH). Created
  `core/` (nc-protocol, nc-identity, nc-policy, nc-tariff, nc-store, nc-blob, nc-exec, nc-net, nc-bridge libs;
  nc-agent bin; nc-cli bin `nc`) and `server/` (nc-coordinator, nc-relay bins), each with a crate doc comment and one
  test. Shared `[workspace.package]` (edition 2021, MIT, rust-version 1.99), placeholder `[workspace.dependencies]`,
  `[workspace.lints]` with `unsafe_code = "deny"`. `proto/` README + placeholder `nc/v1/common.proto`; `infra/`
  and `app/` READMEs; `spikes/phase0/REPORT.md` with headings 0.1–0.10 and the 0.1 section filled in.
  `rust-toolchain.toml`, `deny.toml`, `.github/workflows/ci.yml` (actions pinned by SHA). TASKS.md row 0.1 set to
  review.
- **Decisions:** app-agent: `deny`, not `forbid`, for `unsafe_code`, so nc-exec/nc-identity can allow it per module.
  Clippy `unwrap_used`/`expect_used` set to warn (errors in CI through `-D warnings`). Build Tools went to D: because C:
  is short of space.
- **Verified:** fmt ok, clippy clean, tests core 11/11 and server 2/2 passed, `cargo deny check` ok for both. The first
  Build Tools install failed with 0x80070070 (disk full on C:). CI result: see the hand-back.
- **Not done / open:** `flutter doctor` still flags Visual Studio as missing "C++ CMake tools"; this blocks Flutter
  Windows builds (0.8) and needs space on C: (about 1.4 GB free now). No Android SDK (0.9).

## 2026-11-01 · Claude Code (coordinator) · `main`

- **Asked:** start Phase 0; give the owner tasks for Codex that don't collide with the Claude agents; keep a log.
- **Done:** launched app-agent on task 0.1 (branch `feat/app-skeleton`, running). Added `TASKS.md` (task board with
  owner, branch and allowed files per task). Assigned to Codex: 0.4 tariff engine (after 0.1 merges), 0.9b Play
  policy document, 0.10 iOS feasibility document. AGENT.md §2 now points to TASKS.md.
- **Decisions:** coordinator: Codex gets self-contained tasks only (one crate or one document) so file sets never
  overlap; every Codex branch is reviewed by the coordinator before merge.
- **Verified:** documents only.
- **Not done / open:** 0.1 still running.

## 2026-11-01 · Claude Code (coordinator, local Windows) · `main`

- **Asked:** bring the platform work to `main`; delete all unnecessary files, code and folders.
- **Done:** fast-forwarded `main` to `platform/v1` (5a8b833). Removed the legacy Python app from `main`
  (`network_connector/`, `tests/`, `scripts/`, `spikes/`, `docs/`, `pyproject.toml`, `Start.cmd`, `.env.example`,
  `CHANGELOG.md`); all of it remains on `legacy/python-v0.3`. Rewrote README.md and HANDOFF.md. AGENT.md, PLAN.md
  and the three agents now point to the legacy branch and use `main` as the trunk. `.gitignore` gained Rust/Flutter
  entries. Deleted local untracked leftovers: `.venv`, `build`, `dist`, `*.egg-info`, `.windows-validation`,
  `.controller-debug`, `.ryzen-test`, three `.test-app-*` folders.
- **Decisions:** owner: clean `main`. Coordinator: keep LICENSE, CLAUDE.md, WORKLOG.md, .gitattributes.
- **Verified:** `git status` clean after commit; top-level now holds only docs, LICENSE and `.claude/`.
- **Follow-up:** the 14 ACL-locked local folders (`.pytest_cache`, `.test-*`) were then deleted with an elevated
  `takeown` + `icacls /reset` (owner approved the UAC prompt). Project folder now holds only the tracked files.
- **Not done / open:** The other PC still pulling `main` will no longer get the Python app; it must
  switch to `legacy/python-v0.3`.

## 2026-11-01 · Claude Code (coordinator, local Windows) · `platform/v1`

- **Asked:** on a new branch, write AGENT.md and PLAN.md for the new platform and create three agents to build it.
- **Done:** created branch `platform/v1` from `main`. Rewrote `PLAN.md` (goal, device roles, decided stack, hybrid
  architecture, repo layout, security model, night-data tariff scheduling, owner control, phases 0–3, engineering
  standards) and `AGENT.md` (11 security invariants, work method, Phase 0 task split, ownership and contracts).
  New agents `.claude/agents/{core,net,app}-agent.md`; the Python-era `link-agent`/`perf-agent` were removed here
  (still on `legacy/python-v0.3`). Banner added to HANDOFF.md.
- **Decisions:** owner: adopt the researched stack, LAN + internet, industry grade, new branch. Coordinator:
  desktop + Raspberry Pi MVP before Android, iOS last; the Python app stays on this branch as read-only reference;
  three agents split by core / networking + server / app + CI.
- **Verified:** documents only; no code built. `cargo`, `rustc`, `flutter` are **not installed** on this PC
  (checked with Get-Command). Git and Docker are present.
- **Not done / open:** Phase 0 not started. Needs the Rust and Flutter toolchains installed first (task 0.1).
  `main` was not changed in this step because the owner asked for a new branch.

## 2026-11-01 · Claude Code (local, Windows) · `main`, `legacy/python-v0.3`

- **Asked:** keep the current Python version safe in a branch; do not start on new technologies yet; give a
  Perplexity research prompt for the target platform (PCs + phones + any capable device, LAN and internet).
- **Done:** created and pushed `legacy/python-v0.3` from `main` at febfb85. No code changes. Research prompt given
  in chat.
- **Decisions (owner):** platform must work across the internet as well as LAN; must be industry-grade; no move to
  new technologies until the owner says so. Proposed (not adopted): Rust core, Flutter UI, Wasm jobs.
- **Verified:** `git push -u origin legacy/python-v0.3` succeeded; branch visible on remote.
- **Not done / open:** technology choice pending owner's research.

## 2026-11-01 · Claude Code (local, Windows) · `main`

- **Asked:** research how the joiner can keep internet (not via the host); build the "joiner keeps its own Wi-Fi"
  option; give a prompt for the other PC.
- **Research:** a legacy group makes the joiner leave its router (one infrastructure connection per adapter).
  Microsoft's WDI docs say one adapter can run an infrastructure connection and Wi-Fi Direct at once. This PC's
  adapter: Wi-Fi Direct Client/GO supported, P2P Clients Port Count 1.
- **Done:** new `p2p.py` with native Wi-Fi Direct (WinRT). `Host` = advertisement publisher (discoverable) +
  connection listener: accepts, pairs push-button with an auto-accept handler, opens WiFiDirectDevice and reports
  the link IPs. `Joiner` = finds ASSOCIATION_ENDPOINT devices, pairs (GO intent 0), connects and returns the host's
  link IP. The window uses it by default; a "Classic mode" checkbox keeps the old legacy group. If several devices
  are found, a picker asks which one. The window now shows any exception type. Added `winrt-Windows.Networking` to the
  extra. Rebased onto 5ea657c (other session's Stop rework) and merged: Stop also closes native host/joiner.
- **Verified:** pytest 21 passed. On this PC: `Joiner.find()` listed WINDOWS_LAPTOP. `Host.start()`/`stop()`
  advertised and stopped cleanly.
- **Not done / open:** pairing, connect and Hello over the native link are not tested between the two PCs. It is
  unknown whether Windows still shows its own "allow device" prompt. The joiner's adapter capabilities are not yet
  checked. No unit tests for p2p.py (needs hardware).

---

## 2026-11-01 · Claude Code (local, Windows) · `main`

- **Asked:** add a Stop button for the host.
- **Result before the change:** first real two-PC run worked. In the window: Firewall fixed (one UAC Yes), group
  DIRECT-NC-JXSS, and repeated Hellos from Windows_Laptop (192.168.137.50) answered by DESKTOP-VMRF6PJ.
- **Done:** `hello.serve(..., stop=Event)` (0.5 s accept timeout). The window has a **Stop** button, enabled while
  hosting: it stops the listener and the Wi-Fi Direct group, then re-enables Host/Join. Closing the window also stops
  the listener.
- **Verified:** pytest 21 passed (new: the listener exits on stop and frees the port). The Stop button was not
  clicked on hardware.

---

## 2026-11-01 · Claude Code (local, Windows) · `main`

- **Asked:** update the local copy; restart the app; explain the "Hosting DIRECT-NC-JXSS" screenshot.
- **Done:** pulled 29f2976 (Retry button, from another session). Restarted the window. Found a bug: the firewall
  check matched only the interpreter that was running, so under the window's `pythonw.exe` it missed the Block
  rules for `python.exe` (and the reverse). `firewall._pythons()` now matches both python.exe and pythonw.exe.
- **Verified:** `problems()` under both python.exe and pythonw.exe reports "2 rule(s) block Python". pytest 20 passed.
- **Not done / open:** the two "Python" Block rules (python.exe only) are still enabled here. They don't affect
  the window, which listens as pythonw.exe; the next Host click offers to disable them.

---

## 2026-11-01 · Claude Code (local, Windows) · `main`

- **Asked:** a GUI on both PCs: run the app, click Yes, and see the messages passing.
- **Done:** `gui.py` (Tkinter, one window for both roles). **Host** checks the firewall (one UAC prompt), starts
  `DIRECT-NC-XXXX` (retries "aborted" twice after 10 s) and logs every Hello received and the reply sent.
  **Join the other PC** scans, joins with the built-in password and sends Hello; **Send Hello** sends another.
  `Start.cmd` (double-click) creates `.venv` and installs on first run, then opens the window with pythonw.
  `nc-gui` gui-script added. README now leads with the window.
- **Verified:** pytest 20 passed, including a GUI test where the window sends a Hello to a local server and logs
  both directions. `Start.cmd` opened the "Network Connector - Hello" window on this PC.
- **Not done / open:** Host and Join buttons not clicked on real hardware yet. The first-run install branch of
  `Start.cmd` was not run here (`.venv` already existed).

---

## 2026-11-01 · Claude Code (local, Windows) · `main`

- **Asked:** an easier fix for the firewall than pasting an admin PowerShell command.
- **Done:** new `firewall.py`. `nc host` checks for enabled inbound Block rules on this Python
  (`sys._base_executable`) and for the Hello allow rule. If either is wrong, it shows one Windows admin prompt
  (Start-Process -Verb RunAs, encoded command) that disables those Block rules and adds the TCP 8765 LocalSubnet
  rule. Fixed README lines damaged by Python string escapes (`\a`, `\n`). Stopped Claude's own host so the owner can
  run `nc host`.
- **Decisions:** the fix runs only after the user clicks Yes on the UAC prompt. Claude did not run it itself
  (Claude Code refused to weaken the firewall). It is limited to Block rules for this exact python.exe.
- **Verified:** `firewall.problems()` here reports "2 rule(s) block Python" (the allow rule is present). pytest:
  19 passed. `fix()` itself not run.
- **Not done / open:** the elevated fix and the two-PC Hello are still untested.

---

## 2026-11-01 · Claude Code (local, Windows) · `main`

- **Asked:** run the app between two PCs; then make joining easier, with no password to type; then push `main` after
  every change.
- **Done:** merged PR #3 (Hello-only app) into `main`. Started `nc host` here and added the TCP 8765 firewall rule
  (UAC). The second PC joined `DIRECT-NC-8OES` (got 192.168.137.168), but its Hello timed out. Cause: two enabled
  inbound **Block** rules named "Python" (Public profile, `C:\program files\python312\python.exe`), which override
  the allow rule. Claude Code would not disable them (security block); the owner was given the command.
  New: `nc host` with no options uses `DIRECT-NC-XXXX` (from the computer name) and a PBKDF2-derived built-in
  password; `nc join` with no arguments finds the group with `netsh wlan show networks` and joins with that password.
  Test added. README updated.
- **Decisions:** the built-in password is the same in every copy of the app (owner asked for no password); README
  says so. Scanning uses netsh, so no new WinRT package is needed.
- **Verified:** pytest 18 passed. `nc host` started `DIRECT-NC-JXSS` here. One `nc host` run in the owner's admin
  window failed with "Windows aborted Wi-Fi Direct". A second run failed with "already running", because Claude's
  host held the group. Starting again about 10 s after a stop worked.
- **Not done / open:** `nc join` with no arguments is not yet tried on the second PC. The netsh scan only sees the
  networks Windows has cached (it retries for 30 s). The "Python" block rules are still enabled unless the owner
  ran the command.

---

## 2026-11-01 · Claude Code (local, Windows) · `simple-hello` (new branch from cd2e4a3)

- **Asked:** make the application very simple: two PCs just send a Hello message to each other. The owner chose
  "strip the app down" and "plain TCP for now".
- **Done:** removed every module except `wifidirect.py` (open groups, AutoJoin and TLS probe also removed;
  `protect_path` moved in), and every test except `test_wifidirect.py` (trimmed) and the new `test_hello.py`. Added
  `hello.py` (one line each way, 200-byte cap, 10 s timeout) and a new `cli.py`: `nc host` (start group, print
  SSID/password, answer Hellos, stop group on exit), `nc join SSID PASS` (join, send Hello to gateway/.1),
  `nc send IP`, `nc listen`. Removed examples, setup scripts and the GUI entry points. The firewall scripts now add one
  inbound TCP 8765 rule (Private+Public, LocalSubnet). pyproject has no runtime dependencies; version 0.4.0. README
  rewritten; notes added at the top of HANDOFF.md and AGENT.md; CHANGELOG entry.
- **Decisions:** worked on a new branch so the full app stays intact on `claude/great-davinci-bch39n`. The plain TCP
  Hello breaks AGENT.md's TLS invariant on purpose (owner's choice); recorded in the AGENT.md note. PLAN.md, docs/,
  spikes/ and `.claude/agents` kept as history and not updated.
- **Verified:** `.venv` pytest: 17 passed. `nc listen --bind 127.0.0.1 --once` plus `nc send 127.0.0.1` as two
  processes: both sides printed the other's "Hello from DESKTOP-VMRF6PJ".
- **Not done / open:** `nc host`/`nc join` not run on two real PCs (needs the i7 and i3). The firewall scripts were
  not run.

---

## 2026-11-01 · Claude Code (local, Windows) · `claude/great-davinci-bch39n` · ae6c341

- **Asked:** remove the Mobile Hotspot path (mode B) from the whole system; use Wi-Fi Direct only.
- **Done:** `wifidirect.DirectGroup.start` no longer adopts an existing owner adapter: it refuses with "Another Wi-Fi
  Direct group or Mobile Hotspot is already running". `status()` reports interface/IP only for our own group (mode A),
  so mode "B" no longer exists. `controller._refresh_direct_subnets` uses the group status instead of any owner
  adapter, so a hotspot subnet is no longer trusted as a direct subnet. Hotspot wording removed from errors, the GUI
  Wi-Fi Direct tab, the CLI help and `add-firewall-rule.ps1`. Hotspot test replaced by
  `test_existing_hotspot_is_neither_adopted_nor_reported`.
- **Decisions:** refuse rather than silently reuse an external group. Design docs (PLAN/AGENT/HANDOFF/ACCEPTANCE)
  still describe mode B; not edited, because the owner then asked to strip the app down to a Hello exchange.
- **Verified:** full `.venv` pytest: 345 passed.
- **Not done / open:** not tried on hardware. ae6c341 also carries the uncommitted 2026-10-29 changes below; this
  entry was committed separately because the first edit failed.

---

## 2026-10-29 · Claude Code (local, Windows) · `claude/great-davinci-bch39n`

- **Asked:** find out why Windows Mobile Hotspot was on, with a helper connected, but the Wi-Fi Direct tab still showed
  "Mode: A | Stopped, Interface: -, IP: -".
- **Done:** `wifidirect._read_adapter_rows`: `Get-NetIPConfiguration` threw for a hidden adapter that has no IP interface
  (here ifIndex 17). `-ErrorAction SilentlyContinue` did not catch the error raised inside the module under
  `$ErrorActionPreference='Stop'`, so the whole PowerShell query exited 1 and the app saw **no adapters at all**. It
  then never detected the hotspot and fell back to starting its own Wi-Fi Direct publisher. That call is now wrapped in
  `try { ... } catch { $null }` for each adapter.
- **Decisions:** one-line fix. The command is still constant (nothing interpolated). Security invariants unchanged.
- **Verified:** before the fix the query returned rc 1 and `direct_interfaces()` returned `[]`. After it,
  `direct_interfaces()` returns the "Microsoft Wi-Fi Direct Virtual Adapter #4" at 192.168.137.1/24, owner=True,
  peer 192.168.137.158. `.venv` pytest `-k "direct or wifi or link or discovery"`: 89 passed.
- **Also:** `link_gui.firewall_status` always said "Firewall rule needs updating" because Windows reports the rule's
  remote address as `192.168.137.0/255.255.255.0` and the code string-compared it to `192.168.137.0/24`. It now
  compares parsed networks. With the real rule: "Direct-link rule enabled for this address and subnet."
  `-k "firewall or link_gui or direct"`: 44 passed. App relaunched (owner approved); controller listens on 192.168.137.1:8765.
- **Also:** owner reported PowerShell windows popping up. Under pythonw every console child opens a window unless
  started with CREATE_NO_WINDOW; only `wifidirect._run` and the job runner did. Added it to the three PowerShell calls in
  `link_gui.py` (firewall status runs on every Wi-Fi Direct refresh), `security.protect_path` (icacls) and the two
  icacls calls in `sandbox.py` (Windows-only flag, 0 elsewhere). `-k "firewall or link or direct or sandbox or
  security"`: 115 passed. Not yet seen in the relaunched GUI. Same flag added to `updater._run` (git/pip).
- **Also (GUI slowness):** owner said the interface is very slow and asked about another UI technology. Measured: the
  adapter query took 12.3 s per call and the firewall check 4.0 s, both started on every 5 s Wi-Fi Direct refresh.
  Tkinter itself was not the cause (work already runs on background threads). Rewrote the adapter query as one bulk
  call per cmdlet joined by ifIndex, without `Get-NetIPConfiguration` (gateway from `Get-NetRoute 0.0.0.0/0`): 2.6-2.9 s,
  same detected interface and peer. The firewall check now reruns only when the address changes, once a minute, or
  after "Allow through firewall". `-k "firewall or link or direct or wifi or discovery or config"`: 90 passed.
  Recommended keeping Tkinter. Not yet seen in the GUI.
- **Also (controller would not start):** popup "Port 8765 is used by another program (PID …, pythonw.exe)" where the
  PID was the app itself. Cause: `controller.json` `bind_hosts` held 192.168.137.1, and the hotspot had turned off
  (Windows hotspot power saving), so that bind failed after 10.63.182.8 and 127.0.0.1 had bound; `port_in_use_message`
  then found the app's own listener. Fixes in `controller.py`: `start()` skips an extra saved address this PC no longer
  has (logged, kept in config); main address and loopback still must bind. `port_in_use_message` ignores this
  process's own listeners and says when the address is missing. Two tests added in `tests/test_local_connection.py`;
  that file plus `test_controller.py`: 13 passed. App restarted at the owner's request.
  Open: how 192.168.137.1 got persisted although the direct listener is added with `persist=False` (likely a later
  `save_config` of the in-memory cfg); not investigated.
- **Not done / open:** no regression test (the failure needs a real hidden adapter). The GUI was not re-run here.
  Separately, the saved controller `bind_host` is 172.21.224.1, the WSL/Hyper-V vEthernet address, which the helper
  cannot reach. The direct-link listener adds 192.168.137.1 once the hotspot is detected.

---

## 2026-10-29 · Codex · `claude/great-davinci-bch39n`

- **Asked:** fix the controller startup popup, "Unable to configure formatter 'default'".
- **Done:** explicitly disable Uvicorn console colours so its formatter does not call
  `sys.stdout.isatty()` in Windows GUI/pythonw launches, where stdout is None.
  Added a regression test starting a real TLS controller with stdout/stderr absent.
- **Decisions:** logging-only change; TLS, binds and security invariants unchanged.
- **Verified:** targeted controller tests: 4 passed, 1 cache-permission warning.
  Initial sandboxed run: 4 setup errors due to temp-directory access; rerun outside
  the execution sandbox passed. Full suite started; result pending.
- **Not done / open:** the existing GUI process needs reopening to load this change;
  Wi-Fi Direct hardware acceptance is still pending.

---

## 2026-10-29 · Claude Code (local, Windows) · `claude/great-davinci-bch39n`

### Acceptance sections 1-2 on a third laptop (Ryzen), run as "i3"

- **Asked:** clone/update the repo, read `docs/ACCEPTANCE_0.3.0.md`, do everything possible on this laptop, then update `main`.
- **Done:** installed Git and Python 3.12.10 (winget), switched to this branch (commit `238819b`), built `.venv`
  (`.[test,app,wifidirect]`, version 0.3.0, import path inside the checkout), ran
  `collect-local.ps1 -PC i3 -FullTests`, then `nc sandbox setup --remove` in the isolated shell. No source changed.
- **Machine:** AMD Ryzen 3 3250U, 4 threads, about 3.4 GB RAM. This is **not** the i3-1115G4; the "i3" label was chosen
  by the owner so the script would run. It cannot count as the real i3 result.
- **Verified:** full suite **342 passed, 0 failed, 0 skipped** (1 deprecation warning), 15 min 19 s. `sandbox setup`
  and `status --check` OK (self-check 5140 ms, profile deleted); primes printed `78498` (0.70 s); bench printed digest
  `f21b6b5f...cab6a` (1 unit, 1.76 s); leftover `NC-Sandbox-*` profiles: 0 before and after.
- **Not done / open:** sections 3-7 (app/tray/notifications, join/consent, Stop <= 1 s timing, quotas, direct link and
  hotspot, firewall rules, two-PC benchmark, listener checks) need the real i7 and i3 and a person at the screens.
  Hardware acceptance is still pending; no tag was created. Result files stay local under
  `spikes/0.3-windows/out` (git-ignored).

---

## 2026-10-29 · Claude Code (local, Windows) · `claude/great-davinci-bch39n`

### Windows validation, i7 checks and acceptance preparation (finishes Codex's interrupted work)

- **Asked:** run the full suite on Windows (never done before), check the i7, fix real failures without weakening
  security, prepare the i3 acceptance steps, commit and push. No merge to `main`, no tags. The owner confirmed Codex had
  stopped (token limit) and asked me to finish.
- **Done:**
  - Reviewed and kept Codex's uncommitted work: `winlimits.limit_memory` now closes the job and returns `None` when the
    CPU-rate cap can't be set, and `worker._start_job` kills and refuses the job in that case (the owner's cap is never
    silently skipped); two new tests for it (`test_helper_policy.py`, `test_sandbox.py` real Job Object check of the
    25 % cap and sandbox refusal); `conftest.py` pins `psutil.sensors_battery` so integration tests don't depend on
    charging; `docs/ACCEPTANCE_0.3.0.md`; `spikes/0.3-windows/collect-local.ps1`.
  - Fixed `tests/test_wasm_bench.py::test_wasm_engine_needs_helpers_with_a_sandbox`: it set `worker._info["sandbox"] = False`
    by hand, but every heartbeat re-reads `sandbox.available()`. It only passed where no sandbox exists (Linux). Now the
    test makes `sandbox.available` return False and waits until the controller has seen it. Product code unchanged.
  - Fixed `collect-local.ps1`: with `$ErrorActionPreference='Stop'` and `2>&1`, Windows PowerShell 5.1 turns the first
    line `nc` prints to stderr (`[ok] exit code ...`) into a terminating error, so the script died after the primes run.
    Native commands now run through `Invoke-Logged` (`Continue`, stderr logged as plain text).
  - `.gitignore`: `.test-tmp*/` widened to `.test-*/` (Codex's `.test-app*`, `.test-policy*`, `.test-stop` folders are
    Windows-ACL-locked and must not be committed).
  - Checked that the environment variables used by the acceptance steps exist (`NC_PORT`, `NC_CONTROLLER_PORT`,
    `NC_DISCOVERY_PORT` come from config's generic `NC_<FIELD>` override; the discovery code uses `cfg.discovery_port`).
- **Decisions:** the stricter behaviour (refuse the job if the owner's CPU cap can't be set) stays; nothing in the
  security invariants was loosened.
- **Verified (real results, this i7, Windows 11, Python 3.12.9, commit `7c1c178` + the uncommitted changes above):**
  - Baseline full suite with Codex's changes: **341 passed, 1 failed** (the test fixed above), 11 min 38 s. This was the
    first run of the Windows-only escape suite, wasm job tests, firewall harness and CPU-cap tests: all passed.
  - Re-run of `test_wasm_bench.py` + `test_helper_policy.py`: 13 passed.
  - Final full suite: **342 passed, 0 failed, 0 skipped, 1 warning (Starlette deprecation)**, 7 min 18 s.
    `sandbox.leftovers()` afterwards: `[]`.
  - `collect-local.ps1 -PC i7` (isolated `NC_HOME`/`NC_RUNTIME_HOME` under `spikes\0.3-windows\out`): `nc sandbox setup` ok;
    `status --check` available, self-check ok (2625 ms, profile deleted); primes printed `78498` (0.39 s, peak 36 MB);
    bench digest `f21b6b5f35b38489a4d9e714089c3df49b59b43de6c0efb2f9d37b43f43cab6a` (matches), 1 unit; leftover profiles
    `[]`; 0 failed commands. (The first run of this script crashed on stderr; see the fix above.)
  - App launch (`network-connector.exe`, isolated home, ports 18765/18766): process alive after 10 s, no output/errors,
    the process owned a visible `TkTopLevel` window titled "Network Connector" (the role chooser, as no role was saved)
    and a `...SystemTrayIcon` window (pystray created its tray window). Probe process was terminated afterwards;
    no sandbox profiles left.
- **Not done / could not be checked (be clear: these are NOT verified):**
  - What the app looks like: tray icon image and right-click menu, a real toast/notification, the "Start at login"
    switch (HKCU Run value), close-to-tray, Both/Give/Ask role screens. I cannot see the screen; the owner must check
    these (acceptance doc §3).
  - Everything that needs the i3: join/consent, Stop <= 1 s on hardware, Wi-Fi Direct group and hotspot, auto-join, the
    discovery firewall rule on a Public link, `Get-NetTCPConnection` listener check on the i3, two-PC benchmark, quotas
    with real unplugging. Hardware acceptance is **pending**; nothing was merged to `main` or tagged.
  - Optional `python.wasm` (PLAN §17.6) was not built.
  - The Windows-locked folders `.test-app*`, `.test-policy*`, `.test-stop`, `.test-tmp-*`, `.pytest_cache` could not be
    deleted by this session (access denied); they are git-ignored.

## 2026-10-28 · Codex · `claude/great-davinci-bch39n`

### Windows validation and acceptance preparation (interrupted: Codex ran out of tokens; finished by Claude Code in the entry above)

- **Asked:** run the complete Windows suite and isolated i7 checks, fix real
  failures without weakening isolation, prepare exact i3 acceptance steps, and
  push this branch only after tests pass. No main merge or tags.
- **Done:** fetched and checked out the requested branch; read CLAUDE, WORKLOG,
  HANDOFF, AGENT §1, PLAN and the security review. Added
  `docs/ACCEPTANCE_0.3.0.md` and `spikes/0.3-windows/collect-local.ps1` for isolated
  per-PC evidence; ignored validation artifacts.
- **Decisions:** keep all application state/runtime copies separate from the
  owner's real data. Two-laptop outcomes stay pending until observed/reported.
- **Verified:** Python 3.12.9 venv initialized from
  `C:\Program Files\Python312\python.exe`; `pip install -e .[test,app]` succeeded;
  import resolves to this checkout. Full baseline Windows suite is running.
- **Not done / open:** baseline/final summaries, actual i7 UI and sandbox results,
  and fixes will be recorded here before the corresponding commit. Hardware
  acceptance and actual login persistence remain owner-run checks.

## 2026-10-28 · Claude Code (cloud session) · `claude/great-davinci-bch39n`

### Session 2: work-log rule

- **Asked:** create a log file that records everything done, and make updating it a project rule.
- **Done:** added this `WORKLOG.md` with the history below; the rule is in `AGENT.md` §1 ("Work log"), in both agent
  definitions (`.claude/agents/link-agent.md`, `perf-agent.md`) and in a new `CLAUDE.md` that Claude Code loads
  automatically; `HANDOFF.md` points to the log.
- **Verified:** documentation-only change; no code touched.

### Session 1: finishing the remaining 0.3.0 work in HANDOFF.md

Commits: `476c7b1`, `1b9e5a1`, `c8cc5e4`, `c8751f1`, `a2e184e` (base: `b1a4943`, the merged `integrate/0.3.0`).

- **Asked:** "read the repo and HANDOFF.md fully and do the left parts".

**Owner decisions (asked first, as HANDOFF §2.2 requires):**
- pystray for the tray icon: **approved** (optional `[app]` extra, LGPL-3.0 used unmodified).
- `add-firewall-rule.ps1 -Discovery` (UDP 8766, base python.exe, Private + direct-link subnet): **approved**.
- Release: **bump to 0.3.0, don't merge to `main`, don't tag** (hardware acceptance pending).

**Environment:** Linux cloud container, Python 3.12 venv, Xvfb for tkinter tests. No Windows, no PowerShell
(downloading PowerShell was refused by the session's safety policy). `rustup target add wasm32-wasip1` was installed.

**Phase 9 — one app, tray, messages, owner limits** (`476c7b1`)
- New `tray.py` (pystray `run_detached`; menu clicks only queue actions that tk polls every 100 ms).
- New `autostart.py` (per-user `HKCU\...\Run` value `NetworkConnector`, `--background`).
- New `notify.py` (AUMID `NetworkConnector.App` registration; toast → tray balloon → window fallback; strict
  `accept:|decline:|stop:<64-hex device id>` arguments; `ConsentRequests` with a 60 s timeout to Decline; messages
  show only name, short ID and thread count).
- New `radio.py` (Wi-Fi radio state; turn **on** only from the owner's click; never off).
- `app.py` rewritten: tray, close to tray, `--background`, Start at login switch, restart to change roles, offers
  `sandbox.setup_runtime()` for Give performance; `ControllerRuntime.wind_down()` announces closing first.
- `link_gui.py`: helper consent flow (auto-accept with message, Ask me with Accept/Decline, Trusted only), Stop from
  a message, in-app consent dialog, Wi-Fi radio row.
- `helper_policy.py`: `limit_reason()` and the `HelperPolicy` contract object (validated, maps to `worker.json`).
- `worker.py`: limits checked before claims **and** about once a second while jobs run; running CPU time counts
  against the daily limit; `limit_stop_now`; blocking always stops jobs; job label in "what is running";
  `cpu_rate_percent` (Job Object CPU-rate hard cap in `winlimits.py`/`sandbox.py`, job refused if it can't be set).
- `worker_gui.py`: accept mode, stop-at-once, CPU cap, "What is running" view, blocked count.
- `controller.py`/`store.py`: claim response carries the batch label. `cli.py`: `nc app open|autostart|reset`.
- `pyproject.toml`: `[app]` extra.
- Tests added: `test_stop_helping.py` (real TLS cluster: job process gone < 1 s, job released; limit reached
  mid-job), `test_notify.py`, `test_tray.py`, `test_autostart.py`, `test_radio.py`, `test_consent.py`,
  `test_helper_policy.py` additions, `test_app.py` rewrite.

**Phase 7 leftovers** (`1b9e5a1`)
- `scripts/add-firewall-rule.ps1 -Discovery [-LocalAddress]`, `remove-firewall-rule.ps1 -Discovery`, and an elevated
  "Allow discovery through firewall" button (`link_gui.discovery_firewall_command`, values validated).
- `wifidirect.py`: `open_group_ssid` (`DIRECT-NC-XXXX`), `derived_passphrase` (PBKDF2, app salt),
  `connected_wlan_ssids`, `scan_direct_groups` (WinRT), `join_open_group`, `AutoJoin` (≤ every 30 s; join only when
  on no Wi-Fi, otherwise offer). Open requesters start groups with derived credentials; closed ones stay random.
- `identity.py`: `cert_expires`, `expiry_warning` (90 days) shown in the log, `nc identity`, Devices tab, helper.
- Tests: firewall static checks (+ Windows-only harness), open-group credentials, auto-join rules, expiry warning.

**Phase 10 — result checks** (`c8cc5e4`)
- `store.py` migration step 4 (`excluded_workers`, `check_of`, `verifier`; `job_checks` state columns).
- Redundancy (`redundancy_percent`, default 10) for open-group wasm results: held as `checking` until a twin on a
  different helper matches (stdout + exit code); mismatch → third helper, majority wins; twins only when another
  helper is online; given up after 10 minutes.
- Reliability score (`devices.score`: +1 / −5 / −1); negative → one chunk at a time; `block_score` −10 → blocked and
  revoked.
- New `verify.py`: requester-side Wasm verifier in the requester's sandbox; rejected results go to another helper;
  fails closed. Submit option `verifier_sha256` (wasm only, module must exist, sandbox must be available).
- Helper-side reputation in `helper_usage.py`; "Block this requester" button.
- API/CLI/GUI: `/admin/checks`, `nc checks`, `nc policy --redundancy`, `--verifier`, Devices tab Score column.
- Tests: `test_verify.py` (a lying helper on a real TLS cluster is caught, outvoted and dropped; verifier paths).

**Phase 11** (`c8751f1`, `a2e184e`)
- `examples/wasm/bench` (Rust, std-only SHA-256) built for `wasm32-wasip1`; module shipped as
  `network_connector/data/wasm_bench.wasm`, pinned SHA-256 `245016c7…4257`; answers byte-identical to the native job.
- Benchmarks: `engines` native/wasm (`scenario@wasm`, Wasm overhead column), `topN` scenarios, device names and short
  IDs, optional shared speed score (`share_speed_score`, off by default); benchmark jobs are never held for checks.
- Rust primes example built once: imports `fd_read`, `fd_write`, `environ_get`, `environ_sizes_get`, `proc_exit`
  (all allowed); prints `78498` for `0 1000000` under wasmtime.
- Docs: README, new `docs/SETUP.md` + `setup-worker.ps1 -App`, JOINING, SECURITY, SANDBOX, CONFIGURATION,
  BENCHMARK, TROUBLESHOOTING, CHANGELOG 0.3.0, example READMEs, HANDOFF update.
- Security review `docs/SECURITY_REVIEW_0.3.0.md`. Fixed: limit-hit kill was process-wide (now only the
  requester's jobs); `Store.finish` now requires `status == running`; checks no longer stall a lone helper;
  benchmarks excluded from redundancy.
- `__version__` = `0.3.0`.

**Verified**
- Baseline before changes: 238 passed, 6 failed (3 need Windows; 3 were loopback aliases going through the proxy),
  43 skipped.
- Final full run (`NO_PROXY=127.0.0.1,127.0.0.2,127.0.0.3`, Xvfb): **286 passed, 4 failed, 49 skipped**. The 3
  `test_wifidirect.py` failures need Windows (same as before). `test_benchmark_two_real_workers_api_cli_exports`
  timed out once while pairing in setup; it passed in the previous full run and twice on its own afterwards.
- `compileall` passed.

**Not done / open**
- Optional `python.wasm` (PLAN §17.6).
- Nothing Windows-only was executed: escape suite, AppContainer/Job Object (incl. the CPU cap), PowerShell scripts,
  toasts and tray on screen, WinRT Wi-Fi/radio.
- Hardware tests (HANDOFF §2.3), PLAN §22.3 acceptance, then merge to `main` and tag — owner.
