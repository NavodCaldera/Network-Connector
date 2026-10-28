"""Small WAT programs for the sandbox tests, compiled with wasmtime.wat2wasm (no Rust needed)."""
from __future__ import annotations

from functools import lru_cache

import wasmtime

_WASI = '"wasi_snapshot_preview1"'

ECHO = f"""
(module
  (import {_WASI} "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import {_WASI} "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_start") (local $n i32)
    (loop $l
      (i32.store (i32.const 0) (i32.const 1024))
      (i32.store (i32.const 4) (i32.const 4096))
      (br_if 1 (call $fd_read (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 8)))
      (local.set $n (i32.load (i32.const 8)))
      (if (i32.eqz (local.get $n)) (then (return)))
      (i32.store (i32.const 4) (local.get $n))
      (drop (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 8)))
      (br $l))))
"""

EXIT_7 = f"""
(module
  (import {_WASI} "proc_exit" (func $exit (param i32)))
  (memory (export "memory") 1)
  (func (export "_start") (call $exit (i32.const 7))))
"""

LOOP = """
(module
  (memory (export "memory") 1)
  (func (export "_start") (local $x i64)
    (loop $l
      (local.set $x (i64.add (i64.mul (local.get $x) (i64.const 6364136223846793005)) (i64.const 1442695040888963407)))
      (br $l))))
"""

# Grows memory 1 MiB at a time until refused, touches every page, then exits 42.
MEMBOMB = f"""
(module
  (import {_WASI} "proc_exit" (func $proc_exit (param i32)))
  (memory (export "memory") 1)
  (func (export "_start") (local $p i32)
    (block $d (loop $l
      (br_if $d (i32.eq (memory.grow (i32.const 16)) (i32.const -1)))
      (br $l)))
    (block $d (loop $l
      (br_if $d (i32.ge_u (local.get $p) (i32.shl (memory.size) (i32.const 16))))
      (i32.store8 (local.get $p) (i32.const 1))
      (local.set $p (i32.add (local.get $p) (i32.const 4096)))
      (br $l)))
    (call $proc_exit (i32.const 42))))
"""

RECURSE = """
(module
  (memory (export "memory") 1)
  (func $r (param i64) (result i64)
    (i64.add (call $r (i64.add (local.get 0) (i64.const 1))) (i64.const 1)))
  (func (export "_start") (drop (call $r (i64.const 0)))))
"""

# poll_oneoff: sleep 10 s. Fuel and epoch can't interrupt a host call; only the helper's kill can.
SLEEP = f"""
(module
  (import {_WASI} "poll_oneoff" (func $poll (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_start")
    (i64.store (i32.const 0) (i64.const 7))
    (i32.store8 (i32.const 8) (i32.const 0))
    (i32.store (i32.const 16) (i32.const 1))
    (i64.store (i32.const 24) (i64.const 10000000000))
    (drop (call $poll (i32.const 0) (i32.const 64) (i32.const 1) (i32.const 128)))))
"""


def _flood(fd: int) -> str:
    return f"""
(module
  (import {_WASI} "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 2)
  (func (export "_start") (local $p i32)
    (block $d (loop $l
      (br_if $d (i32.ge_u (local.get $p) (i32.const 65536)))
      (i32.store8 offset=1024 (local.get $p) (i32.const 65))
      (local.set $p (i32.add (local.get $p) (i32.const 1)))
      (br $l)))
    (i32.store (i32.const 0) (i32.const 1024))
    (i32.store (i32.const 4) (i32.const 65536))
    (loop $w
      (drop (call $fd_write (i32.const {fd}) (i32.const 0) (i32.const 1) (i32.const 8)))
      (br $w))))
"""


HUGE_OUT, HUGE_ERR = _flood(1), _flood(2)

# Uses only allowed calls and prints what it can see:
# "P=<fd_prestat_get(3) errno> E=<env count> A=<argc> " (WASI errno 8 = EBADF: nothing pre-opened).
LOOK_AROUND = f"""
(module
  (import {_WASI} "fd_prestat_get" (func $prestat (param i32 i32) (result i32)))
  (import {_WASI} "environ_sizes_get" (func $env_sizes (param i32 i32) (result i32)))
  (import {_WASI} "args_sizes_get" (func $args_sizes (param i32 i32) (result i32)))
  (import {_WASI} "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (global $out (mut i32) (i32.const 2048))
  (func $emit (param $tag i32) (param $v i32)
    (i32.store8 (global.get $out) (local.get $tag))
    (i32.store8 offset=1 (global.get $out) (i32.const 61))
    (i32.store8 offset=2 (global.get $out) (i32.add (i32.const 48) (i32.div_u (local.get $v) (i32.const 10))))
    (i32.store8 offset=3 (global.get $out) (i32.add (i32.const 48) (i32.rem_u (local.get $v) (i32.const 10))))
    (i32.store8 offset=4 (global.get $out) (i32.const 32))
    (global.set $out (i32.add (global.get $out) (i32.const 5))))
  (func (export "_start")
    (call $emit (i32.const 80) (call $prestat (i32.const 3) (i32.const 200)))
    (drop (call $env_sizes (i32.const 400) (i32.const 404)))
    (call $emit (i32.const 69) (i32.load (i32.const 400)))
    (drop (call $args_sizes (i32.const 408) (i32.const 412)))
    (call $emit (i32.const 65) (i32.load (i32.const 408)))
    (i32.store (i32.const 0) (i32.const 2048))
    (i32.store (i32.const 4) (i32.sub (global.get $out) (i32.const 2048)))
    (drop (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 8)))))
"""

PATH_OPEN = f"""
(module
  (import {_WASI} "path_open" (func $path_open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1000) "C:/Users")
  (func (export "_start")
    (drop (call $path_open (i32.const 3) (i32.const 0) (i32.const 1000) (i32.const 8)
                           (i32.const 0) (i64.const 0) (i64.const 0) (i32.const 0) (i32.const 300)))))
"""

READDIR = f"""
(module
  (import {_WASI} "fd_readdir" (func $readdir (param i32 i32 i32 i64 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_start") (drop (call $readdir (i32.const 3) (i32.const 0) (i32.const 64) (i64.const 0) (i32.const 100)))))
"""

SOCK_ACCEPT = f"""
(module
  (import {_WASI} "sock_accept" (func $sock_accept (param i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_start") (drop (call $sock_accept (i32.const 3) (i32.const 0) (i32.const 16)))))
"""

SOCK_OPEN = f"""
(module
  (import {_WASI} "sock_open" (func $sock_open (param i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_start") (drop (call $sock_open (i32.const 1) (i32.const 1) (i32.const 0)))))
"""

HOST_SYSTEM = """
(module
  (import "env" "system" (func $system (param i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_start") (drop (call $system (i32.const 0)))))
"""

NO_START = """(module (memory (export "memory") 1) (func (export "main")))"""


@lru_cache(maxsize=None)
def wasm(text: str) -> bytes:
    return wasmtime.wat2wasm(text)
