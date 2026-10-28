"""Phase 6 spike: Windows toast notifications from unpackaged desktop Python (WinRT). Throwaway code.

Run with the SPIKE venv:
  spikes\\0.3-network\\.venv\\Scripts\\python.exe spikes\\0.3-network\\toast_spike.py [--wait 20]

Variants:
  A  create_toast_notifier() with no AppUserModelID (expected to fail for unpackaged apps)
  B  an unregistered custom AUMID
  C  the same AUMID registered per-user under HKCU\\Software\\Classes\\AppUserModelId\\<id>
     (DisplayName only; the key is DELETED again at the end of the run)
Variant C also carries Accept/Decline buttons and reports Activated/Dismissed/Failed events while
the process is alive. A person must click a button to test activation.
"""
from __future__ import annotations

import argparse
import threading
import time
import winreg

AUMID = "NetworkConnector.Spike"
REG = rf"Software\Classes\AppUserModelId\{AUMID}"


def xml_doc(text: str, buttons: bool):
    from winrt.windows.data.xml.dom import XmlDocument

    actions = ""
    if buttons:
        actions = ('<actions><action content="Accept" arguments="accept" activationType="foreground"/>'
                   '<action content="Decline" arguments="decline" activationType="foreground"/></actions>')
    doc = XmlDocument()
    doc.load_xml(f'<toast scenario="reminder"><visual><binding template="ToastGeneric"><text>Network Connector</text>'
                 f'<text>{text}</text></binding></visual>{actions}</toast>' if buttons else
                 f'<toast><visual><binding template="ToastGeneric"><text>Network Connector</text>'
                 f'<text>{text}</text></binding></visual></toast>')
    return doc


def show(label: str, notifier, text: str, buttons: bool, wait: float, events: list) -> None:
    from winrt.windows.ui.notifications import ToastNotification, NotificationSetting, ToastDismissalReason

    try:
        print(f"[{label}] notifier.setting = {NotificationSetting(notifier.setting).name}")
    except Exception as exc:
        print(f"[{label}] notifier.setting failed: {type(exc).__name__}: {exc}")
    toast = ToastNotification(xml_doc(text, buttons))
    toast.tag, toast.group = label, "spike"
    done = threading.Event()

    def activated(sender, args):
        try:
            from winrt.windows.ui.notifications import ToastActivatedEventArgs
            arg = ToastActivatedEventArgs._from(args).arguments
        except Exception as exc:
            arg = f"(could not read arguments: {type(exc).__name__})"
        events.append(f"[{label}] Activated arguments={arg!r} thread={threading.current_thread().name}")
        print(events[-1], flush=True)
        done.set()

    def dismissed(sender, args):
        events.append(f"[{label}] Dismissed reason={ToastDismissalReason(args.reason).name}")
        print(events[-1], flush=True)
        done.set()

    def failed(sender, args):
        events.append(f"[{label}] Failed error=0x{args.error_code.value & 0xFFFFFFFF:08X}")
        print(events[-1], flush=True)
        done.set()

    toast.add_activated(activated)
    toast.add_dismissed(dismissed)
    toast.add_failed(failed)
    try:
        notifier.show(toast)
        print(f"[{label}] show() returned", flush=True)
    except Exception as exc:
        print(f"[{label}] show() failed: {type(exc).__name__}: {exc}")
        return
    if not done.wait(wait):
        print(f"[{label}] no event within {wait:.0f} s")
    from winrt.windows.ui.notifications import ToastNotificationManager
    try:
        items = list(ToastNotificationManager.history.get_history_with_id(AUMID))
        print(f"[{label}] notification history for {AUMID}: {len(items)} item(s) tags={[i.tag for i in items]}")
    except Exception as exc:
        print(f"[{label}] history read failed: {type(exc).__name__}: {exc}")


def main() -> None:
    from winrt.windows.ui.notifications import ToastNotificationManager

    ap = argparse.ArgumentParser()
    ap.add_argument("--wait", type=float, default=12)
    args = ap.parse_args()
    events: list[str] = []
    print(time.strftime("%Y-%m-%d %H:%M:%S"))

    try:
        ToastNotificationManager.create_toast_notifier()
        print("[A] default notifier created (unexpected for unpackaged)")
    except Exception as exc:
        print(f"[A] create_toast_notifier() without AUMID failed: {type(exc).__name__}: {exc}")

    show("B", ToastNotificationManager.create_toast_notifier_with_id(AUMID), "Spike B: unregistered AppUserModelID", False, args.wait, events)

    key = winreg.CreateKey(winreg.HKEY_CURRENT_USER, REG)
    winreg.SetValueEx(key, "DisplayName", 0, winreg.REG_SZ, "Network Connector (spike)")
    winreg.CloseKey(key)
    print(f"[C] registered HKCU\\{REG}")
    try:
        show("C", ToastNotificationManager.create_toast_notifier_with_id(AUMID),
             "Spike C: Navod's i7 asks to use 4 CPU threads", True, args.wait, events)
    finally:
        try:
            ToastNotificationManager.history.clear_with_id(AUMID)
            print("[C] history cleared")
        except Exception as exc:
            print(f"[C] history clear failed: {type(exc).__name__}: {exc}")
        winreg.DeleteKey(winreg.HKEY_CURRENT_USER, REG)
        print(f"[C] deleted HKCU\\{REG}")
    print("events:", events or "none")


if __name__ == "__main__":
    main()
