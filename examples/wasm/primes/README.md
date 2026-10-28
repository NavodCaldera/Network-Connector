# Example Wasm job: count primes (stdin → stdout)

This program counts the primes in `[start, end)`. It gives the same answer as the built-in `prime_range` job,
so you can check the sandbox against native code.

- **stdin:** two decimal numbers, `"<start> <end>"`, for example `0 1000000`. `end` is at most 4,000,000,000.
- **stdout:** the count and a newline, for example `78498`.
- **exit code 2:** the input could not be read.

It uses only stdin and stdout. That is all a job gets in the sandbox: no files, no network and no environment
variables (see [docs/SANDBOX.md](../../../docs/SANDBOX.md)).

| File | What it is |
|---|---|
| `src/main.rs`, `Cargo.toml` | The Rust program (segmented sieve). Build it yourself with the steps below. |
| `primes.wat` | The same program written by hand in WebAssembly text (simple trial division, slower). |
| `primes.wasm` | `primes.wat` compiled. It is committed so you can try the example and the tests run without Rust. |

## Build the Rust version

```powershell
rustup target add wasm32-wasip1
cd examples\wasm\primes
cargo build --release --target wasm32-wasip1
# the module is target\wasm32-wasip1\release\primes.wasm
```

The Rust standard library imports a few WASI functions (`fd_write`, `environ_get`, `proc_exit`, …). All of them are
on the sandbox's allowlist. Don't use `std::fs` or `std::net`: they need `path_open` or sockets, and a module that imports
them is refused.

## Try it on this computer first

The helper must have the sandbox set up once (`nc sandbox setup`, no admin rights needed). Then:

```powershell
nc sandbox run examples\wasm\primes\primes.wasm --input-text "0 1000000"
# 78498
# [ok] exit code 0, 0.45 s, cpu 0.39 s, peak 37 MB
```

## Send it to your helpers

On the requester (controller) computer, split the range yourself: one job per chunk, each with its own input.
The module is uploaded once and shared by its SHA-256.

```powershell
$dir = New-Item -ItemType Directory -Force "$env:TEMP\nc-primes"
0..7 | ForEach-Object { "$($_ * 250000) $(($_ + 1) * 250000)" | Set-Content -NoNewline "$dir\chunk$_.txt" }
nc job submit wasm --module examples\wasm\primes\primes.wasm `
   --input "$dir\chunk0.txt" --input "$dir\chunk1.txt" --input "$dir\chunk2.txt" --input "$dir\chunk3.txt" `
   --input "$dir\chunk4.txt" --input "$dir\chunk5.txt" --input "$dir\chunk6.txt" --input "$dir\chunk7.txt" `
   --memory-mb 32 --seconds 60 --wait
```

Each job prints its count. Their sum for `0 2000000` is 148,933, the same as
`nc job submit prime_range -p start=0 -p end=2000000 --wait`.

Or upload once and refer to the hash:

```powershell
nc module upload examples\wasm\primes\primes.wasm
nc job submit wasm -p module_sha256=<sha256> --input-text "0 1000000" --wait
```
