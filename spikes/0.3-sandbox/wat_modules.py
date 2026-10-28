"""Throwaway: WAT sources for the 0.3 sandbox spike, compiled to build/*.wasm.

Run: .venv\\Scripts\\python.exe wat_modules.py
"""
from __future__ import annotations

import hashlib
import struct
from pathlib import Path

import wasmtime

HERE = Path(__file__).resolve().parent
BUILD = HERE / "build"

K = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]
H0 = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19]


def _data(blob: bytes) -> str:
    return "".join(f"\\{b:02x}" for b in blob)


# stdin: 8 bytes little-endian round count + 32 byte seed. stdout: hex(sha256^count(seed)) + "\n".
# Memory: 0 K[64], 256 W[64], 512 M[8], 600 iovec, 608 n, 1024 input, 2048 output.
def sha_chain_wat() -> str:
    regs = "abcdefgh"
    init = "\n".join(f"    (local.set ${r} (i32.const {h:#x}))" for r, h in zip(regs, H0))
    fold = "\n".join(
        f"    (i32.store (i32.const {512 + 4 * k}) (i32.add (i32.const {h:#x}) (local.get ${r})))"
        for k, (r, h) in enumerate(zip(regs, H0)))
    zeros = "\n".join(f"    (i32.store (i32.const {256 + 4 * w}) (i32.const 0))" for w in range(9, 15))
    return f"""
(module
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (param i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "{_data(struct.pack('<64I', *K))}")
  (func $be32 (param $p i32) (result i32)
    (i32.or (i32.or (i32.shl (i32.load8_u (local.get $p)) (i32.const 24))
                    (i32.shl (i32.load8_u offset=1 (local.get $p)) (i32.const 16)))
            (i32.or (i32.shl (i32.load8_u offset=2 (local.get $p)) (i32.const 8))
                    (i32.load8_u offset=3 (local.get $p)))))
  (func $compress
    (local $i i32) (local $p i32) (local $x i32) (local $y i32) (local $t1 i32) (local $t2 i32)
    (local $a i32) (local $b i32) (local $c i32) (local $d i32)
    (local $e i32) (local $f i32) (local $g i32) (local $h i32)
    (local.set $i (i32.const 0))
    (block $d0 (loop $l0
      (br_if $d0 (i32.ge_u (local.get $i) (i32.const 32)))
      (i32.store offset=256 (local.get $i) (i32.load offset=512 (local.get $i)))
      (local.set $i (i32.add (local.get $i) (i32.const 4)))
      (br $l0)))
    (i32.store (i32.const 288) (i32.const 0x80000000))
{zeros}
    (i32.store (i32.const 316) (i32.const 256))
    (local.set $p (i32.const 320))
    (block $d1 (loop $l1
      (br_if $d1 (i32.ge_u (local.get $p) (i32.const 512)))
      (local.set $x (i32.load (i32.sub (local.get $p) (i32.const 60))))
      (local.set $y (i32.load (i32.sub (local.get $p) (i32.const 8))))
      (i32.store (local.get $p)
        (i32.add
          (i32.add (i32.load (i32.sub (local.get $p) (i32.const 64)))
                   (i32.xor (i32.xor (i32.rotr (local.get $x) (i32.const 7)) (i32.rotr (local.get $x) (i32.const 18)))
                            (i32.shr_u (local.get $x) (i32.const 3))))
          (i32.add (i32.load (i32.sub (local.get $p) (i32.const 28)))
                   (i32.xor (i32.xor (i32.rotr (local.get $y) (i32.const 17)) (i32.rotr (local.get $y) (i32.const 19)))
                            (i32.shr_u (local.get $y) (i32.const 10))))))
      (local.set $p (i32.add (local.get $p) (i32.const 4)))
      (br $l1)))
{init}
    (local.set $i (i32.const 0))
    (block $d2 (loop $l2
      (br_if $d2 (i32.ge_u (local.get $i) (i32.const 256)))
      (local.set $t1
        (i32.add
          (i32.add (local.get $h)
                   (i32.xor (i32.xor (i32.rotr (local.get $e) (i32.const 6)) (i32.rotr (local.get $e) (i32.const 11)))
                            (i32.rotr (local.get $e) (i32.const 25))))
          (i32.add
            (i32.xor (i32.and (local.get $e) (local.get $f))
                     (i32.and (i32.xor (local.get $e) (i32.const -1)) (local.get $g)))
            (i32.add (i32.load (local.get $i)) (i32.load offset=256 (local.get $i))))))
      (local.set $t2
        (i32.add
          (i32.xor (i32.xor (i32.rotr (local.get $a) (i32.const 2)) (i32.rotr (local.get $a) (i32.const 13)))
                   (i32.rotr (local.get $a) (i32.const 22)))
          (i32.xor (i32.xor (i32.and (local.get $a) (local.get $b)) (i32.and (local.get $a) (local.get $c)))
                   (i32.and (local.get $b) (local.get $c)))))
      (local.set $h (local.get $g))
      (local.set $g (local.get $f))
      (local.set $f (local.get $e))
      (local.set $e (i32.add (local.get $d) (local.get $t1)))
      (local.set $d (local.get $c))
      (local.set $c (local.get $b))
      (local.set $b (local.get $a))
      (local.set $a (i32.add (local.get $t1) (local.get $t2)))
      (local.set $i (i32.add (local.get $i) (i32.const 4)))
      (br $l2)))
{fold})
  (func (export "_start")
    (local $total i32) (local $n i32) (local $count i64) (local $k i32) (local $j i32) (local $w i32) (local $nib i32)
    (block $eof (loop $rd
      (br_if $eof (i32.ge_u (local.get $total) (i32.const 40)))
      (i32.store (i32.const 600) (i32.add (i32.const 1024) (local.get $total)))
      (i32.store (i32.const 604) (i32.sub (i32.const 40) (local.get $total)))
      (if (call $fd_read (i32.const 0) (i32.const 600) (i32.const 1) (i32.const 608))
        (then (call $proc_exit (i32.const 2))))
      (local.set $n (i32.load (i32.const 608)))
      (br_if $eof (i32.eqz (local.get $n)))
      (local.set $total (i32.add (local.get $total) (local.get $n)))
      (br $rd)))
    (if (i32.lt_u (local.get $total) (i32.const 40)) (then (call $proc_exit (i32.const 2))))
    (local.set $count (i64.load (i32.const 1024)))
    (local.set $k (i32.const 0))
    (block $d (loop $l
      (br_if $d (i32.ge_u (local.get $k) (i32.const 32)))
      (i32.store offset=512 (local.get $k) (call $be32 (i32.add (i32.const 1032) (local.get $k))))
      (local.set $k (i32.add (local.get $k) (i32.const 4)))
      (br $l)))
    (block $d (loop $l
      (br_if $d (i64.eqz (local.get $count)))
      (call $compress)
      (local.set $count (i64.sub (local.get $count) (i64.const 1)))
      (br $l)))
    (local.set $k (i32.const 0))
    (block $d (loop $l
      (br_if $d (i32.ge_u (local.get $k) (i32.const 8)))
      (local.set $w (i32.load offset=512 (i32.shl (local.get $k) (i32.const 2))))
      (local.set $j (i32.const 0))
      (block $dj (loop $lj
        (br_if $dj (i32.ge_u (local.get $j) (i32.const 8)))
        (local.set $nib (i32.and (i32.shr_u (local.get $w) (i32.sub (i32.const 28) (i32.shl (local.get $j) (i32.const 2))))
                                 (i32.const 15)))
        (i32.store8 offset=2048 (i32.add (i32.shl (local.get $k) (i32.const 3)) (local.get $j))
          (select (i32.add (i32.const 48) (local.get $nib)) (i32.add (i32.const 87) (local.get $nib))
                  (i32.lt_u (local.get $nib) (i32.const 10))))
        (local.set $j (i32.add (local.get $j) (i32.const 1)))
        (br $lj)))
      (local.set $k (i32.add (local.get $k) (i32.const 1)))
      (br $l)))
    (i32.store8 (i32.const 2112) (i32.const 10))
    (i32.store (i32.const 600) (i32.const 2048))
    (i32.store (i32.const 604) (i32.const 65))
    (drop (call $fd_write (i32.const 1) (i32.const 600) (i32.const 1) (i32.const 608))))
)
"""


ECHO = """
(module
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
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

# Pure compute, never ends.
LOOP = """
(module
  (memory (export "memory") 1)
  (func (export "_start") (local $x i64)
    (loop $l
      (local.set $x (i64.add (i64.mul (local.get $x) (i64.const 6364136223846793005)) (i64.const 1442695040888963407)))
      (br $l))))
"""

# Counts N iterations of the LOOP body (N from stdin, 8 bytes LE) so fuel/s and speed can be calibrated.
COUNTED = """
(module
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_start") (local $x i64) (local $n i64)
    (i32.store (i32.const 0) (i32.const 64))
    (i32.store (i32.const 4) (i32.const 8))
    (drop (call $fd_read (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 8)))
    (local.set $n (i64.load (i32.const 64)))
    (block $d (loop $l
      (br_if $d (i64.eqz (local.get $n)))
      (local.set $x (i64.add (i64.mul (local.get $x) (i64.const 6364136223846793005)) (i64.const 1442695040888963407)))
      (local.set $n (i64.sub (local.get $n) (i64.const 1)))
      (br $l)))
    (i64.store (i32.const 64) (local.get $x))
    (i32.store (i32.const 4) (i32.const 8))
    (drop (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 8)))))
"""

# Grows memory 1 MiB at a time until refused, then touches every page, then exits 42.
MEMBOMB = """
(module
  (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (param i32)))
  (memory (export "memory") 1)
  (func (export "_start") (local $p i32)
    (block $d (loop $l
      (br_if $d (i32.eq (memory.grow (i32.const 16)) (i32.const -1)))
      (br $l)))
    (local.set $p (i32.const 0))
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

# poll_oneoff: sleep 10 s on the monotonic clock. Fuel and epoch can't interrupt a host call.
SLEEP = """
(module
  (import "wasi_snapshot_preview1" "poll_oneoff" (func $poll (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_start")
    (i64.store (i32.const 0) (i64.const 7))
    (i32.store8 (i32.const 8) (i32.const 0))
    (i32.store (i32.const 16) (i32.const 1))
    (i64.store (i32.const 24) (i64.const 10000000000))
    (i64.store (i32.const 32) (i64.const 0))
    (i32.store16 (i32.const 40) (i32.const 0))
    (drop (call $poll (i32.const 0) (i32.const 64) (i32.const 1) (i32.const 128)))))
"""

# Tries WASI file, env and socket calls and prints each errno as "name=NN ".
# WASI errno: 0 ok, 8 badf, 28 inval, 52 nosys, 54 notdir, 58 notsup, 63 perm, 76 notcapable.
PROBE = """
(module
  (import "wasi_snapshot_preview1" "fd_prestat_get" (func $prestat (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "path_open" (func $path_open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "environ_sizes_get" (func $env_sizes (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "args_sizes_get" (func $args_sizes (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "sock_accept" (func $sock_accept (param i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1000) "C:/Users")
  (data (i32.const 1100) "../../Windows/win.ini")
  (global $out (mut i32) (i32.const 2048))
  (func $emit (param $tag i32) (param $v i32)
    (i32.store8 (global.get $out) (local.get $tag))
    (i32.store8 offset=1 (global.get $out) (i32.const 61))
    (i32.store8 offset=2 (global.get $out) (i32.add (i32.const 48) (i32.div_u (local.get $v) (i32.const 10))))
    (i32.store8 offset=3 (global.get $out) (i32.add (i32.const 48) (i32.rem_u (local.get $v) (i32.const 10))))
    (i32.store8 offset=4 (global.get $out) (i32.const 32))
    (global.set $out (i32.add (global.get $out) (i32.const 5))))
  (func (export "_start")
    ;; P: fd_prestat_get(3)    O: path_open(3, "C:/Users")   o: path_open(0 = stdin, "../../Windows/win.ini")
    ;; E: environ_sizes_get rc, e: env count   A: argc   S: sock_accept(3)
    (call $emit (i32.const 80) (call $prestat (i32.const 3) (i32.const 200)))
    (call $emit (i32.const 79) (call $path_open (i32.const 3) (i32.const 0) (i32.const 1000) (i32.const 8)
                                 (i32.const 0) (i64.const 0x1fffffff) (i64.const 0x1fffffff) (i32.const 0) (i32.const 300)))
    (call $emit (i32.const 111) (call $path_open (i32.const 0) (i32.const 0) (i32.const 1100) (i32.const 21)
                                 (i32.const 0) (i64.const 0x1fffffff) (i64.const 0x1fffffff) (i32.const 0) (i32.const 300)))
    (call $emit (i32.const 69) (call $env_sizes (i32.const 400) (i32.const 404)))
    (call $emit (i32.const 101) (i32.load (i32.const 400)))
    (drop (call $args_sizes (i32.const 408) (i32.const 412)))
    (call $emit (i32.const 65) (i32.load (i32.const 408)))
    (call $emit (i32.const 83) (call $sock_accept (i32.const 3) (i32.const 0) (i32.const 416)))
    (i32.store (i32.const 0) (i32.const 2048))
    (i32.store (i32.const 4) (i32.sub (global.get $out) (i32.const 2048)))
    (drop (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 8)))))
"""

BAD_IMPORT_ENV = """
(module
  (import "env" "system" (func $system (param i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_start") (drop (call $system (i32.const 0)))))
"""

BAD_IMPORT_WASI = """
(module
  (import "wasi_snapshot_preview1" "sock_open" (func $sock_open (param i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_start") (drop (call $sock_open (i32.const 1) (i32.const 1) (i32.const 0)))))
"""

# Writes 64 KiB of 'A' to stdout forever, ignoring errors.
HUGE_OUT = """
(module
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
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
      (drop (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 8)))
      (br $w))))
"""

MODULES = {
    "sha_chain": sha_chain_wat(), "echo": ECHO, "loop": LOOP, "counted": COUNTED, "membomb": MEMBOMB,
    "recurse": RECURSE, "sleep": SLEEP, "probe": PROBE, "bad_env": BAD_IMPORT_ENV, "bad_wasi": BAD_IMPORT_WASI,
    "huge_out": HUGE_OUT,
}


def sha_chain_input(rounds: int, seed: bytes = b"\x00" * 32) -> bytes:
    return struct.pack("<Q", rounds) + seed


def sha_chain_reference(rounds: int, seed: bytes = b"\x00" * 32) -> str:
    value = seed
    for _ in range(rounds):
        value = hashlib.sha256(value).digest()
    return value.hex()


def build() -> None:
    BUILD.mkdir(exist_ok=True)
    for name, text in MODULES.items():
        wasm = wasmtime.wat2wasm(text)
        (BUILD / f"{name}.wasm").write_bytes(wasm)
        print(f"{name:10s} {len(wasm):6d} bytes  sha256={hashlib.sha256(wasm).hexdigest()[:16]}")


if __name__ == "__main__":
    build()
