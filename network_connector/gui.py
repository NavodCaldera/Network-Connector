"""One window for both PCs: "Host" on one, "Join" on the other, and every Hello shown as it passes."""
from __future__ import annotations

import queue
import threading
import time
import tkinter as tk
from tkinter import scrolledtext, ttk

from . import firewall, hello, p2p
from .wifidirect import (DirectGroup, DirectLinkError, candidate_controllers, default_ssid, derived_passphrase, join,
                         leave, scan_direct_groups)


class App:
    def __init__(self, root: tk.Tk) -> None:
        self.root = root
        self.events: queue.Queue = queue.Queue()
        self.group: DirectGroup | None = None
        self.peer: str | None = None  # the host's address once joined
        self.native_host: p2p.Host | None = None
        self.joiner: p2p.Joiner | None = None
        self.classic = tk.BooleanVar(value=False)
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
        self.stop_btn = ttk.Button(top, text="Stop", command=self.on_stop, state="disabled")
        self.stop_btn.pack(side="left", padx=8)
        ttk.Checkbutton(top, text="Classic mode (joiner leaves its Wi-Fi)", variable=self.classic).pack(side="left")
        self.retry_btn = ttk.Button(top, text="Retry", command=self.on_retry)  # packed only after a failure
        self.last_work = None
        self.stopping = threading.Event()  # set by Stop: ends the Hello server and a running search
        self.joined_ssid: str | None = None
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
            elif kind == "choose":
                self.ask_choice(*text)
            elif kind == "enable_send":
                self.send_btn.config(state="normal")
            elif kind == "failed":  # let the user try again instead of restarting the app
                self.host_btn.config(state="normal")
                self.join_btn.config(state="normal")
                self.stop_btn.config(state="disabled")
                self.retry_btn.pack(side="left", padx=8)
            elif kind == "stopped":
                self.host_btn.config(state="normal")
                self.join_btn.config(state="normal")
                self.send_btn.config(state="disabled")
                self.stop_btn.config(state="disabled")
            else:
                self.log.config(state="normal")
                self.log.insert("end", f"{time.strftime('%H:%M:%S')}  {text}\n", tag)
                self.log.see("end")
                self.log.config(state="disabled")
        self.root.after(100, self.pump)

    def run(self, work) -> None:
        self.last_work = work
        self.stopping.clear()
        self.host_btn.config(state="disabled")
        self.join_btn.config(state="disabled")
        self.stop_btn.config(state="normal")
        self.retry_btn.pack_forget()

        def wrapped() -> None:
            try:
                work()
            except Exception as exc:  # noqa: BLE001 - WinRT raises assorted types; show them all
                if self.stopping.is_set():
                    return  # Stop interrupted it; on_stop reports the outcome
                self.say(f"Error: {exc or type(exc).__name__}", "err")
                self.set_status("Something went wrong - see below, then click Retry.")
                self.events.put(("failed", "", ""))
        threading.Thread(target=wrapped, daemon=True).start()

    def on_retry(self) -> None:
        self.say("Retrying...")
        self.run(self.last_work)

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
        if not self.classic.get():
            self.host_native()
        else:
            self.host_classic()
        self.serve()

    def host_native(self) -> None:
        def on_peer(name: str, local: str, remote: str) -> None:
            self.say(f"{name} connected over Wi-Fi Direct (this PC {local}, {name} {remote}).", "in")
            self.set_status(f"{name} connected. Waiting for its Hello...")

        self.native_host = p2p.Host(on_peer, lambda msg: self.say(msg, "err"))
        self.native_host.start()
        self.say("Wi-Fi Direct is on. Both PCs keep their normal Wi-Fi and internet.")
        self.set_status("Hosting. On the other PC open this app and click 'Join the other PC'. "
                        "If Windows asks to allow a device, click Allow.")

    def host_classic(self) -> None:
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

    def serve(self) -> None:
        def note(msg: str) -> None:
            if " says: " in msg:
                self.say("<- " + msg, "in")
                self.say(f"-> replied: {hello.greeting()}", "out")
                self.set_status("Hello received and answered.")
        threading.Thread(target=hello.serve, kwargs={"on_message": note, "stop": self.stopping}, daemon=True).start()

    # ---- join
    def on_join(self) -> None:
        self.run(self.join)

    def join(self) -> None:
        if self.classic.get():
            self.join_classic()
        else:
            self.join_native()

    def join_native(self) -> None:
        self.set_status("Looking for the other PC over Wi-Fi Direct...")
        if self.joiner:
            self.joiner.close()
        self.joiner = p2p.Joiner()
        peers = self.joiner.find()
        if not peers:
            raise p2p.DirectLinkError("No Wi-Fi Direct PC found. Click Host on the other PC first.")
        peer = peers[0] if len(peers) == 1 else self.choose(peers)
        if peer is None:
            raise p2p.DirectLinkError("No PC chosen.")
        self.say(f"Found {peer.name}, connecting (click Allow if Windows asks)...")
        local, remote = self.joiner.connect(peer)
        self.say(f"Connected over Wi-Fi Direct. This PC {local}, {peer.name} {remote}. Your normal Wi-Fi stays on.")
        self.peer = remote
        for _ in range(5):  # the host may need a moment to see the connection
            if self.send():
                self.events.put(("enable_send", "", ""))
                self.set_status(f"Connected to {peer.name}. Click Send Hello to say it again.")
                return
            time.sleep(2)
        raise p2p.DirectLinkError("Connected, but the host did not answer. Is the app hosting on the other PC?")

    def choose(self, peers: list) -> p2p.Peer | None:
        """Ask on the Tk thread which PC to join; blocks this worker thread until answered."""
        answer: list = []
        done = threading.Event()
        self.events.put(("choose", (peers, answer, done), ""))
        done.wait()
        return answer[0] if answer else None

    def ask_choice(self, peers: list, answer: list, done: threading.Event) -> None:
        win = tk.Toplevel(self.root)
        win.title("Which PC?")
        ttk.Label(win, text="Several Wi-Fi Direct devices were found. Pick the other PC:", padding=10).pack()
        box = tk.Listbox(win, height=min(len(peers), 10), width=40)
        for peer in peers:
            box.insert("end", peer.name)
        box.selection_set(0)
        box.pack(padx=10)

        def ok() -> None:
            if box.curselection():
                answer.append(peers[box.curselection()[0]])
            win.destroy()
        ttk.Button(win, text="Connect", command=ok).pack(pady=10)
        win.bind("<Destroy>", lambda _e: done.set())

    def join_classic(self) -> None:
        self.set_status("Looking for the other PC's DIRECT-NC group...")
        for _ in range(6):
            found = scan_direct_groups()
            if found or self.stopping.is_set():
                break
            self.stopping.wait(5)
        else:
            raise DirectLinkError("No DIRECT-NC group found. Click Host on the other PC first.")
        if self.stopping.is_set():
            return
        ssid = found[0]
        self.say(f"Found {ssid}, joining...")
        iface = join(ssid, derived_passphrase(ssid))
        self.joined_ssid = ssid
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

    # ---- stop: end hosting, leave the joined group, or cancel a search
    def on_stop(self) -> None:
        self.stopping.set()
        self.stop_btn.config(state="disabled")
        self.send_btn.config(state="disabled")
        self.set_status("Stopping...")
        threading.Thread(target=self.stop, daemon=True).start()

    def stop(self) -> None:
        try:
            if self.group:
                self.group.stop()
                self.group = None
                self.say("Wi-Fi Direct group stopped.")
            if self.native_host:
                self.native_host.stop()
                self.native_host = None
                self.say("Wi-Fi Direct stopped.")
            if self.joiner:
                self.joiner.close()
                self.joiner = None
                self.say("Disconnected from the other PC.")
            if self.joined_ssid:
                leave(self.joined_ssid)
                self.say(f"Left {self.joined_ssid}.")
                self.joined_ssid = None
            self.peer = None
            self.set_status("Stopped. Click Host or Join to start again.")
        except DirectLinkError as exc:
            self.say(f"Error while stopping: {exc}", "err")
            self.set_status("Stop did not finish - see below.")
        self.events.put(("stopped", "", ""))

    def on_close(self) -> None:
        self.stopping.set()
        for link in (self.native_host, self.joiner):
            try:
                if link:
                    link.stop() if isinstance(link, p2p.Host) else link.close()
            except Exception:  # noqa: BLE001 - closing anyway
                pass
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
