# 1.L8b: hosted-runner procfs bootstrap failure

## Evidence and cause

Diagnostic commit `1cb53a24604db5fe8e96acac9ea543629e3b2ded`:
[CI 41540856262](https://github.com/NavodCaldera/Network-Connector/actions/runs/41540856262/job/125604321713).
All 10 sandbox tests failed closed with:

```text
SandboxError { step: "mount:proc", detail: "Operation not permitted (os error 1)" }
test result: FAILED. 0 passed; 10 failed; 0 ignored
```

The root-private, tmpfs, bind, read-only remount, pivot and old-root unmount calls had already
succeeded. The fresh procfs mount was attempted after the inherited root and procfs had been
detached. The runner's kernel checks for an already-visible procfs in an unprivileged mount
namespace; absence returns EPERM via `mount_too_revealing`.
See [Linux v7.0 fs/namespace.c](https://github.com/torvalds/linux/blob/v7.0/fs/namespace.c#L5952-L6027).

Fixed commit `890ee73cc36bbbb10ffa8a3cda0a46aed1c74ea6`:
[CI 41541545484](https://github.com/NavodCaldera/Network-Connector/actions/runs/41541545484/job/125606964458).
The only bootstrap behavior change is mounting a **fresh** procfs under the new tmpfs before
pivot/detach, already in the PID-namespace init process. This is never a host-proc bind mount.
All namespaces, read-only binds, pivot/detach, proc mount restrictions, seccomp, capability drop,
no_new_privs, fd/environment clearing, rlimits, suspended start and cgroup limits remain required.
No privileged worker, fallback, ignored failure or continue-on-error was introduced.

## Runner inspection and successful test output

The workflow inspects kernel version, userns sysctls, aa-status, mountinfo outside and inside the
transient unit, its AppArmor context, and fixed systemd restriction properties. No environment
dump or job data is printed. Ubuntu 24.04 runner output:

```text
Linux ... 7.0.0-1017-azure #17~24.04.1-Ubuntu SMP PREEMPT ... x86_64 GNU/Linux
kernel.apparmor_restrict_unprivileged_userns = 0
kernel.apparmor_restrict_unprivileged_userns_complain = 0
kernel.apparmor_restrict_unprivileged_userns_force = 0
PrivateTmp=no
PrivateMounts=no
ProtectSystem=no
NoNewPrivileges=no
RestrictNamespaces=no
ProtectProc=default
ProcSubset=all
unconfined
```

Mountinfo showed the ordinary ext4 root and procfs with a binfmt_misc child mount, without
container-style masked proc files. No additional AppArmor sysctl, systemd restriction change,
source-library mount-flag workaround, container, or replacement runner was needed.

Exact real-kernel test commands, invoked by the existing delegated-service harness:

```bash
cargo build -p nc-exec --bin nc-exec-worker --example sandbox_probe -j 2 --locked
cargo test -p nc-exec --test sandbox_linux -j 2 --locked -- --ignored --nocapture --test-threads 1
```

Hosted-runner output:

```text
external watchdog and Stop: passed; Stop returned in 3.960365ms
probe isolation: exit=0 events=[] peak=905216
kill latency=1.234614ms
teardown: processes gone; drop: cgroup gone; sweep: stale removed, live retained
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.98s
```

The regression probe reads procfs kernel state and verifies that only PID 1 is visible and that
/proc is a proc filesystem with ro,nosuid,nodev,noexec. It reports only success/failure. The suite
also verifies filesystem/environment isolation, forbidden socket/exec/ptrace/unshare/mount/fork,
memory and process limits, malicious Wasm jobs, suspended start, watchdog/Stop and cleanup/sweep.
The ordinary Linux executor unit suite includes unavailable-userns/cgroup refusal coverage.

WSL2 Ubuntu 24.04, kernel `6.6.87.2-microsoft-standard-WSL2`, unprivileged UID 1000, also passed
all 10 tests after the ordering change (kill 3.739276 ms; Stop 8.862134 ms). No Docker options apply.
Final local and Windows verification is recorded in WORKLOG.md.

The entire fix CI run completed successfully: both Windows/Linux core and server fmt, clippy and
tests, both cargo-deny jobs, Flutter and Docker stack. Windows sandbox tests: 12 passed, 1 ignored
(the existing cold/warm measurement); Windows nc-agent real sandbox_jobs: 4 passed. Local Windows
workspace clippy also passed (-j 2). The systemd properties above describe the trusted test service;
the worker sets no_new_privs itself and its probe verifies enforcement.
