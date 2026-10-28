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
