# Android helper policy — Phase 0 task 0.9b

Checked: **2026-11-01**. This is a policy assessment, not Google Play approval or task 0.9a's runtime/foreground-service experiment. Every external claim cites a source and check date. Recommendations are explicitly project inferences. Device results, sustained performance and approval of this particular helper are **unknown**.

## Downloaded code and use of other people's phones

Google Play prohibits updating an app outside Play and downloading executable dex/JAR/native `.so` code from other sources. It makes an exception for code executed in a VM/interpreter with indirect Android API access; runtime-loaded interpreted code must still be incapable of facilitating policy violations. **Inference:** sandboxed interpreted Wasm is a candidate for the exception; the policy does not explicitly name Wasm or approve this platform. Downloading a native runtime/kernel `.so` is not covered by calling the overall app a Wasm helper. Ship the native runtime through Play. [Device and Network Abuse](https://support.google.com/googleplay/android-developer/answer/16559646?hl=en) (checked 2026-11-01).

The same policy forbids unauthorized device/network interference and attempts to bypass system power management. A signed job is not sufficient evidence that its behaviour is policy-compliant. **Recommendation:** narrow host API, no ambient network/filesystem access, curated workloads, module verification, caps and abuse screening. Whether an arbitrary public job marketplace can satisfy the policy is **unknown**; do not claim blanket permission. [Device and Network Abuse](https://support.google.com/googleplay/android-developer/answer/16559646?hl=en) (checked 2026-11-01).

Play forbids cryptocurrency mining **on the device**, while allowing remote management of mining. Consent does not create an exception. **Recommendation:** exclude mining jobs, including third-party submissions disguised as generic compute. This is not a general prohibition on all scientific/distributed compute; no explicit approval of our use case was located. [Developer Program Policy, Cryptomining](https://support.google.com/googleplay/android-developer/answer/18265346?hl=en) (checked 2026-11-01).

Apps must accurately describe functionality and cannot hide review behaviour; device-setting changes require informed consent and reversibility. **Recommendation:** explain before Start that CPU, battery, heat and bandwidth serve other people's jobs; identify permitted work, session duration, charging/network/data caps, results shared and Stop behaviour. Record affirmative session consent and provide visible progress/Stop. Do not present this recommended consent text as a Google-issued compute-specific checklist. [Deceptive Behavior](https://support.google.com/googleplay/android-developer/answer/17006354?hl=en) (checked 2026-11-01). Any personal/sensitive data also needs appropriate disclosure, consent and secure handling; helper consent alone does not cover unrelated telemetry. [User Data](https://support.google.com/googleplay/android-developer/answer/10144311?hl=en) (checked 2026-11-01).

## Foreground-service fit

The following are **project assessments**, based on the official type definitions, not permission to select whichever type lasts longest. [Foreground service types](https://developer.android.com/develop/background-work/services/fgs/service-types) (checked 2026-11-01).

| Type | Honest fit for this platform |
|---|---|
| `dataSync` | Upload/download and local file processing are listed uses. May fit an actual transfer/file-processing phase; general CPU simulation is not automatically data synchronization. |
| `mediaProcessing` | For actual media operations such as transcoding; not general numerical jobs. |
| `shortService` | Critical nondeferrable work, about 3 minutes; unsuitable for a standing helper queue. No sticky service support or starting other FGSs. |
| `specialUse` | Candidate for otherwise uncovered valid user-started compute. Requires `FOREGROUND_SERVICE_SPECIAL_USE` and an explanatory `android.app.PROPERTY_SPECIAL_USE_FGS_SUBTYPE` service property. Google reviews the explanation. Acceptance for this use case: **unknown**. |
| `connectedDevice` | Candidate only for genuine ongoing external-device interaction satisfying its prerequisites; a network connection alone is not proof that arbitrary local computation qualifies. |
| `systemExempted`, media playback, location, etc. | Do not claim system exemptions or unrelated functions to keep a compute loop alive. Eligibility must match the documented use. |

**Recommendation:** prototype a directly user-started, finite `specialUse` compute session with a clear notification and Stop. Use appropriate separate transfer APIs/phases; retain a foreground-only fallback if the FGS use case is rejected. Whether the work truly requires immediate, uninterrupted execution must be justified to Play. All FGS types are reviewed; a notification alone is not an approval. [Play FGS requirements](https://support.google.com/googleplay/android-developer/answer/13392821?hl=en) (checked 2026-11-01).

## Android 14/15/16 constraints

- **Android 14 / target 34+:** appropriate service types and their permissions are required. Missing types fail rather than silently providing a generic compute service. [Android 14 changes](https://developer.android.com/about/versions/14/changes/fgs-types-required) (checked 2026-11-01).
- **Android 15 / target 35+:** while backgrounded, `dataSync` and `mediaProcessing` each have an aggregate **6 hours per 24 hours**, tracked separately per type but shared across the app's services of that type. Foreground user interaction resets the timer. At timeout, `Service.onTimeout(int,int)` gives only a few seconds to stop; do not wait for process failure. Exhausted allowance blocks another service until the documented reset. These limits are not a promise of six uninterrupted hours. [FGS timeouts](https://developer.android.com/develop/background-work/services/fgs/timeout) (checked 2026-11-01).
- **Android 15:** adds `mediaProcessing` and restricts launching several FGS types, including `dataSync`, from `BOOT_COMPLETED` for apps targeting 35+. **Recommendation:** no boot-started or remotely auto-started helper. [FGS changes](https://developer.android.com/develop/background-work/services/fgs/changes) (checked 2026-11-01).
- **Android 16, regardless of target:** jobs run concurrently with an FGS now obey job quotas, including JobScheduler/WorkManager/DownloadManager work; an FGS is not a job-quota escape. [Android 16 changes](https://developer.android.com/about/versions/16/behavior-changes-all) (checked 2026-11-01). Long-running WorkManager workers can exhaust quotas; Android suggests considering direct FGS use where appropriate. [Long-running workers](https://developer.android.com/develop/background-work/background-tasks/persistent/how-to/long-running) (checked 2026-11-01).

`shortService` still times out when battery optimization is disabled. No general numerical time allowance for `specialUse` is documented on the cited type/timeout pages; an actual uninterrupted duration is **unknown**, not unlimited. **Recommendation:** enforce our own short session deadline and checkpoint/requeue on every interruption. [Types](https://developer.android.com/develop/background-work/services/fgs/service-types), [timeouts](https://developer.android.com/develop/background-work/services/fgs/timeout) (checked 2026-11-01).

For target 34+, declare each FGS type in Play Console App content as well as the manifest. Supply functionality, why deferral/interruption harms users, a demonstration video showing the user's trigger, and the actual use case. **Recommendation:** demonstrate consent, finite workload, notification/progress and Stop; explicitly disclose third-party compute and `specialUse` rationale. User-initiated transfer jobs are recommended for appropriate explicit network transfers, not arbitrary computation. [Play Console requirements](https://support.google.com/googleplay/android-developer/answer/13392821?hl=en) (checked 2026-11-01).

## Power management and reliability

Doze defers CPU/network work, jobs and syncs during idle periods, with maintenance windows. App Standby defers background network activity for inactive apps. Battery-optimization exemption is only partial; users can manage it in settings, and direct exemption requests have restricted acceptable uses. **Recommendation:** do not require blanket exemption, silently change settings, use fake notifications or bypass power management. Test recovery after idle and permission revocation. [Doze and App Standby](https://developer.android.com/training/monitoring-device-state/doze-standby) (checked 2026-11-01).

Standby buckets prioritize apps by usage and constrain resources: active, working set, frequent, rare and restricted. Android 16 introduces job runtime quotas even for active apps. Do not assume charging, an FGS, or an “active” bucket makes all work unconstrained. [Standby buckets](https://developer.android.com/topic/performance/appstandby), [resource limits](https://developer.android.com/topic/performance/power/power-details) (checked 2026-11-01).

**Secondary source, explicitly requested exception:** Don't Kill My App documents vendor-specific background restrictions. It is community evidence, not Android/Play policy or proof about the owner's phone; no vendor rankings are used as a pass criterion. **Recommendation:** test actual OEM/OS builds with screen off, charging/unplugged, thermal load, user Stop and process death. OEM survival time and supported-model list are **unknown** until measured. [Don't Kill My App](https://dontkillmyapp.com/) (checked 2026-11-01).

## Nearby connectivity

| Transport | Availability and permissions |
|---|---|
| Wi-Fi Direct | Devices can discover/connect without an access point; support must be checked, not assumed on every phone. Declare Wi-Fi/network/socket permissions from the guide. For target 33+, `discoverPeers()`/`connect()` need runtime `NEARBY_WIFI_DEVICES`; older targets use fine location. The guide also identifies discovery APIs requiring Location Mode enabled. Use `neverForLocation` only if the app really does not derive location. [Wi-Fi Direct guide](https://developer.android.com/develop/connectivity/wifi/wifip2p) (checked 2026-11-01). |
| Wi-Fi Aware | API 26+, supported hardware only: check `FEATURE_WIFI_AWARE` and current `isAvailable()`, handle availability changes. Discovers and forms data paths without an access point; resources/concurrency can affect availability. Target 33+ needs `NEARBY_WIFI_DEVICES`; retain applicable earlier-version location and Wi-Fi/network permissions. [Wi-Fi Aware guide](https://developer.android.com/develop/connectivity/wifi/wifi-aware) (checked 2026-11-01). |

**Recommendation:** ordinary LAN plus authenticated pairing first, capability-gated Direct/Aware adapters later. Android-to-iPhone Aware interoperability, simultaneous infrastructure connectivity and useful throughput on target phones are **unknown until validated**; neither permission nor discovery authorizes compute jobs. [Aware](https://developer.android.com/develop/connectivity/wifi/wifi-aware), [Direct](https://developer.android.com/develop/connectivity/wifi/wifip2p) (checked 2026-11-01).

## Decision: Android helper sessions — GO-WITH-LIMITS

This is a **conditional project recommendation**, not a passed shipping gate: the VM/interpreter exception and `specialUse` mechanism provide a plausible route, while Play approval and real-device reliability remain **unknown**. [Code policy](https://support.google.com/googleplay/android-developer/answer/16559646?hl=en), [service types](https://developer.android.com/develop/background-work/services/fgs/service-types), [Play review declaration](https://support.google.com/googleplay/android-developer/answer/13392821?hl=en) (checked 2026-11-01).

Conditions proposed for the platform:

1. Explicit finite user-started sessions; clear third-party compute disclosure, progress notification, immediate Stop, bounded CPU/memory/data/thermal/power policies.
2. Runtime delivered through Play; interpreted, verified, policy-screened Wasm only; no downloaded native libraries, mining, ambient host access or power-management bypass.
3. Honest FGS type and approved Console declaration; respect timeout/start rules and user/system termination. If `specialUse` is rejected, restrict to visible foreground operation rather than mislabel work.
4. Task 0.9a must measure binary/RSS/thermal/performance, timeout handling, process-death recovery and one-second Stop on actual supported phones. Required project sandbox layers must be demonstrated or jobs refused; this policy document does not waive them.
5. No guaranteed unattended overnight availability. Use short retryable leases/checkpoints and count phone capacity as interruptible.

These are recommendations inferred from the dated policy, timeout and power sources above. **Recommended v1:** requester first; Android helping stays opt-in and gated on 0.9a plus Play review. No measurements or acceptance results are invented here.
