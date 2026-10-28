# Joining nearby requesters without a code (0.3.0, in development)

In 0.3.0 a **requester** ("Ask for performance", the old controller) can let nearby **helpers** ("Give performance",
the old workers) join in one click, with no pairing code. Pairing codes still work, and a requester can keep
requiring them (a *closed group*).

> Status: Phase 7. Identity, discovery, the join protocol, trust/block and Verify are built and tested on one PC.
> The sandbox (Phase 8) is not built yet, so a helper never joins **automatically**: you click **Join**. Discovery on
> real networks (router, Wi-Fi Direct, hotspot) still needs the hardware checks below.

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
until you unblock it. **Close** (`POST /admin/close`, the tray in Phase 9) stops handing out work: helpers that joined
finish their running job and then leave by themselves. Paired helpers wait for you to come back.

## Helper: joining

Worker window → **Nearby requesters**. The list shows each requester's name, short ID, a Trusted/Blocked badge, its
group state, helpers, how much work is waiting, and the link type.

1. Select a requester and click **Join**. Confirm. That's all. If its owner chose **Ask me**, the window waits up to
   2 minutes for them to accept.
2. The helper starts right away with the limits in section 2 of the window.
3. **Leave** gives unfinished jobs back and removes this computer from that requester. Coming back is one click.

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
which needs no inbound firewall rule. **Find controller** in section 0 works as in 0.2.0. A narrow firewall rule for
discovery is planned but not added yet (it needs your approval).

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
- A valid announcement can only add a line to the list. Joining always needs your click (or, after Phase 8, your
  auto-accept setting), and then it's an **outbound** TLS connection whose certificate must hash to the announced
  device ID.
- The signature can't prove a stranger is honest. It stops anyone from posing as a device you already trust or blocked.

## Settings

See [CONFIGURATION.md](CONFIGURATION.md#joining-and-discovery-030).

## Hardware checks still to run

These need the real PCs (PLAN §22.3) and are not verified yet:

- [ ] Router: the i3 shows the i7 in the list within 5 s; one click joins and runs a job.
- [ ] Wi-Fi Direct and hotspot: Search finds the i7 when announcements are blocked (Public link).
- [ ] Ask me: the i3 waits, the i7 accepts, the i3 starts.
- [ ] Block on the i7: the i3 stops at once and can't join again.
- [ ] Verify: both screens show the same 6 digits.
- [ ] `Get-NetTCPConnection -State Listen` on the i3 shows no Network Connector process; `Get-NetUDPEndpoint -LocalPort 8766`
      shows only per-interface addresses, never `0.0.0.0`.
