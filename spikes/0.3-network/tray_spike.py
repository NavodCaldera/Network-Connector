"""Phase 6 spike: pystray tray icon from desktop Python, next to a tkinter main loop. Throwaway code.

Run with the SPIKE venv (shows a tray icon for a few seconds, plus one balloon message):
  spikes\\0.3-network\\.venv\\Scripts\\python.exe spikes\\0.3-network\\tray_spike.py [--seconds 6] [--interactive]

--interactive keeps the icon for 60 s so a person can right-click it and try the menu;
every click is printed with the thread it ran on.
"""
from __future__ import annotations

import argparse
import threading
import time
import tkinter as tk

from PIL import Image, ImageDraw


def make_image(color: str) -> Image.Image:
    img = Image.new("RGBA", (64, 64), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.ellipse((4, 4, 60, 60), fill=color)
    d.text((22, 22), "NC", fill="white")
    return img


def main() -> None:
    import pystray

    ap = argparse.ArgumentParser()
    ap.add_argument("--seconds", type=float, default=6)
    ap.add_argument("--interactive", action="store_true")
    args = ap.parse_args()
    seconds = 60 if args.interactive else args.seconds
    log: list[str] = []

    def clicked(name: str):
        def handler(icon, item):
            log.append(f"{time.strftime('%H:%M:%S')} menu {name!r} on thread {threading.current_thread().name}")
            print(log[-1], flush=True)
            if name == "Quit":
                root.after(0, root.quit)  # hand back to the tk thread; never touch tk from pystray's thread
        return handler

    paused = {"v": False}

    def toggle_pause(icon, item):
        paused["v"] = not paused["v"]
        icon.icon = make_image("#888888" if paused["v"] else "#2a7")
        clicked("Pause")(icon, item)

    menu = pystray.Menu(
        pystray.MenuItem(lambda item: "Helping: paused" if paused["v"] else "Helping: ready", None, enabled=False),
        pystray.MenuItem("Pause", toggle_pause, checked=lambda item: paused["v"]),
        pystray.MenuItem("Stop helping", clicked("Stop helping")),
        pystray.MenuItem("Open", clicked("Open"), default=True),
        pystray.MenuItem("Quit", clicked("Quit")),
    )
    root = tk.Tk()
    root.withdraw()
    t0 = time.perf_counter()
    icon = pystray.Icon("nc-spike", make_image("#2a7"), "Network Connector (spike)", menu)
    print("pystray backend:", type(icon).__module__)
    icon.run_detached()  # Windows backend: message loop on its own thread, tk keeps the main thread
    print(f"run_detached returned after {(time.perf_counter() - t0) * 1000:.0f} ms; threads: "
          f"{[t.name for t in threading.enumerate()]}")

    def check_visible() -> None:
        print(f"icon.visible={icon.visible} HAS_NOTIFICATION={icon.HAS_NOTIFICATION} HAS_MENU={icon.HAS_MENU} "
              f"HAS_DEFAULT_ACTION={icon.HAS_DEFAULT_ACTION}", flush=True)
        try:
            icon.notify("Navod's i7 is using 4 of your CPU threads (spike test)", "Network Connector")
            print("icon.notify() returned without error", flush=True)
        except Exception as exc:
            print(f"icon.notify() failed: {type(exc).__name__}: {exc}", flush=True)

    root.after(1500, check_visible)
    root.after(int(seconds * 1000), root.quit)
    root.mainloop()
    t1 = time.perf_counter()
    icon.remove_notification()
    icon.stop()
    print(f"icon.stop() took {(time.perf_counter() - t1) * 1000:.0f} ms; tk main loop ended normally")
    print("menu clicks:", log or "none")
    root.destroy()


if __name__ == "__main__":
    main()
