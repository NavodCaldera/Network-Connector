"""nc host | nc join | nc send | nc listen: Wi-Fi Direct between two PCs, then a Hello each way."""
from __future__ import annotations

import argparse
import time

from . import firewall, hello
from .wifidirect import (DirectGroup, DirectLinkError, candidate_controllers, default_ssid, derived_passphrase, join, leave,
                         scan_direct_groups)


def cmd_host(args: argparse.Namespace) -> int:
    issues = firewall.problems()
    if issues:
        print(f"Firewall: {'; '.join(issues)}. Click Yes on the Windows prompt to fix it...")
        print("Firewall fixed." if firewall.fix() else "Firewall not fixed; the other PC's Hello may time out.")
    group = DirectGroup()
    ssid = args.ssid or default_ssid()
    info = group.start(ssid, args.passphrase or (None if args.ssid else derived_passphrase(ssid)))
    print(f"Wi-Fi Direct group started: {info.ssid}")
    if args.ssid or args.passphrase:
        print(f"  Password: {info.passphrase}\nOn the other PC run:  nc join {info.ssid} <Password>")
    else:
        print("On the other PC run:  nc join")
    for _ in range(20):  # the adapter's address appears a few seconds after the group starts
        if group.status().ip:
            break
        time.sleep(0.5)
    print(f"This PC's address on the group: {group.status().ip or 'unknown yet'}")
    try:
        hello.serve(port=args.port, once=args.once)
    except KeyboardInterrupt:
        pass
    finally:
        group.stop()
        print("Group stopped.")
    return 0


def cmd_join(args: argparse.Namespace) -> int:
    if not args.ssid:
        print("Looking for a DIRECT-NC group nearby...")
        for _ in range(6):  # Windows refreshes its network list every few seconds
            found = scan_direct_groups()
            if found:
                break
            time.sleep(5)
        else:
            print("No DIRECT-NC group found. Is 'nc host' running on the other PC?")
            return 1
        if len(found) > 1:
            print(f"Several groups found: {', '.join(found)}. Run: nc join <name>")
            return 1
        args.ssid = found[0]
    print(f"Joining {args.ssid}...")
    iface = join(args.ssid, args.passphrase or derived_passphrase(args.ssid))
    print(f"Joined {args.ssid}. This PC's address: {iface.ip}")
    try:
        for host in candidate_controllers(iface):
            try:
                print(f"{host} replied: {hello.send(host, args.port)}")
                return 0
            except OSError as exc:
                print(f"{host}: {exc}")
        print("No reply from the group owner. Is 'nc host' running there, and is the port allowed in the firewall?")
        return 1
    finally:
        if args.leave:
            leave(args.ssid)


def cmd_send(args: argparse.Namespace) -> int:
    print(f"{args.host} replied: {hello.send(args.host, args.port)}")
    return 0


def cmd_listen(args: argparse.Namespace) -> int:
    try:
        hello.serve(args.bind, args.port, once=args.once)
    except KeyboardInterrupt:
        pass
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="nc", description="Say Hello between two PCs over Wi-Fi Direct.")
    sub = parser.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("host", help="start a Wi-Fi Direct group and answer Hellos")
    p.add_argument("--ssid", help="DIRECT-... name (blank: DIRECT-NC-XXXX from this PC's name)")
    p.add_argument("--passphrase", help="8-63 characters (blank: built in, no typing needed)")
    p.add_argument("--once", action="store_true", help="stop after one Hello")
    p.set_defaults(fn=cmd_host)
    p = sub.add_parser("join", help="find the group, join it and send Hello to its owner")
    p.add_argument("ssid", nargs="?", help="blank: find the DIRECT-NC group nearby")
    p.add_argument("passphrase", nargs="?", help="blank: the built-in password")
    p.add_argument("--leave", action="store_true", help="remove the Wi-Fi profile afterwards")
    p.set_defaults(fn=cmd_join)
    p = sub.add_parser("send", help="send Hello to an address")
    p.add_argument("host")
    p.set_defaults(fn=cmd_send)
    p = sub.add_parser("listen", help="answer Hellos without starting a group")
    p.add_argument("--bind", default="0.0.0.0")
    p.add_argument("--once", action="store_true")
    p.set_defaults(fn=cmd_listen)
    for p in sub.choices.values():
        p.add_argument("--port", type=int, default=hello.PORT)
    args = parser.parse_args(argv)
    try:
        return args.fn(args)
    except (DirectLinkError, OSError) as exc:
        print(f"Error: {exc}")
        return 1
