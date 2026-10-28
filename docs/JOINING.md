# Joining nearby requesters without a code (0.3.0)

In 0.3.0 a **requester** ("Ask for performance", the old controller) can let nearby **helpers** ("Give performance",
the old workers) join in one click, with no pairing code. Pairing codes still work, and a requester can keep
requiring them (a *closed group*).

> Status: identity, discovery, the join protocol, trust/block, Verify, automatic joining with owner messages, and
> Wi-Fi Direct groups without a router are built and tested on one PC. Discovery and joining on real networks
> (router, Wi-Fi Direct, hotspot) still need the hardware checks at the end.

## Device identity

Every install has one device key and certificate in `%LOCALAPPDATA%\NetworkConnector\identity\`
(ACL-protected to your user).

- **Device ID** = SHA-256 of the certificate. **Short ID** = its first 8 characters, shown as `AB12-CD34`.
- **Name** = your computer name, or what you set with `nc identity --name "Navod's i7"`.
- On a PC that already ran a 0.2.0 controller, the existing `controller\server.crt` and `server.key` *become* the
  device identity. The fingerprint doesn't change, so paired workers keep working without pairing again.
- A new requester uses the device certificate as its TLS certificate. Nothing ever replaces an existing one.

`nc identity` shows all of this.

## Requester: who may join

Controller window → **Devices** tab, or `nc policy`:

| Policy | Who can join without a code |
|---|---|
| **Anyone nearby** (default for new installs) | Any device that proves its identity |
| **Trusted only** | Only devices you marked Trusted |
| **Ask me** | You click **Accept** or **Decline** for each new device (`nc devices accept <id>`) |
| **Require pairing code** | Nobody. `/join` is off; only Part 1 pairing works |

A controller upgraded from 0.2.0 keeps **Require pairing code** until you change it, so an existing closed i7 + i3
setup doesn't open by itself.

While the policy isn't "Require pairing code" and **Announce** is on, the requester sends a small signed announcement
every 2 seconds on each of its listening addresses (never `0.0.0.0`). Everyone on that network can read the name, the
short ID, the number of helpers and the optional **Label**. Don't put anything private in the label.

**Privacy:** helpers see the inputs of the jobs they run. Don't send private data to helpers you don't trust; use
**Trusted only** for that.

**Block** a device (Devices tab, or `nc block <id>`) and its helper tokens stop working at once; it can't join again
until you unblock it. **Close** (`POST /admin/close`) stops handing out work: helpers that joined finish their running
job and then leave by themselves. Paired helpers wait for you to come back. Quitting the app or changing roles does
this for you: it announces "closing", lets running chunks finish for up to 20 seconds, then stops.

**Double-check** (Devices tab, `nc policy --redundancy`): the share of open-group results that also run on a second
helper. A helper caught returning wrong results loses score (the **Score** column) and is dropped. See
[SANDBOX.md](SANDBOX.md#checking-results-protects-the-requester).

## Helper: joining

Worker window → **Nearby requesters**. The list shows each requester's name, short ID, a Trusted/Blocked badge, its
group state, helpers, how much work is waiting, and the link type.

1. Select a requester and click **Join**. Confirm. That's all. If its owner chose **Ask me**, the window waits up to
   2 minutes for them to accept.
2. The helper starts right away with the limits in section 2 of the window.
3. **Leave** gives unfinished jobs back and removes this computer from that requester. Coming back is one click.

### Without a click: When a requester asks

Section 2 of the window, **When a requester asks** (`join_mode`). Joining by itself needs a working sandbox; until
`nc sandbox status --check` says available, every join is a click.

| Setting | What happens when an open requester appears nearby |
|---|---|
| **Accept automatically and tell me** (default) | The helper joins and shows a message: "Navod's i7 (AB12-CD34) is using 4 of your CPU threads", with **Stop** |
| **Ask me each time** | A message with **Accept** / **Decline**. No answer within 60 s means Decline. It also appears in the window |
| **Trusted requesters only** | Joins by itself only requesters you marked Trusted; others need a click |

Messages are Windows notifications when they are allowed, otherwise the tray balloon, otherwise the app window. They
show only the requester's name, short ID and thread count, never job contents. **Stop** (in the message or the tray)
kills the running jobs within a second and leaves the group. **Block this requester** (under "What is running") stops
its work and never helps it again.

Limits are checked on the helper, before taking each job and while jobs run: only while plugged in, only when idle,
allowed hours, a CPU cap, and a daily CPU-minute limit per requester. When a limit is reached the helper stops taking
work and says why; the running job finishes, unless you ticked *stop running jobs at once*.

Command line:

```powershell
nc nearby                                              # listen for 6 s (receive-only) and probe the direct link
nc worker connect --to 192.168.1.4:8765 --device-id <ID from nc nearby>
nc worker run --requester AB12-CD34
nc worker leave --requester AB12-CD34
```

Joined requesters are stored in `%LOCALAPPDATA%\NetworkConnector\requesters\<id>\`, separate from the Part 1 pairing
in `worker\`, so joining never overwrites a pairing.

### Nothing in the list? ("Discovery blocked? Use Find controller")

Announcements are UDP packets to port 8766. They don't arrive when:

- the network is marked **Public** (Wi-Fi Direct and Mobile Hotspot usually are), or a firewall rule blocks them;
- port 8766 is already used by another program on this PC (the window says "Discovery port busy");
- the router blocks multicast between Wi-Fi clients.

Click **Search**: it asks the direct-link gateway and any address typed in the window over an outbound TLS connection,
which needs no inbound firewall rule. **Find controller** in section 0 works as in 0.2.0.

Or click **Allow discovery through firewall** (Windows asks for administrator approval). It adds
`scripts\add-firewall-rule.ps1 -Discovery`: inbound **UDP 8766 only**, for this app's base `python.exe` only, on
Private networks from the local subnet, plus, while a direct link is up, on that link's address from its /24 subnet.
It never allows Python as a whole. An existing **Block** rule for Python still wins; remove that one in Windows
Defender Firewall if you created it by accident. `scripts\remove-firewall-rule.ps1 -Discovery` removes the rules.

## No router: Wi-Fi Direct groups

A requester with an open policy that starts its group from the **Wi-Fi Direct** tab (leaving SSID and passphrase blank)
calls it `DIRECT-NC-XXXX` (the first 4 characters of its device ID) and derives the passphrase from that name, so every
copy of the app can join it with one click. **The Wi-Fi password is not the security boundary**: the sandbox, the
signatures, pinned TLS and result checks are. A closed (pairing-code) group keeps a random passphrase.

On the helper, **Join nearby Wi-Fi Direct groups automatically** (off by default) scans for `DIRECT-NC-XXXX` at most
every 30 seconds:

- when this PC is on **no** Wi-Fi network, it joins the group by itself, finds the requester with Search, and then
  follows *When a requester asks*;
- when it is on Wi-Fi, it only offers **Join DIRECT-NC-XXXX (disconnects <your Wi-Fi>)**: most adapters, including
  the i7's AX203, can't stay on two networks at once;
- **Leave** removes the Wi-Fi profile it created, and Windows reconnects your usual network.

If Wi-Fi is off, the window shows **Wi-Fi is off — Turn on**. Only that click asks Windows to turn the radio on; the app
never turns a radio off.

When Windows asks whether Python may use networks, allow **Private** only. Don't tick Public: that would allow every
port for every Python program.

## Verify (optional, only to mark a device Trusted)

Verify checks that both computers really talk to each other, like Bluetooth's "do these numbers match?".

1. On the helper, while joined: **Verify** (or `nc worker verify --requester <id>`). It shows a 6-digit number.
2. On the requester: the **Devices** tab shows the same number for that helper under "Verify".
3. If they match, click **Trust** on both sides. If they differ, don't trust: you are not connected to the computer
   you think.

The requester commits to its random number before it sees the helper's, so nobody in between can pick keys that give
the same 6 digits. Verify is never required to help. It only unlocks "Trusted only".

## What a nearby announcement can and can't do

- The helper's discovery socket only **receives**. It never replies and never runs anything.
- A packet that is too big, malformed, older than 10 seconds, replayed, wrongly signed, from this computer, or not from
  the address it announces, is dropped. Floods are rate limited per sender and overall.
- A valid announcement can only add a line to the list. Joining needs your click or your *When a requester asks*
  setting (and a working sandbox), and then it's an **outbound** TLS connection whose certificate must hash to the
  announced device ID.
- The signature can't prove a stranger is honest. It stops anyone from posing as a device you already trust or blocked.

## Settings

See [CONFIGURATION.md](CONFIGURATION.md#joining-and-discovery-030).

## Hardware checks still to run

These need the real PCs (PLAN §22.3) and are not verified yet:

- [ ] Router: the i3 shows the i7 in the list within 5 s; one click joins and runs a job.
- [ ] Auto-accept: the i3 joins by itself and shows the message; **Stop** in the message stops it within 1 s.
- [ ] Ask me each time: the i3 shows Accept / Decline; no answer for 60 s declines.
- [ ] No router: the i7 starts `DIRECT-NC-XXXX`; the i3 (not on Wi-Fi) joins it by itself with the setting on, or offers
      "Join (disconnects …)" while on Wi-Fi; Leave restores its Wi-Fi.
- [ ] Discovery firewall rule: with it, announcements arrive on the direct link (Public); without it, Search still works.
- [ ] Tray icon and toasts appear; the toast buttons work while the app runs.
- [ ] Wi-Fi Direct and hotspot: Search finds the i7 when announcements are blocked (Public link).
- [ ] Ask me: the i3 waits, the i7 accepts, the i3 starts.
- [ ] Block on the i7: the i3 stops at once and can't join again.
- [ ] Verify: both screens show the same 6 digits.
- [ ] `Get-NetTCPConnection -State Listen` on the i3 shows no Network Connector process; `Get-NetUDPEndpoint -LocalPort 8766`
      shows only per-interface addresses, never `0.0.0.0`.
