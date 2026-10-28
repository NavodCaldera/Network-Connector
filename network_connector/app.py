"""Single-window entry point for requesting, helping, or both."""
from __future__ import annotations

import threading
import tkinter as tk
from dataclasses import dataclass
from tkinter import messagebox, ttk

from .config import load_config, save_config, setup_logging
from .paths import data_home


@dataclass
class AppConfig:
    role: str = ""  # ask | give | both


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
    def __init__(self, root: tk.Tk) -> None:
        self.root = root
        self.path = data_home() / "app.json"
        self.cfg = load_config(AppConfig, self.path)
        self.controller = None
        self.worker = None
        root.title("Network Connector")
        root.geometry("1120x780")
        root.minsize(900, 600)
        root.protocol("WM_DELETE_WINDOW", self.on_close)
        self.header = ttk.Frame(root, padding=10)
        self.header.pack(fill="x")
        ttk.Label(self.header, text="Network Connector", font=("Segoe UI", 16, "bold")).pack(side="left")
        ttk.Button(self.header, text="Change roles", command=self.choose_roles).pack(side="right")
        self.content = ttk.Frame(root)
        self.content.pack(fill="both", expand=True)
        if self.cfg.role in {"ask", "give", "both"}:
            self._show_roles()
        else:
            self.choose_roles()

    def choose_roles(self) -> None:
        if self.controller or self.worker:
            messagebox.showinfo("Change roles", "Close Network Connector before changing roles.")
            return
        for child in self.content.winfo_children():
            child.destroy()
        panel = ttk.Frame(self.content, padding=30)
        panel.pack(expand=True)
        ttk.Label(panel, text="What would you like this computer to do?",
                  font=("Segoe UI", 16, "bold")).pack(pady=(0, 20))
        for label, role in (("Give performance", "give"), ("Ask for performance", "ask"), ("Both", "both")):
            ttk.Button(panel, text=label, width=32, command=lambda r=role: self.set_role(r)).pack(pady=8)

    def set_role(self, role: str) -> None:
        if role not in {"ask", "give", "both"}:
            raise ValueError("unknown role")
        self.cfg.role = role
        save_config(self.cfg, self.path)
        self._show_roles()

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

    def on_close(self) -> None:
        if (self.controller and self.controller.runtime) or (self.worker and self.worker.agent):
            if not messagebox.askyesno("Exit", "Stop requesting and helping, then exit?"):
                return
        if self.worker:
            self.worker._nearby_stop()
            if self.worker.agent:
                self.worker.agent.stop_helping()

        def shutdown() -> None:
            if self.worker and self.worker.agent:
                self.worker.agent.stop()
            if self.controller:
                self.controller._stop_all()
            self.root.after(0, self.root.destroy)

        threading.Thread(target=shutdown, daemon=True).start()


def main() -> None:
    setup_logging("app", console=False)
    root = tk.Tk()
    NetworkConnectorApp(root)
    root.mainloop()


if __name__ == "__main__":
    main()
