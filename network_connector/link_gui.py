"""Direct-link controls. All network and operating-system calls run in Background."""
from __future__ import annotations

import base64
import ipaddress
import json
import subprocess
import sys
import tkinter as tk
from pathlib import Path
from tkinter import messagebox, ttk
from typing import Any

from .config import WorkerConfig, load_config, save_config
from .security import constant_time_equal, fingerprint_pem_file


def discover_controller(cfg: WorkerConfig, directory: Path) -> dict[str, str]:
    """Probe a bounded address list; paired workers accept only their pinned identity."""
    from .wifidirect import candidate_controllers, probe

    expected = getattr(cfg, "controller_fingerprint", "")
    if cfg.token:
        expected = fingerprint_pem_file(directory / "controller.crt")
    hosts = list(dict.fromkeys(([cfg.controller_host] if cfg.controller_host else [])
                              + getattr(cfg, "known_hosts", []) + candidate_controllers()))[:8]
    for host in hosts:
        fingerprint = probe(host, cfg.controller_port)
        if fingerprint and (not expected or constant_time_equal(fingerprint, expected)):
            return {"host": host, "fingerprint": fingerprint}
    if expected:
        raise RuntimeError("No reachable controller matched the pinned certificate. Check the link and known addresses.")
    raise RuntimeError("No controller found. Join the direct Wi-Fi first, or enter its address manually.")


def firewall_status(host: str) -> str:
    if not host:
        return "Start the link before checking its firewall rule."
    ipaddress.IPv4Address(host)
    script = ("$r=Get-NetFirewallRule -DisplayName 'Network Connector Controller (Wi-Fi Direct)' "
              "-ErrorAction SilentlyContinue; if($r){$a=$r|Get-NetFirewallAddressFilter; "
              "[pscustomobject]@{enabled=[string]$r.Enabled;action=[string]$r.Action;local=$a.LocalAddress;remote=$a.RemoteAddress}|ConvertTo-Json -Compress}")
    proc = subprocess.run(["powershell", "-NoProfile", "-NonInteractive", "-Command", script],
                          capture_output=True, text=True, timeout=10)
    if proc.returncode:
        return "Firewall status unavailable."
    if not proc.stdout.strip():
        return "Direct-link firewall rule missing."
    data = json.loads(proc.stdout)
    local = data.get("local", [])
    if isinstance(local, str):
        local = [local]
    remote = data.get("remote", [])
    if isinstance(remote, str):
        remote = [remote]
    subnet = str(ipaddress.IPv4Network(f"{host}/24", strict=False))
    safe = host in local and subnet in remote and data.get("enabled") == "True" and data.get("action") == "Allow"
    return "Direct-link rule enabled for this address and subnet." if safe else "Firewall rule needs updating for this address."


def allow_direct_firewall(host: str, port: int) -> None:
    address = str(ipaddress.IPv4Address(host))
    if not 1 <= port <= 65535:
        raise ValueError("Invalid controller port")
    script = Path(__file__).resolve().parents[1] / "scripts" / "add-firewall-rule.ps1"
    if not script.is_file():
        script = Path(sys.prefix) / "share" / "network_connector" / "scripts" / "add-firewall-rule.ps1"
    if not script.is_file():
        raise RuntimeError("Firewall setup script is missing. Run from a full project installation.")
    quoted = str(script).replace("'", "''")
    command = ("$p=Start-Process -FilePath powershell.exe -Verb RunAs -WindowStyle Hidden -Wait -PassThru "
               f"-ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-File','\"{quoted}\"',"
               f"'-Port','{port}','-WiFiDirect','-LocalAddress','{address}'); exit $p.ExitCode")
    encoded = base64.b64encode(command.encode("utf-16-le")).decode("ascii")
    proc = subprocess.run(["powershell", "-NoProfile", "-NonInteractive", "-EncodedCommand", encoded],
                          capture_output=True, text=True, timeout=120)
    if proc.returncode:
        raise RuntimeError("Firewall setup was declined or failed. Run scripts/add-firewall-rule.ps1 as administrator.")


class ControllerDirectControls:
    def _build_direct(self, nb: ttk.Notebook) -> None:
        tab = ttk.Frame(nb, padding=12)
        nb.add(tab, text="Wi-Fi Direct")
        ttk.Label(tab, text="Connect the helper computer without a router. Windows and adapter support varies.",
                  wraplength=950).pack(anchor="w")
        self.direct_ssid = tk.StringVar()
        self.direct_password = tk.StringVar()
        self.direct_listen = tk.BooleanVar(value=True)
        self.direct_reuse = tk.BooleanVar(value=False)
        self._direct_data: dict[str, Any] = {}
        self._direct_busy = False
        self._direct_last_poll = 0.0
        fields = ttk.Frame(tab)
        fields.pack(fill="x", pady=10)
        for r, (label, var) in enumerate((("SSID (blank: generate)", self.direct_ssid),
                                         ("Passphrase (blank: generate)", self.direct_password))):
            ttk.Label(fields, text=label).grid(row=r, column=0, sticky="w", pady=4)
            ttk.Entry(fields, textvariable=var, width=36, show="*" if r else "").grid(row=r, column=1, padx=8)
            ttk.Button(fields, text="Copy", command=lambda v=var: self._direct_copy(v.get())).grid(row=r, column=2)
        buttons = ttk.Frame(tab)
        buttons.pack(fill="x")
        for label, fn in (("Start group / detect hotspot", self._direct_start), ("Stop group", self._direct_stop),
                          ("Refresh status", self._direct_refresh), ("Allow through firewall", self._direct_firewall)):
            ttk.Button(buttons, text=label, command=fn).pack(side="left", padx=3)
        ttk.Checkbutton(tab, text="Listen on the direct link", variable=self.direct_listen,
                        command=self._direct_set_listen).pack(anchor="w", pady=8)
        ttk.Checkbutton(tab, text="Reuse these credentials on the next start", variable=self.direct_reuse).pack(anchor="w")
        self.direct_status = ttk.Label(tab, text="Start the controller first.", wraplength=950, justify="left")
        self.direct_status.pack(anchor="w", pady=5)
        self.direct_firewall = ttk.Label(tab, text="", wraplength=950)
        self.direct_firewall.pack(anchor="w")
        self.direct_qr = ttk.Label(tab)
        self.direct_qr.pack(anchor="w", pady=5)
        self._direct_qr_key = None
        ttk.Label(tab, text="If the publisher is unavailable, start Windows Mobile Hotspot in Settings, then refresh. "
                  "Stop a manually started hotspot in Settings. Install dependencies on both PCs before going offline.",
                  wraplength=950).pack(anchor="w", pady=8)
        self.root.after(3000, self._direct_tick)

    def _direct_copy(self, text: str) -> None:
        self.root.clipboard_clear()
        self.root.clipboard_append(text)

    def _direct_error(self, exc: Exception) -> None:
        self._direct_busy = False
        self.direct_status.config(text=str(exc))

    def _direct_start(self) -> None:
        admin = self._need_admin()
        if not admin or self._direct_busy:
            return
        ssid, password, listen = self.direct_ssid.get().strip(), self.direct_password.get(), self.direct_listen.get()
        self._direct_busy = True
        self.direct_status.config(text="Starting group or detecting an existing hotspot...")
        self.bg.run(lambda: admin.direct_start(ssid or None, password or None, listen), self._direct_render, self._direct_error)

    def _direct_stop(self) -> None:
        if self.admin and not self._direct_busy:
            admin = self.admin
            self._direct_busy = True
            self.bg.run(admin.direct_stop, self._direct_render, self._direct_error)

    def _direct_set_listen(self) -> None:
        if self.admin:
            admin, enabled = self.admin, self.direct_listen.get()
            self.bg.run(lambda: admin.direct_listen(enabled), self._direct_render, self._direct_error)

    def _direct_tick(self) -> None:
        self.root.after(5000, self._direct_tick)
        self._direct_refresh()

    def _direct_refresh(self) -> None:
        if self.admin and not self._direct_busy:
            self._direct_busy = True
            self.bg.run(self.admin.direct_status, self._direct_render, self._direct_error)

    def _direct_render(self, data: dict[str, Any]) -> None:
        self._direct_busy = False
        if self._direct_data.get("running") and not data.get("running") and not self.direct_reuse.get():
            self.direct_ssid.set("")
            self.direct_password.set("")
        self._direct_data = data
        if data.get("ssid"):
            self.direct_ssid.set(data["ssid"])
        if data.get("passphrase"):
            self.direct_password.set(data["passphrase"])
        host = data.get("ip", "")
        peers = data.get("peers") or []
        self.direct_status.config(text=f"Mode: {data.get('mode', '?')} | {'Running' if data.get('running') else 'Stopped'}\n"
                                  f"Interface: {data.get('interface') or '-'} | IP: {host or '-'} | "
                                  f"Network: {data.get('category') or 'unknown'}\nPeers: {', '.join(map(str, peers)) or 'none reported'}\n"
                                  f"{data.get('error') or data.get('status') or ''}")
        if "listeners" in data and host:
            self.direct_listen.set(host in data["listeners"])
        if hasattr(self, "direct_pair_hint"):
            hint = f"Controller certificate: {data.get('fingerprint', '')}"
            if host:
                hint = f"Connect the worker to Wi-Fi {data.get('ssid') or '(your hotspot)'}, then use {host}:{self.cfg.port}.\n" + hint
            self.direct_pair_hint.config(text=hint)
        if host:
            self.bg.run(lambda: firewall_status(host), lambda s: self.direct_firewall.config(text=s),
                        lambda _e: self.direct_firewall.config(text="Firewall status unavailable."))
        self._direct_render_qr(data)

    def _direct_render_qr(self, data: dict[str, Any]) -> None:
        key = (data.get("ssid"), data.get("passphrase"))
        if key == self._direct_qr_key:
            return
        self._direct_qr_key = key
        if not all(key):
            self.direct_qr.config(image="", text="")
            return
        try:
            import qrcode
            from PIL import ImageTk
            def escape(value: str) -> str:
                return "".join("\\" + c if c in '\\;,:"' else c for c in value)
            code = qrcode.make(f"WIFI:T:WPA;S:{escape(key[0])};P:{escape(key[1])};;")
            self._direct_qr_image = ImageTk.PhotoImage(code.resize((180, 180)))
            self.direct_qr.config(image=self._direct_qr_image)
        except ImportError:
            self.direct_qr.config(text="Optional: install qrcode to display a Wi-Fi QR code.")

    def _direct_firewall(self) -> None:
        host = self._direct_data.get("ip")
        if not host:
            messagebox.showinfo("Firewall", "Start the direct link first.")
            return
        port = self.cfg.port
        self.direct_firewall.config(text="Waiting for Windows administrator approval...")
        self.bg.run(lambda: allow_direct_firewall(host, port), lambda _r: self._direct_refresh(),
                    lambda e: self.direct_firewall.config(text=str(e)))


POLICY_LABELS = {"anyone": "Anyone nearby", "trusted": "Trusted only", "ask": "Ask me", "code": "Require pairing code"}


class ControllerJoinControls:
    """Requester side: join policy, announcements, devices (trust/block), Ask-me requests and Verify numbers."""

    def _build_devices(self, nb: ttk.Notebook) -> None:
        tab = ttk.Frame(nb, padding=12)
        nb.add(tab, text="Devices")
        self.identity_label = ttk.Label(tab, text="Start the controller to see this requester's identity.",
                                        font=("Segoe UI", 10, "bold"))
        self.identity_label.pack(anchor="w")
        box = ttk.LabelFrame(tab, text="Who may join without a pairing code", padding=8)
        box.pack(fill="x", pady=6)
        self.policy_var = tk.StringVar(value=POLICY_LABELS.get(self.cfg.join_policy, "Anyone nearby"))
        self.announce_var = tk.BooleanVar(value=self.cfg.announce)
        self.announce_label_var = tk.StringVar(value=self.cfg.announce_label)
        ttk.Combobox(box, values=list(POLICY_LABELS.values()), textvariable=self.policy_var, state="readonly",
                     width=24).grid(row=0, column=0, sticky="w")
        ttk.Checkbutton(box, text="Announce to nearby computers", variable=self.announce_var).grid(row=0, column=1, padx=10)
        ttk.Label(box, text="Label").grid(row=0, column=2)
        ttk.Entry(box, textvariable=self.announce_label_var, width=24).grid(row=0, column=3, padx=4)
        ttk.Button(box, text="Save", command=self._policy_save).grid(row=0, column=4, padx=4)
        ttk.Label(box, text="Anyone nearby: helpers join in one click. Everyone on this network can see the name and label "
                  "you announce, and joined helpers see the inputs of the jobs they run, so don't send private data to "
                  "helpers you don't trust. 'Require pairing code' keeps the Part 1 closed group.",
                  wraplength=950, foreground="#555").grid(row=1, column=0, columnspan=5, sticky="w", pady=(6, 0))
        self.policy_status = ttk.Label(box, text="")
        self.policy_status.grid(row=2, column=0, columnspan=5, sticky="w")
        from .gui_common import make_tree

        ttk.Label(tab, text="Waiting to join, and Verify numbers", font=("Segoe UI", 10, "bold")).pack(anchor="w", pady=(6, 2))
        self.requests_tree = make_tree(tab, [("What", 140), ("Name", 200), ("ID", 110), ("Number", 120)], height=3)
        ttk.Label(tab, text="Devices", font=("Segoe UI", 10, "bold")).pack(anchor="w", pady=(6, 2))
        self.devices_tree = make_tree(tab, [("Name", 200), ("ID", 110), ("Trusted", 70), ("Blocked", 70),
                                            ("Workers", 260), ("Last seen", 140)], height=8)
        buttons = ttk.Frame(tab)
        buttons.pack(fill="x", pady=4)
        for label, action in (("Accept", "accept"), ("Decline", "decline"), ("Trust", "trust"), ("Untrust", "untrust"),
                              ("Block", "block"), ("Unblock", "unblock")):
            ttk.Button(buttons, text=label, command=lambda a=action: self._device_action(a)).pack(side="left", padx=2)
        ttk.Label(buttons, text="Select a row first. Trust only after the helper shows the same Verify number.",
                  foreground="#666").pack(side="left", padx=8)
        self._devices_busy = False
        self.root.after(2000, self._devices_tick)

    def _selected_device(self) -> str | None:
        for tree in (self.requests_tree, self.devices_tree):
            selection = tree.selection()
            if selection:
                return selection[0].split(":", 1)[-1]
        return None

    def _policy_save(self) -> None:
        from .client import join_policy

        admin = self._need_admin()
        if not admin:
            return
        policy = next(k for k, v in POLICY_LABELS.items() if v == self.policy_var.get())
        announce, label = self.announce_var.get(), self.announce_label_var.get().strip()[:60]
        self.bg.run(lambda: join_policy(admin, join_policy=policy, announce=announce, announce_label=label),
                    self._policy_render, lambda e: messagebox.showerror("Join policy", str(e)))

    def _policy_render(self, data: dict[str, Any]) -> None:
        self.policy_status.config(text=f"Now: {POLICY_LABELS.get(data['join_policy'], data['join_policy'])}, "
                                  f"{'announcing' if data.get('announcing') else 'not announcing'}"
                                  f"{', closing' if data.get('closing') else ''}.")

    def _device_action(self, action: str) -> None:
        from .client import device_action

        admin, device = self._need_admin(), self._selected_device()
        if not admin or not device:
            return
        if action == "block" and not messagebox.askyesno("Block", "Block this device? Its helpers are removed at once."):
            return
        self.bg.run(lambda: device_action(admin, device, action), lambda _r: self._devices_refresh(),
                    lambda e: messagebox.showerror("Devices", str(e)))

    def _devices_tick(self) -> None:
        self.root.after(3000, self._devices_tick)
        self._devices_refresh()

    def _devices_refresh(self) -> None:
        from .client import join_policy, requester_devices, requester_identity

        admin = self.admin
        if not admin or self._devices_busy:
            return
        self._devices_busy = True

        def work() -> tuple[Any, Any, Any]:
            return requester_identity(admin), join_policy(admin), requester_devices(admin)

        def fail(_exc: Exception) -> None:
            self._devices_busy = False

        self.bg.run(work, self._devices_render, fail)

    def _devices_render(self, data: tuple[dict[str, Any], dict[str, Any], dict[str, Any]]) -> None:
        import time

        from .gui_common import fill_tree

        self._devices_busy = False
        ident, policy, devices = data
        self.identity_label.config(text=f"This requester: {ident['name']}   ·   ID {ident['short_id']}")
        self._policy_render(policy)
        rows = [(f"r:{r['device_id']}", ["Wants to join", r["name"], r["short_id"], ""]) for r in devices["requests"]]
        rows += [(f"v:{v['device_id']}", ["Verify", v["worker"], v["short_id"], f"{v['code'][:3]} {v['code'][3:]}"])
                 for v in devices["verifications"]]
        fill_tree(self.requests_tree, rows)
        fill_tree(self.devices_tree, [(d["device_id"], [
            d["name"] or "(not seen yet)", d["short_id"], "yes" if d["trusted"] else "", "yes" if d["blocked"] else "",
            ", ".join(d["workers"]), time.strftime("%Y-%m-%d %H:%M", time.localtime(d["last_seen"])) if d["last_seen"] else ""])
            for d in devices["devices"]])


def helper_threads(cfg: WorkerConfig) -> int:
    return max(1, min(cfg.max_cpu_threads, cfg.max_concurrent_jobs))


def sandbox_ready() -> bool:
    """Automatic joining waits for the Compute agent's sandbox (PLAN §17); until then joining is one click."""
    try:
        from . import sandbox  # type: ignore[attr-defined]

        return bool(sandbox.available()[0])
    except Exception:  # noqa: BLE001 - missing module or a failed check both mean "not ready"
        return False


class WorkerNearbyControls:
    """Helper side: the nearby requesters list, one-click Join, Trust/Block, Verify and Leave."""

    def _build_nearby(self) -> None:
        from . import identity
        from .discovery import Listener, NearbyList
        from .gui_common import make_tree

        self.ident = identity.current()
        self.peers = identity.PeerBook()
        box = ttk.LabelFrame(getattr(self, "body", self.root), text="Nearby requesters (join without a code)", padding=6)
        box.pack(fill="x", padx=8, pady=4)
        ttk.Label(box, text=f"This computer: {self.ident.name}  ·  ID {self.ident.short_id}", foreground="#444").pack(anchor="w")
        self.nearby_tree = make_tree(box, [("Name", 170), ("ID", 95), ("Badge", 70), ("Group", 60), ("Helpers", 60),
                                           ("Needs", 60), ("Link", 85), ("Address", 140), ("Label", 140)], height=4)
        buttons = ttk.Frame(box)
        buttons.pack(fill="x", pady=3)
        for label, fn in (("Join", self._nearby_join), ("Search", self._nearby_search), ("Trust", self._nearby_trust),
                          ("Block", self._nearby_block), ("Verify", self._nearby_verify), ("Leave", self._nearby_leave)):
            ttk.Button(buttons, text=label, command=fn).pack(side="left", padx=2)
        self.auto_direct = tk.BooleanVar(value=False)
        ttk.Checkbutton(buttons, text="Join nearby Wi-Fi Direct groups automatically (not available yet)",
                        variable=self.auto_direct, state="disabled").pack(side="left", padx=10)
        self.nearby_status = ttk.Label(box, text="", wraplength=840, foreground="#555")
        self.nearby_status.pack(anchor="w")
        self._nearby_busy = False
        self.nearby = NearbyList(on_new=lambda e: self.bg.call_soon(self._nearby_new, e))
        from .notify import ConsentRequests, Notifier

        self.consent = ConsentRequests()
        self._consent_windows: dict[str, tk.Toplevel] = {}
        self.notifier = Notifier(lambda action, device: self.bg.call_soon(self._notify_action, action, device),
                                 window=lambda kind, text, device: self.bg.call_soon(self._notify_window, kind, text, device))
        self.listener: Listener | None = None
        if self.cfg.discovery:
            self.listener = Listener(self.ident.device_id, self.nearby, port=self.cfg.discovery_port)
            try:
                self.listener.start()
            except OSError as exc:
                self.nearby_status.config(text=f"Discovery unavailable ({exc}). Use Search or Find controller.")
        self.root.after(1000, self._nearby_tick)

    def _nearby_stop(self) -> None:
        if getattr(self, "listener", None):
            self.listener.stop()
            self.listener = None

    def _nearby_tick(self) -> None:
        from .gui_common import fill_tree

        self.root.after(1000, self._nearby_tick)
        for request in self.consent.expire():
            self._consent_closed(request.device_id)
            self._write(f"No answer for {request.name} ({request.short_id}) in 60 s, so it was declined.")
        rows = []
        for e in self.nearby.entries():
            badge = "Blocked" if self.peers.is_blocked(e.device_id) else "Trusted" if self.peers.is_trusted(e.device_id) else ""
            rows.append((e.device_id, [e.name, e.short_id, badge, "open" if e.open else "closed", e.helpers, e.threads,
                                       e.link, f"{e.host}:{e.port}", e.job_label]))
        fill_tree(self.nearby_tree, rows)
        if not self._nearby_busy:
            hint = self.listener.status()["hint"] if self.listener else "Discovery is off in settings."
            if not rows:
                hint += "  Discovery blocked? Use Search, or Find controller above."
            self.nearby_status.config(text=hint)

    def _nearby_selected(self) -> Any:
        selection = self.nearby_tree.selection()
        entry = self.nearby.get(selection[0]) if selection else None
        if entry is None:
            messagebox.showinfo("Nearby", "Select a requester in the list first.")
        return entry

    def _nearby_new(self, entry: Any) -> None:
        """A requester appeared. Joining by itself (or asking first) needs a working sandbox (PLAN §16.2)."""
        if self.agent or not entry.open or self.peers.is_blocked(entry.device_id) or not sandbox_ready():
            return
        mode = self.cfg.join_mode
        if mode == "auto" or (mode == "trusted" and self.peers.is_trusted(entry.device_id)):
            self._join_entry(entry, ask=False, announce=True)
        elif mode == "ask" and self.consent.add(entry.device_id, entry.name, entry.short_id, helper_threads(self.cfg)):
            request = next(r for r in self.consent.pending() if r.device_id == entry.device_id)
            self.notifier.ask(request)

    # ---- owner messages: Accept / Decline / Stop (from a toast, the tray or the window)
    def _notify_action(self, action: str, device_id: str) -> None:
        if action == "stop":
            agent = self.agent
            if agent and device_id in (agent.cfg.requester_device_id, agent.cfg.controller_fingerprint):
                self.stop()
            return
        request = self.consent.answer(action, device_id)
        self._consent_closed(device_id)
        if request is None or action != "accept":
            if request is not None:
                self._write(f"Declined {request.name} ({request.short_id}).")
            return
        entry = self.nearby.get(device_id)
        if entry is None or not entry.open:
            self._write(f"{request.name} is no longer asking for help nearby.")
            return
        self._join_entry(entry, ask=False, announce=True)

    def _notify_window(self, kind: str, text: str, device_id: str) -> None:
        self._write(text)
        if kind != "ask" or device_id in self._consent_windows:
            return
        win = tk.Toplevel(self.root)
        win.title("Network Connector")
        win.attributes("-topmost", True)
        ttk.Label(win, text=text, wraplength=380, padding=12).pack()
        row = ttk.Frame(win, padding=(12, 0, 12, 12))
        row.pack()
        ttk.Button(row, text="Accept", command=lambda: self._notify_action("accept", device_id)).pack(side="left", padx=4)
        ttk.Button(row, text="Decline", command=lambda: self._notify_action("decline", device_id)).pack(side="left", padx=4)
        win.protocol("WM_DELETE_WINDOW", lambda: self._notify_action("decline", device_id))
        self._consent_windows[device_id] = win

    def _consent_closed(self, device_id: str) -> None:
        self.notifier.withdraw(device_id)
        win = self._consent_windows.pop(device_id, None)
        if win is not None:
            try:
                win.destroy()
            except tk.TclError:
                pass

    def _nearby_search(self) -> None:
        from .discovery import probe_requesters
        from .wifidirect import DirectLinkError, candidate_controllers

        if self._nearby_busy:
            return
        typed = self.addr.get().strip().partition(":")[0]
        port = self.cfg.controller_port
        self._nearby_busy = True
        self.nearby_status.config(text="Asking the direct-link gateway and the typed address (outbound only) …")

        def work() -> list[Any]:
            try:
                hosts = candidate_controllers()
            except (DirectLinkError, OSError, RuntimeError):
                hosts = []
            return probe_requesters(([typed] if typed else []) + hosts, port, own_device_id=self.ident.device_id)

        def done(found: list[Any]) -> None:
            self._nearby_busy = False
            for entry in found:
                self.nearby.update(entry)
            self.nearby_status.config(text=f"Search found {len(found)} open requester(s).")

        def fail(exc: Exception) -> None:
            self._nearby_busy = False
            self.nearby_status.config(text=f"Search failed: {exc}")

        self.bg.run(work, done, fail)

    def _nearby_join(self) -> None:
        entry = self._nearby_selected()
        if entry:
            self._join_entry(entry, ask=True)

    def _join_entry(self, entry: Any, ask: bool, announce: bool = False) -> None:
        from .client import JoinPending, join_requester, joined_dir

        if self.agent:
            messagebox.showinfo("Join", "Stop the current work first.")
            return
        if self.peers.is_blocked(entry.device_id):
            messagebox.showinfo("Join", "You blocked this requester. Unblock it with Trust, or 'nc block --undo'.")
            return
        if not entry.open:
            messagebox.showinfo("Join", "This requester is not accepting helpers right now.")
            return
        if ask and not messagebox.askyesno("Join", f"Help {entry.name} ({entry.short_id})?\n\nIt can send jobs to this "
                                           "computer within the limits below. Its jobs run only inside the Wasm sandbox "
                                           "(no network, no files). You can Stop or Leave at any time."):
            return
        cfg = self._apply_limits(load_config(WorkerConfig, self.dir / "worker.json"))
        cfg.token = cfg.worker_id = ""
        ident = self.ident
        self._nearby_busy = True
        self.nearby_status.config(text=f"Joining {entry.name} … (waits up to 2 minutes if its owner must accept)")

        def work() -> Any:
            return join_requester(entry.host, entry.port, entry.device_id, ident, None, cfg, wait_s=120)

        def done(new_cfg: WorkerConfig) -> None:
            self._nearby_busy = False
            self._use_dir(joined_dir(entry.device_id))
            self._write(f"Joined {entry.name} ({entry.short_id}) without a code. Its certificate is pinned.")
            self._launch(new_cfg)
            if announce:
                from .notify import using_text

                self.notifier.info(using_text(entry.name, entry.short_id, helper_threads(new_cfg)), entry.device_id)

        def fail(exc: Exception) -> None:
            self._nearby_busy = False
            text = exc.message if isinstance(exc, JoinPending) else str(exc)
            self.nearby_status.config(text=f"Join did not finish: {text}")

        self.bg.run(work, done, fail)

    def _nearby_trust(self) -> None:
        entry = self._nearby_selected()
        if entry:
            trusted = not self.peers.is_trusted(entry.device_id)
            if trusted and not messagebox.askyesno("Trust", f"Trust {entry.name} ({entry.short_id})? Use Verify first "
                                                   "to be sure it is the computer you think it is."):
                return
            self.peers.set(entry.device_id, trusted=trusted, blocked=False if trusted else None, name=entry.name)

    def _nearby_block(self) -> None:
        entry = self._nearby_selected()
        if not entry:
            return
        blocked = not self.peers.is_blocked(entry.device_id)
        self.peers.set(entry.device_id, blocked=blocked, name=entry.name)
        if blocked and self.agent and self.cfg.controller_fingerprint == entry.device_id:
            self._nearby_leave()

    def _nearby_verify(self) -> None:
        from .client import verify_requester
        from .worker import NotPaired, load_worker_config

        try:
            cfg = load_worker_config(self.dir)
        except NotPaired:
            messagebox.showinfo("Verify", "Join a requester first.")
            return
        ident, directory = self.ident, self.dir

        def done(code: str) -> None:
            name = cfg.requester_name or cfg.controller_host
            if messagebox.askyesno("Verify", f"{code[:3]} {code[3:]}\n\nDoes {name} show the same number in its Devices "
                                   "tab?\n\nYes trusts it. If the numbers differ, click No: you may not be connected to "
                                   "the computer you think."):
                self.peers.set(cfg.controller_fingerprint, trusted=True, name=cfg.requester_name or None)

        self.bg.run(lambda: verify_requester(cfg, directory, ident), done, lambda e: messagebox.showerror("Verify", str(e)))

    def _nearby_leave(self) -> None:
        from .client import leave_requester
        from .paths import worker_dir
        from .worker import NotPaired, load_worker_config

        agent = self.agent
        group = agent.cfg.group_mode if agent else load_config(WorkerConfig, self.dir / "worker.json").group_mode
        if group != "open" and not messagebox.askyesno(
                "Leave", "This is a paired (closed) group. Leaving removes this computer there, and you need a new "
                "pairing code to come back. Leave anyway?"):
            return
        if agent:
            self._write("Leaving: unfinished jobs go back to the requester …")
            self.bg.run(agent.leave, lambda _r: (self._stopped(), self._use_dir(worker_dir("worker"))))
            return
        try:
            cfg = load_worker_config(self.dir)
        except NotPaired:
            return
        directory = self.dir

        def finish(_r: Any = None) -> None:
            self._use_dir(worker_dir("worker"))
            self._write("Left the requester's group.")

        self.bg.run(lambda: leave_requester(cfg, directory), finish, lambda _e: finish())


class WorkerDirectControls:
    def _build_worker_direct(self) -> None:
        box = ttk.LabelFrame(getattr(self, "body", self.root), text="0. Connect (router-less Wi-Fi)", padding=6)
        box.pack(fill="x", padx=8, pady=4)
        self.join_ssid = tk.StringVar()
        self.join_password = tk.StringVar()
        self._join_busy = False
        ttk.Label(box, text="SSID").grid(row=0, column=0)
        ttk.Entry(box, textvariable=self.join_ssid, width=25).grid(row=0, column=1, padx=4)
        ttk.Label(box, text="Password").grid(row=0, column=2)
        ttk.Entry(box, textvariable=self.join_password, show="*", width=24).grid(row=0, column=3, padx=4)
        ttk.Button(box, text="Join", command=self._join_direct).grid(row=0, column=4, padx=3)
        ttk.Button(box, text="Find controller", command=self._find_controller).grid(row=0, column=5, padx=3)
        self.link_status = ttk.Label(box, text="Join the controller's SSID, then find its address.", wraplength=840)
        self.link_status.grid(row=1, column=0, columnspan=6, sticky="w", pady=3)
        self.radio_status = ttk.Label(box, text="", foreground="#b00020")
        self.radio_button = ttk.Button(box, text="Turn on", command=self._radio_on)
        self._radio_busy = False
        self.root.after(1500, self._radio_tick)

    # ---- Wi-Fi radio: shown only when it is off; turned on only by this click, never off
    def _radio_tick(self) -> None:
        from .radio import wifi_state

        self.root.after(15000, self._radio_tick)
        if not self._radio_busy:
            self._radio_busy = True
            self.bg.run(wifi_state, self._radio_render, lambda _e: self._radio_render("unknown"))

    def _radio_render(self, state: str) -> None:
        self._radio_busy = False
        if state == "off":
            self.radio_status.config(text="Wi-Fi is off — nearby requesters can't be found.")
            self.radio_status.grid(row=2, column=0, columnspan=4, sticky="w")
            self.radio_button.grid(row=2, column=4, sticky="w")
        else:
            self.radio_status.grid_remove()
            self.radio_button.grid_remove()

    def _radio_on(self) -> None:
        from .radio import turn_wifi_on

        self.radio_button.config(state="disabled")
        self.radio_status.config(text="Asking Windows to turn Wi-Fi on …")

        def done(text: str) -> None:
            self.radio_button.config(state="normal")
            self._radio_render("on")
            self._write(text)

        def fail(exc: Exception) -> None:
            self.radio_button.config(state="normal")
            self.radio_status.config(text=str(exc))

        self.bg.run(turn_wifi_on, done, fail)

    def _join_direct(self) -> None:
        if self._join_busy:
            return
        from .wifidirect import join
        ssid, password = self.join_ssid.get().strip(), self.join_password.get()
        self._join_busy = True
        self.link_status.config(text="Joining Wi-Fi...")
        def done(info: Any) -> None:
            self._join_busy = False
            self.join_password.set("")
            self.link_status.config(text=f"Connected: {info.name}, {info.ip}. Find the controller next.")
        def fail(exc: Exception) -> None:
            self._join_busy = False
            self.link_status.config(text=str(exc))
        self.bg.run(lambda: join(ssid, password), done, fail)

    def _find_controller(self) -> None:
        if self._join_busy:
            return
        cfg = load_config(WorkerConfig, self.cfg_path)
        # Typed addresses are additional candidates; identity still comes from the certificate.
        host, _, port = self.addr.get().strip().partition(":")
        if host:
            cfg.controller_host = host
            try:
                cfg.controller_port = int(port or cfg.controller_port)
            except ValueError:
                self.link_status.config(text="Enter a numeric controller port.")
                return
        self._join_busy = True
        self.link_status.config(text="Finding controller and checking its certificate...")
        def done(data: dict[str, str]) -> None:
            self._join_busy = False
            self.addr.set(f"{data['host']}:{cfg.controller_port}")
            self.link_status.config(text=f"{data['host']} — certificate {data['fingerprint']}\nCompare this fingerprint with the controller before pairing.")
            if cfg.token:
                cfg.controller_host = data["host"]
                cfg.controller_fingerprint = data["fingerprint"]
                cfg.known_hosts = list(dict.fromkeys([data["host"]] + cfg.known_hosts))[:4]
                save_config(cfg, self.cfg_path)
        def fail(exc: Exception) -> None:
            self._join_busy = False
            self.link_status.config(text=str(exc))
        self.bg.run(lambda: discover_controller(cfg, self.dir), done, fail)
