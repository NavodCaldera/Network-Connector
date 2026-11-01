# nc-tariff

Pure tariff library for Phase 0 task 0.4. All profiles and simulated traffic are examples, not claims about any ISP or package.

## Contract

- Deserialize `TariffProfile` with a serde JSON/YAML codec, then call `validate`. IANA time zones use `chrono-tz`. Window weekdays refer to the start date; windows are half-open and may cross midnight. Equal start/end and overlapping windows are rejected.
- DST ambiguity is conservative: choose the later start and earlier end. Skip occurrences with nonexistent local endpoints. This can miss usable time but never broadens permission. Monthly reset dates clamp to the last day of shorter months; a nonexistent midnight advances to its first valid minute, and an ambiguous midnight uses the later instant.
- `billing.remaining_bytes` is the configured allocation available now and renewed each month. It is not queried from a provider. The percentage reserve rounds **up** to whole bytes. A profile revision/configuration change requires deliberate ledger migration; an old snapshot cannot authorize a new profile.
- `NetworkObservation.connection_id` must come from the adapter's owner-confirmed connection binding, never an SSID match. With explicit cellular fallback, a nonempty confirmed cellular identity is accepted under this same budget/window policy. Use another profile if it has another tariff.
- `decide` is pure and does not reserve. `Allow.until` is exclusive, guarded, and capped at billing renewal. Throughput is conservative estimated **wire bytes/second**; bytes must include expected overhead. Unknown throughput defers. `Defer.next_open` searches 370 days; `None` does not claim no future time can ever exist.
- Strict mode blocks all five classes outside windows. Non-strict mode permits only explicit outside-window classes. Boundary guards remain hard exclusions even for those exceptions.
- `BudgetLedger::reserve_transfer` performs authorization and reservation under one mutex; raw `reserve(bytes)` is period-only accounting, not transport authorization. `commit` settles exactly once and reports estimate, period and window overages, including retransmits/relay overhead. `release` is only for unused reservations. Partial transfers must commit actual bytes, not release them.
- Arithmetic saturates or fails closed. Mutex poisoning refuses work. Reservation IDs are not reused on renewal. All outstanding reservations must be settled before a renewal can replenish capacity. Backward clock movement behind the ledger watermark refuses authorization; forward jumps renew at most the current period, never accumulate missed allocations. Clock discontinuities must invalidate active transport permissions and trigger reauthorization.
- The planner returns a deterministic greedy **serial** estimate, with device jitter and explicit unscheduled work. It is not an optimal-cost solver and does not mutate the caller's ledger. The forecast assumes constant throughput and configured monthly renewals; reservations are obtained again at execution time. Horizon is capped at 366 days.

## Integration obligations

The library does no I/O, networking, logging or clock reads. The supervisor owns consent, power/thermal checks, wall-clock-change detection, persistence of accounting events, and transport cancellation. Persist reservation/settlement events transactionally before transfer; this Phase 0 in-memory ledger does not supply crash recovery or storage adapters. On restart, reconstruct/reconcile authoritative accounting rather than create an empty ledger over existing spending.

Reauthorize every bounded chunk and immediately on network/throughput/clock changes. Cancel at `until` even if a throughput estimate proves optimistic; no library function can stop a socket it does not own. This crate does not itself promise a one-second Stop or exact ISP billing. Actual bytes can exceed estimates; reconciliation reports the overage and subsequent reservations consume only remaining capacity. Profile identity and observation authenticity are caller responsibilities.

## Example and validation

From `core/`, with `CARGO_TARGET_DIR=D:\nc-target\codex`:

```text
cargo run -p nc-tariff --example night       # 90 synthetic nights
cargo run -p nc-tariff --example night -- 180
cargo fmt --all --check
cargo clippy -p nc-tariff --all-targets -- -D warnings
cargo test -p nc-tariff
cargo deny check
```

The deterministic simulation varies transfer sizes and observed throughput, checks schedule containment and ledger limits, and rejects any unreported cap violation. Tests cover Colombo cross-midnight, London DST, guard boundaries, exact reserve, all traffic classes, network switches, monthly renewal, clock jumps, concurrent reservations, overage and overflow. Proptest explores both schedule containment and reconciled committed totals.

The checked commands and their actual output are in `VALIDATION.txt`. Cargo regenerated the workspace lockfile during validation; it is intentionally not part of this branch's changes because it is outside task ownership. The coordinator must regenerate/review `core/Cargo.lock` for these exact crate-local pins before using `--locked` CI.
