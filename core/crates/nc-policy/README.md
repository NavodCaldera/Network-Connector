# nc-policy

Pure owner-policy decisions for task 1.P1. This library reads no clocks, performs no I/O, logs no data, and applies no OS controls.

## Decisions

- `admit` evaluates the offer and current device snapshot. Accept is provisional: the supervisor must serialize admission/resource reservation to prevent concurrent offers from sharing the same free capacity.
- `check_running` evaluates **all** entries in `DeviceState.running_tasks`, including paused tasks. The vector provides the count, authenticated peer key, execution profile and start time needed for revocation and duration checks. It is not a second wire format.
- Invalid policy, withdrawn consent, blocked peers, critical temperature, revoked trust/profiles, and expired/invalid task clocks cause Stop. Hard stops precede temporary pauses. A blocked key wins even if it is also trusted.
- Battery/power/activity/hours/temperature/resource/tariff restrictions cause Pause and reject admissions. Concurrency includes the incoming task for admission; existing tasks may run exactly at the cap. Memory/disk admission adds estimates with checked arithmetic. Fields ending `_mb` use MiB.
- `next_change` returns the earliest predictable future **reevaluation** time: working-hour boundary, idle threshold, task deadline, or supplied tariff opening/expiry. It is not a promise of future acceptance. External changes require immediate reevaluation, and None never authorizes indefinite running.

## Configuration and unknown observations

`OwnerPolicy` is serde-compatible, revisioned, rejects unknown fields, and supplies safe defaults for missing fields. The default disables helping and has empty trusted-peer, execution-profile and time-window allowlists. Turning on `accept_jobs` alone cannot bypass those lists. Call `validate` after parsing; decision functions also validate and refuse invalid configuration.

Peer keys contain exactly 32 bytes. Serde accepts exactly 64 ASCII hexadecimal characters (either case) and emits lowercase hex. Parsing validates encoding only; the identity layer must authenticate the key before supplying an offer or task snapshot. Block/trust overlap is valid and resolves to blocked.

Thermal thresholds are inclusive: Serious pauses at Serious, and Critical always stops. Unknown thermal always rejects/pauses because every policy specifies a thermal threshold. Battery is required, even on AC, when `min_battery_percent > 0`; None then rejects/pauses. For a genuinely batteryless device, the owner may explicitly choose minimum 0. Percentages above 100 are invalid observations. Activity information must be known only when `pause_on_user_activity` is enabled. The idle and minimum-battery boundaries themselves are allowed. `on_ac_power` is an adapter-provided proxy for the charging-only rule, not a measurement of battery charge current.

CPU budgets are configuration requests from 1 to 100 percent and are **best effort**, not hard quotas enforced by this library. Task estimates must have positive duration within the maximum. Running-task limits use elapsed UTC wall time, including paused time; the external executor watchdog must also use a monotonic clock. A future task start or unrepresentable deadline stops work. A backward clock jump that remains after the recorded start cannot be detected from one snapshot; the supervisor must detect clock discontinuities separately.

## Working hours and tariff integration

Working windows are half-open, use weekdays of their **start** date, and may cross midnight. Empty windows mean never; equal endpoints and overlapping weekly windows are rejected. `nc-tariff` validates the windows and calculates the shared calendar through a private synthetic unlimited profile. This internal projection authorizes **no transport**. It uses the same conservative DST semantics: later ambiguous start, earlier ambiguous end, skip nonexistent endpoints. Calendar extremes within 400 days of chrono's representable limits fail closed. The shared calendar may schedule an earlier monthly reevaluation point or search up to 370 days for the next opening.

`DeviceState.tariff_decision` separately carries the **real** `nc_tariff::decide` result for a required transfer. None explicitly means local computation with no transfer. Deny, Defer, expired Allow and zero-byte-capacity Allow all block that transfer/work. The supervisor must provide the latest decision for the exact transfer request, atomically reserve through the real tariff ledger, enforce `max_bytes`/`until`, and recheck on network or throughput changes. A stale/forged/missing decision cannot be distinguished from truthful caller input by this pure library.

## Supervisor responsibilities

Supply fresh power/battery/thermal/activity observations and complete authenticated task metadata. Refresh snapshots and recheck on owner-policy updates and device/network/clock events. Persist consent and accounting outside this crate. Enforce actual memory/disk/CPU constraints, stop/pause sockets and executors, and tear down sandboxes on all required paths. A returned Stop is a decision, not proof that the one-second Stop invariant has been executed. No existing security requirement is waived.

Unit/property tests cover all rule/reason categories, serialization/validation, overflow, resource/idle/battery boundaries, Colombo midnight, London DST, tariff integration, hard-stop precedence and next-change scheduling. Required command output belongs in the hand-back; no run logs are included in this crate.
