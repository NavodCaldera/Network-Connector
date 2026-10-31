"""The whole exchange: one PC listens, the other connects, each side sends one Hello line.

Plain TCP, no encryption: use it only on the private Wi-Fi Direct link between the two PCs.
"""
from __future__ import annotations

import socket

PORT = 8765
MAX_LINE = 200


def greeting() -> str:
    return f"Hello from {socket.gethostname()}"


def _read_line(conn: socket.socket) -> str:
    data = b""
    while b"\n" not in data and len(data) < MAX_LINE:
        chunk = conn.recv(MAX_LINE)
        if not chunk:
            break
        data += chunk
    return data.split(b"\n", 1)[0].decode("utf-8", "replace").strip()[:MAX_LINE]


def serve(host: str = "0.0.0.0", port: int = PORT, once: bool = False, on_message=print, stop=None) -> None:
    """Answer every Hello with our own Hello. Stops after one exchange when `once` is set, or when `stop`
    (a threading.Event) is set."""
    with socket.create_server((host, port)) as server:
        server.settimeout(0.5)  # wake up regularly to notice `stop`
        on_message(f"Waiting for Hello on {host}:{port} ...")
        while not (stop and stop.is_set()):
            try:
                conn, (peer, _) = server.accept()
            except socket.timeout:
                continue
            with conn:
                conn.settimeout(10)
                try:
                    on_message(f"{peer} says: {_read_line(conn)}")
                    conn.sendall((greeting() + "\n").encode())
                except OSError as exc:
                    on_message(f"{peer}: {exc}")
            if once:
                return


def send(host: str, port: int = PORT, timeout: float = 10) -> str:
    """Send our Hello to `host` and return its answer."""
    with socket.create_connection((host, port), timeout=timeout) as conn:
        conn.sendall((greeting() + "\n").encode())
        return _read_line(conn)
