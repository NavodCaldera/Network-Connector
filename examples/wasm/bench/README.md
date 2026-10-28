# wasm_bench: the benchmark's hash_chain in the Wasm sandbox

This is the Part 1 `benchmark` job's `hash_chain` workload, written in Rust and compiled to WebAssembly, so a
benchmark can show how fast the **sandbox** is next to native code (PLAN §19, the "Wasm overhead" column).

- **stdin:** `"<seed> <start> <units>"`, for example `0 0 2`. `1 <= units <= 500`, `start + units <= 20000`.
- **work:** for each unit `u`, start from the ASCII text `"<seed>:<u>"` and apply SHA-256 2,000,000 times; XOR the
  final digests together.
- **stdout:** `{"units":N,"digest":"<64 hex>"}`: byte for byte what the native `benchmark` job returns, so both engines
  must agree.
- **exit code 2:** the input could not be read.

SHA-256 is written out in `src/main.rs` (FIPS 180-4) so the module has no dependencies. It imports only `fd_read`,
`fd_write`, `environ_get`, `environ_sizes_get` and `proc_exit`, all on the sandbox's allowlist.

| File | What it is |
|---|---|
| `src/main.rs`, `Cargo.toml` | The source |
| `bench.wasm` | The prebuilt module (Rust 1.97.0, `wasm32-wasip1`, release). The same file ships as `network_connector/data/wasm_bench.wasm`, pinned in `benchmark.py` by SHA-256 `245016c78af96ff4ec963b6bbd121119a2e5e5520166b20cfc7c26c1eee64257` |

The benchmark uses the shipped copy; you don't need Rust. To rebuild:

```powershell
rustup target add wasm32-wasip1
cd examples\wasm\bench
cargo build --release --target wasm32-wasip1
```

A rebuild with another compiler version gives a different hash. To ship it, replace both copies and update
`WASM_BENCH_SHA256` in `network_connector/benchmark.py`; `tests/test_wasm_bench.py` checks that the module still gives
the native answer.

Try it on a helper with a sandbox:

```powershell
nc sandbox run examples\wasm\bench\bench.wasm --input-text "7 3 1" --seconds 60
# {"units":1,"digest":"f21b6b5f35b38489a4d9e714089c3df49b59b43de6c0efb2f9d37b43f43cab6a"}
```
