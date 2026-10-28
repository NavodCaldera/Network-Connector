"""Single-window entry point for requesting, helping, or both, with a tray icon (PLAN §13)."""
from __future__ import annotations

import argparse
import subprocess
import sys
import threading
import tkinter as tk
from dataclasses import dataclass
from tkinter import messagebox, ttk

from . import autostart
from .config import load_config, save_config, setup_logging
from .paths import data_home

ROLES = ("ask", "give", "both")


@dataclass
class AppConfig:
    role: str = ""  # ask | give | both
    autostart: bool = True  # start in the tray at login (per user); the owner can turn it off
    autostart_asked: bool = False  # set once the first-start choice has written (or skipped) the login entry
    close_to_tray: bool = True


class RoleFrame(ttk.Frame):
    """Lets the existing standalone windows render inside a tab."""

    def title(self, _value: str) -> None:
        pass

    def geometry(self, _value: str) -> None:
        pass

    def minsize(self, _width: int, _height: int) -> None:
        pass

    def protocol(self, _name: str, _callback: object) -> None:
        pass


class NetworkConnectorApp:
    def __init__(self, root: tk.Tk, background: bool = False, tray: bool = True) -> None:
        self.root = root
        self.path = data_home() / "app.json"
        self.cfg = load_config(AppConfig, self.path)
        self.controller = None
        self.worker = None
        self._quitting = False
        root.title("Network Connector")
        root.geometry("1120x780")
        root.minsize(900, 600)
        root.protocol("WM_DELETE_WINDOW", self.on_close)
        self.header = ttk.Frame(root, padding=10)
        self.header.pack(fill="x")
        ttk.Label(self.header, text="Network Connector", font=("Segoe UI", 16, "bold")).pack(side="left")
        ttk.Button(self.header, text="Change roles", command=self.choose_roles).pack(side="right")
        self.autostart_var = tk.BooleanVar(value=self.cfg.autostart)
        if autostart.supported():
            ttk.Checkbutton(self.header, text="Start at login", variable=self.autostart_var,
                            command=self._autostart_changed).pack(side="right", padx=10)
        self.content = ttk.Frame(root)
        self.content.pack(fill="both", expand=True)
        self.tray = None
        if tray:
            from .tray import Tray

            self.tray = Tray(self.tray_status, self.tray_paused)
            if not self.tray.start():
                self.tray = None
        self.root.after(100, self._poll_tray)
        if self.cfg.role in ROLES:
            self._show_roles()
        else:
            self.choose_roles()
        if background and self.tray and self.cfg.role in ROLES:
            root.withdraw()

    # ---- roles
    def choose_roles(self) -> None:
        if self.controller or self.worker:
            if not messagebox.askyesno("Change roles", "Network Connector restarts to change roles. Running work stops: "
                                       "joined groups are left and unfinished jobs go back to the queue. Continue?"):
                return
            self.cfg.role = ""
            save_config(self.cfg, self.path)
            self._shutdown(restart=True)
            return
        for child in self.content.winfo_children():
            child.destroy()
        panel = ttk.Frame(self.content, padding=30)
        panel.pack(expand=True)
        ttk.Label(panel, text="What would you like this computer to do?",
                  font=("Segoe UI", 16, "bold")).pack(pady=(0, 20))
        for label, role in (("Give performance", "give"), ("Ask for performance", "ask"), ("Both", "both")):
            ttk.Button(panel, text=label, width=32, command=lambda r=role: self.set_role(r)).pack(pady=8)
        ttk.Label(panel, text="Your device identity, trusted and blocked lists and settings are kept when you change roles.",
                  foreground="#666").pack(pady=(16, 0))

    def set_role(self, role: str) -> None:
        if role not in ROLES:
            raise ValueError("unknown role")
        self.cfg.role = role
        if not self.cfg.autostart_asked:
            self.cfg.autostart_asked = True
            self._apply_autostart(self.cfg.autostart)
        save_config(self.cfg, self.path)
        self._show_roles()
        if role in ("give", "both"):
            self.root.after(500, self.offer_sandbox_setup)

    def _show_roles(self) -> None:
        for child in self.content.winfo_children():
            child.destroy()
        notebook = ttk.Notebook(self.content)
        notebook.pack(fill="both", expand=True)
        if self.cfg.role in {"ask", "both"}:
            from .gui import ControllerApp

            frame = RoleFrame(notebook)
            notebook.add(frame, text="Ask for performance")
            self.controller = ControllerApp(frame)
        if self.cfg.role in {"give", "both"}:
            from .worker_gui import WorkerApp

            frame = RoleFrame(notebook)
            notebook.add(frame, text="Give performance")
            self.worker = WorkerApp(frame)
            self.worker.notifier.tray = self.tray

    def offer_sandbox_setup(self) -> None:
        """Helping needs the Wasm sandbox. Its setup needs no admin rights: one read grant on its own folder."""
        if not self.worker:
            return
        from . import sandbox

        def done(result: tuple[bool, str]) -> None:
            ok, reason = result
            if ok or "not set up" not in reason:
                if not ok:
                    self.worker._write(f"Sandbox unavailable: {reason}")
                return
            if not messagebox.askyesno("Set up the sandbox", "Other people's work runs only inside the Wasm sandbox. "
                                       "Set it up now? It copies the sandbox runtime to its own folder and gives app "
                                       "containers read access to that folder only. No administrator rights needed."):
                return
            self.worker.bg.run(lambda: (sandbox.setup_runtime(), sandbox.available(refresh=True))[1],
                               lambda r: self.worker._write("Sandbox ready." if r[0] else f"Sandbox still unavailable: {r[1]}"),
                               lambda e: messagebox.showerror("Sandbox", str(e)))

        self.worker.bg.run(sandbox.available, done)

    # ---- autostart
    def _apply_autostart(self, on: bool) -> None:
        try:
            autostart.set_enabled(on)
        except OSError as exc:
            messagebox.showerror("Start at login", f"Could not change the login entry: {exc}")

    def _autostart_changed(self) -> None:
        self.cfg.autostart = bool(self.autostart_var.get())
        self.cfg.autostart_asked = True
        self._apply_autostart(self.cfg.autostart)
        save_config(self.cfg, self.path)

    # ---- tray (its menu runs on pystray's thread; everything here runs on tk's)
    def tray_status(self) -> str:
        agent = self.worker.agent if self.worker else None
        if agent:
            state = getattr(agent, "state", "")
            return f"Helping: {state} · {len(agent.running)} running"
        runtime = self.controller.runtime if self.controller else None
        return "Asking for performance" if runtime else "Not helping"

    def tray_paused(self) -> bool:
        agent = self.worker.agent if self.worker else None
        return bool(agent and agent.local_paused)

    def _poll_tray(self) -> None:
        if self._quitting:
            return
        self.root.after(100, self._poll_tray)
        if self.tray:
            self.tray.poll(self.tray_action)
            agent = self.worker.agent if self.worker else None
            self.tray.set_state("stopped" if not agent else "paused" if agent.local_paused else
                                "busy" if agent.running else "ready")

    def tray_action(self, action: str) -> None:
        if action == "open":
            self.root.deiconify()
            self.root.lift()
        elif action == "pause" and self.worker and self.worker.agent:
            self.worker.toggle_pause()
        elif action == "stop" and self.worker:
            self.worker.stop()  # sandbox.stop_all() at once, then leave an open group
        elif action == "quit":
            self.quit()

    # ---- closing
    def on_close(self) -> None:
        if self.tray and self.cfg.close_to_tray and self.cfg.role in ROLES:
            self.root.withdraw()  # keep helping in the background; Quit is in the tray menu
            return
        self.quit()

    def quit(self) -> None:
        if (self.controller and self.controller.runtime) or (self.worker and self.worker.agent):
            self.root.deiconify()
            if not messagebox.askyesno("Exit", "Stop requesting and helping, then exit?"):
                return
        self._shutdown(restart=False)

    def _shutdown(self, restart: bool) -> None:
        self._quitting = True
        if self.worker:
            self.worker._nearby_stop()
            if self.worker.agent:
                self.worker.agent.stop_helping()

        def shutdown() -> None:
            if self.worker and self.worker.agent:
                self.worker.agent.stop()
            if self.controller:
                runtime = self.controller.runtime
                if runtime and hasattr(runtime, "wind_down"):
                    runtime.wind_down()  # announce "closing" so joined helpers leave cleanly
                self.controller._stop_all()
            if self.tray:
                self.tray.stop()
            if restart:
                subprocess.Popen([sys.executable, "-m", "network_connector.app"], close_fds=True)
            self.root.after(0, self.root.destroy)

        threading.Thread(target=shutdown, daemon=True).start()


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(prog="network-connector")
    parser.add_argument("--background", action="store_true", help="start in the tray (used at login)")
    args = parser.parse_args(argv)
    setup_logging("app", console=False)
    root = tk.Tk()
    NetworkConnectorApp(root, background=args.background)
    root.mainloop()


if __name__ == "__main__":
    main()
