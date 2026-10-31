"""One window for both PCs: "Host" on one, "Join" on the other, and every Hello shown as it passes."""
from __future__ import annotations

import queue
import threading
import time
import tkinter as tk
from tkinter import scrolledtext, ttk

from . import firewall, hello
from .wifidirect import (DirectGroup, DirectLinkError, candidate_controllers, default_ssid, derived_passphrase, join,
                         scan_direct_groups)


class App:
    def __init__(self, root: tk.Tk) -> None:
        self.root = root
        self.events: queue.Queue = queue.Queue()
        self.group: DirectGroup | None = None
        self.peer: str | None = None  # the host's address once joined
        root.title("Network Connector - Hello")
        root.geometry("620x420")
        top = ttk.Frame(root, padding=10)
        top.pack(fill="x")
        self.host_btn = ttk.Button(top, text="Host (start Wi-Fi Direct)", command=self.on_host)
        self.host_btn.pack(side="left")
        self.join_btn = ttk.Button(top, text="Join the other PC", command=self.on_join)
        self.join_btn.pack(side="left", padx=8)
        self.send_btn = ttk.Button(top, text="Send Hello", command=self.on_send, state="disabled")
        self.send_btn.pack(side="left")
        self.status = ttk.Label(root, text="Click Host on one PC, then Join on the other.", padding=(10, 0))
        self.status.pack(fill="x")
        self.log = scrolledtext.ScrolledText(root, state="disabled", font=("Consolas", 10), height=18)
        self.log.pack(fill="both", expand=True, padx=10, pady=10)
        self.log.tag_config("in", foreground="#0a7d2c")
        self.log.tag_config("out", foreground="#1a5fb4")
        self.log.tag_config("err", foreground="#c01c28")
        root.protocol("WM_DELETE_WINDOW", self.on_close)
        root.after(100, self.pump)

    # ---- thread-safe output: workers put events, the Tk thread draws them
    def say(self, text: str, tag: str = "") -> None:
        self.events.put(("log", text, tag))

    def set_status(self, text: str) -> None:
        self.events.put(("status", text, ""))

    def pump(self) -> None:
        while not self.events.empty():
            kind, text, tag = self.events.get()
            if kind == "status":
                self.status.config(text=text)
            elif kind == "enable_send":
                self.send_btn.config(state="normal")
            else:
                self.log.config(state="normal")
                self.log.insert("end", f"{time.strftime('%H:%M:%S')}  {text}\n", tag)
                self.log.see("end")
                self.log.config(state="disabled")
        self.root.after(100, self.pump)

    def run(self, work) -> None:
        self.host_btn.config(state="disabled")
        self.join_btn.config(state="disabled")

        def wrapped() -> None:
            try:
                work()
            except (DirectLinkError, OSError) as exc:
                self.say(f"Error: {exc}", "err")
                self.set_status("Something went wrong - see below.")
        threading.Thread(target=wrapped, daemon=True).start()

    # ---- host
    def on_host(self) -> None:
        self.run(self.host)

    def host(self) -> None:
        issues = firewall.problems()
        if issues:
            self.say(f"Firewall: {'; '.join(issues)}.")
            self.set_status("Click Yes on the Windows prompt to let the other PC in...")
            self.say("Firewall fixed." if firewall.fix() else "Firewall not fixed; the other PC's Hello may not arrive.",
                     "" if not firewall.problems() else "err")
        self.set_status("Starting Wi-Fi Direct...")
        ssid = default_ssid()
        for attempt in range(3):  # Windows may abort while a previous group is still being released
            try:
                self.group = DirectGroup()
                self.group.start(ssid, derived_passphrase(ssid))
                break
            except DirectLinkError as exc:
                if attempt == 2 or "aborted" not in str(exc):
                    raise
                self.say("Windows was not ready; trying again in 10 s...")
                time.sleep(10)
        self.say(f"Wi-Fi Direct group started: {ssid}")
        self.set_status(f"Hosting {ssid}. On the other PC open this app and click 'Join the other PC'.")

        def note(msg: str) -> None:
            if " says: " in msg:
                self.say("<- " + msg, "in")
                self.say(f"-> replied: {hello.greeting()}", "out")
                self.set_status("Hello received and answered.")
        threading.Thread(target=hello.serve, kwargs={"on_message": note}, daemon=True).start()

    # ---- join
    def on_join(self) -> None:
        self.run(self.join)

    def join(self) -> None:
        self.set_status("Looking for the other PC's DIRECT-NC group...")
        for _ in range(6):
            found = scan_direct_groups()
            if found:
                break
            time.sleep(5)
        else:
            raise DirectLinkError("No DIRECT-NC group found. Click Host on the other PC first.")
        ssid = found[0]
        self.say(f"Found {ssid}, joining...")
        iface = join(ssid, derived_passphrase(ssid))
        self.say(f"Joined. This PC's address: {iface.ip}")
        for host in candidate_controllers(iface):
            self.peer = host
            if self.send():
                self.events.put(("enable_send", "", ""))
                self.set_status(f"Connected to {host}. Click Send Hello to say it again.")
                return
        raise DirectLinkError("Joined, but the host did not answer. Is the app hosting on the other PC?")

    def on_send(self) -> None:
        threading.Thread(target=self.send, daemon=True).start()

    def send(self) -> bool:
        self.say(f"-> {hello.greeting()}", "out")
        try:
            self.say(f"<- {self.peer} replied: {hello.send(self.peer)}", "in")
            return True
        except OSError as exc:
            self.say(f"No answer from {self.peer}: {exc}", "err")
            return False

    def on_close(self) -> None:
        if self.group:
            try:
                self.group.stop()
            except DirectLinkError:
                pass
        self.root.destroy()


def main() -> None:
    root = tk.Tk()
    App(root)
    root.mainloop()


if __name__ == "__main__":
    main()
