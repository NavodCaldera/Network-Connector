# Task 0.5: two PCs on two different ISPs

Purpose: find out, with **real measurements**, how often two of our PCs on different Sri Lankan ISPs can connect
**directly** (same LAN, internet direct, or hole-punched with DCUtR) and how often they need **our relay**, and the
throughput on each path. Nothing here creates a cloud resource.

You need:

- **PC A** on **ISP 1** (e.g. home fibre/ADSL Wi-Fi).
- **PC B** on **ISP 2** (e.g. a phone hotspot on a different mobile operator than ISP 1).
- About 30 minutes. Both PCs Windows 10/11.
- For test 3 only: a **relay** that both PCs can reach (see test 3).

Write down, for every run: the ISP name, connection type (fibre / 4G hotspot / ...), and the time of day.

---

## 0. Get the probe onto both PCs (once)

On the development PC (this repo, Rust installed), build the probe:

```powershell
cd "<repo>\core"
cargo build --release -p nc-net --bin nc-netprobe
```

The binary is `nc-netprobe.exe` in the Cargo target folder (`$env:CARGO_TARGET_DIR\release\` if set, else
`core\target\release\`). Copy that one file to a folder on each PC, for example `C:\nc-probe\`. Open PowerShell in that
folder on each PC for every step below.

Each PC makes its own probe key the first time (file `nc-probe.key` in that folder; it is a throwaway test key, not a
device key). Print the peer id on **both** PCs and write them down:

```powershell
.\nc-netprobe.exe id
```

When Windows Firewall asks whether `nc-netprobe` may communicate, tick **Private and Public** and allow. (Afterwards
you can remove the rule in *Windows Defender Firewall > Allow an app*.)

---

## 1. Baseline: same LAN (both PCs on the same Wi-Fi)

PC A:
```powershell
.\nc-netprobe.exe listen
```
It prints a long line starting with `nc-pair:v1:`. This is a **one-time secret** (valid 10 minutes). Copy it to PC B
(e.g. type it, or send it to yourself in a private chat and delete it afterwards). Do **not** paste it into the
report.

PC B:
```powershell
.\nc-netprobe.exe dial "nc-pair:v1:...the line from PC A..."
```
Both PCs show a 6-digit **short authentication string**. If the digits are the same on both screens, type `y` and
Enter on **both** PCs. If they differ, type `n` (and tell the coordinator: that means something interfered).

Expected: rung `lan_direct`. PC B prints a block between `=== NC-NETPROBE RESULT ===` and `=== END ===`.
Stop PC A with Ctrl+C.

---

## 2. Different ISPs, no relay (tests whether a plain direct connection works)

Move PC B to ISP 2 (phone hotspot). PC A stays on ISP 1.

PC A: `.\nc-netprobe.exe listen`
PC B: `.\nc-netprobe.exe dial "nc-pair:v1:..."`

This only works if PC A has a reachable public address (no CGNAT, or a router port-forward of UDP+TCP **4002** to
PC A). A failure here (error `timed out` or `Stream ...`) is a **valid result**: paste it. Then also try it the other
way round (PC B runs `listen`, PC A runs `dial`).

---

## 3. Different ISPs through a relay (DCUtR hole punching, relay fallback)

A relay must be reachable from both PCs on UDP and TCP port **4001**. Options, cheapest first:

1. **A third machine or one of the two PCs with a public IPv4** and a router port-forward of UDP+TCP 4001 to it. Ask
   your ISP whether your line has a public IP (many home and mobile connections are behind CGNAT and cannot do this).
2. **A small cloud VM.** This costs money: **do not create one yourself for this test**; tell the coordinator, who asks
   for your approval first.

On the relay machine (it only serves the two peer ids you give it; it is not an open relay):
```powershell
.\nc-netprobe.exe relay --allow <peer id of PC A> --allow <peer id of PC B>
```
It prints lines like `relay listening on /ip4/<address>/udp/4001/quic-v1/p2p/<relay id>`. Take the line with the
**public** IP (if it shows only private addresses, use `/ip4/<your public IP>/udp/4001/quic-v1/p2p/<relay id>`).
Call that `RELAY`.

PC A (ISP 1):
```powershell
.\nc-netprobe.exe listen --relay "RELAY"
```
Wait for `relay reservation accepted`, then copy the `nc-pair:v1:` line to PC B.

PC B (ISP 2):
```powershell
.\nc-netprobe.exe dial "nc-pair:v1:..." --relay "RELAY"
```

PC B first connects through the relay (`rung=Relayed`), then DCUtR tries to punch a direct hole (`dcutr ... ok=true`
and a `hole_punched` connection, or `ok=false` with a reason). The test then runs over the best path it got.

**Repeat test 3 five times** (new `listen` each time, because each offer works once), and once with PC A and PC B
swapped. Five runs per direction let us say "direct in N of 5".

Optional, if you have the time: repeat test 3 at night (the cheap-data window) and with a different mobile operator.

---

## What to paste back to the coordinator

For **every** run (tests 1, 2 and each test 3 run):

1. A header line you type yourself, e.g. `Test 3 run 2 - A: SLT fibre, B: Dialog 4G hotspot - 21:40`.
2. From **PC B** (the `dial` side): everything from `=== NC-NETPROBE RESULT ===` to `=== END ===`, or the full error
   if it failed.
3. From **PC A** (the `listen` side): the lines containing `connected`, `dcutr`, `autonat`, `REJECTED`, `relay
   reservation` and `final traffic`.
4. From the relay (test 3): the lines with `CircuitReqAccepted`, `CircuitReqDenied`, `ReservationReqAccepted` or
   `ReservationReqDenied`.

Never paste the `nc-pair:v1:` line or the `nc-probe.key` file.

The result block contains: the connection rung(s) used at test time (`lan_direct`, `internet_direct`,
`hole_punched`, `relayed`), upload and download Mbit/s, bytes counted as direct vs relayed, and the event log.
The coordinator turns these into the direct % / relay % / throughput table in `spikes/phase0/REPORT.md`.

---

## Cleaning up

Delete the `C:\nc-probe\` folder on each PC (this removes the throwaway keys) and the firewall rule.
