;; Count the primes in [start, end): the same answer as the native prime_range job and as src/main.rs.
;;
;; stdin : "<start> <end>" as decimal text, for example "0 1000000". 0 <= start <= end <= 4000000000.
;; stdout: the count and a newline, for example "78498\n".
;; exit 2: the input could not be read.
;;
;; WASI imports: fd_read, fd_write and proc_exit only. Built into primes.wasm with:
;;   .venv\Scripts\python.exe -c "import wasmtime,sys; open('primes.wasm','wb').write(wasmtime.wat2wasm(open('primes.wat').read()))"
(module
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (param i32)))
  (memory (export "memory") 1)
  ;; 0..15 iovec and counters, 1024..1279 input text, 2048..2079 output text
  (global $pos (mut i32) (i32.const 1024))
  (global $end (mut i32) (i32.const 1024))

  (func $is_prime (param $n i64) (result i32)
    (local $i i64)
    (if (i64.lt_u (local.get $n) (i64.const 2)) (then (return (i32.const 0))))
    (if (i64.lt_u (local.get $n) (i64.const 4)) (then (return (i32.const 1))))
    (if (i64.eqz (i64.rem_u (local.get $n) (i64.const 2))) (then (return (i32.const 0))))
    (if (i64.eqz (i64.rem_u (local.get $n) (i64.const 3))) (then (return (i32.const 0))))
    (local.set $i (i64.const 5))
    (block $done (loop $next
      (br_if $done (i64.gt_u (i64.mul (local.get $i) (local.get $i)) (local.get $n)))
      (if (i64.eqz (i64.rem_u (local.get $n) (local.get $i))) (then (return (i32.const 0))))
      (if (i64.eqz (i64.rem_u (local.get $n) (i64.add (local.get $i) (i64.const 2)))) (then (return (i32.const 0))))
      (local.set $i (i64.add (local.get $i) (i64.const 6)))
      (br $next)))
    (i32.const 1))

  ;; Reads the next decimal number from the input, or exits with code 2 if there is none.
  (func $number (result i64)
    (local $value i64) (local $c i32) (local $digits i32)
    (block $start (loop $skip
      (if (i32.ge_u (global.get $pos) (global.get $end)) (then (call $proc_exit (i32.const 2))))
      (local.set $c (i32.load8_u (global.get $pos)))
      (br_if $start (i32.and (i32.ge_u (local.get $c) (i32.const 48)) (i32.le_u (local.get $c) (i32.const 57))))
      (global.set $pos (i32.add (global.get $pos) (i32.const 1)))
      (br $skip)))
    (block $stop (loop $digit
      (br_if $stop (i32.ge_u (global.get $pos) (global.get $end)))
      (local.set $c (i32.load8_u (global.get $pos)))
      (br_if $stop (i32.or (i32.lt_u (local.get $c) (i32.const 48)) (i32.gt_u (local.get $c) (i32.const 57))))
      (local.set $digits (i32.add (local.get $digits) (i32.const 1)))
      (if (i32.gt_u (local.get $digits) (i32.const 10)) (then (call $proc_exit (i32.const 2))))
      (local.set $value (i64.add (i64.mul (local.get $value) (i64.const 10)) (i64.extend_i32_u (i32.sub (local.get $c) (i32.const 48)))))
      (global.set $pos (i32.add (global.get $pos) (i32.const 1)))
      (br $digit)))
    (local.get $value))

  (func (export "_start")
    (local $n i32) (local $a i64) (local $b i64) (local $count i64) (local $p i32)
    ;; read at most 255 bytes of stdin
    (block $eof (loop $read
      (br_if $eof (i32.ge_u (global.get $end) (i32.const 1279)))
      (i32.store (i32.const 0) (global.get $end))
      (i32.store (i32.const 4) (i32.sub (i32.const 1279) (global.get $end)))
      (if (call $fd_read (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 8)) (then (call $proc_exit (i32.const 2))))
      (local.set $n (i32.load (i32.const 8)))
      (br_if $eof (i32.eqz (local.get $n)))
      (global.set $end (i32.add (global.get $end) (local.get $n)))
      (br $read)))
    (local.set $a (call $number))
    (local.set $b (call $number))
    (if (i32.or (i64.gt_u (local.get $a) (local.get $b)) (i64.gt_u (local.get $b) (i64.const 4000000000)))
      (then (call $proc_exit (i32.const 2))))
    (block $done (loop $each
      (br_if $done (i64.ge_u (local.get $a) (local.get $b)))
      (local.set $count (i64.add (local.get $count) (i64.extend_i32_u (call $is_prime (local.get $a)))))
      (local.set $a (i64.add (local.get $a) (i64.const 1)))
      (br $each)))
    ;; write the count as decimal text, right to left, ending at 2079 with a newline
    (local.set $p (i32.const 2079))
    (i32.store8 (local.get $p) (i32.const 10))
    (loop $digit
      (local.set $p (i32.sub (local.get $p) (i32.const 1)))
      (i32.store8 (local.get $p) (i32.add (i32.const 48) (i32.wrap_i64 (i64.rem_u (local.get $count) (i64.const 10)))))
      (local.set $count (i64.div_u (local.get $count) (i64.const 10)))
      (br_if $digit (i64.ne (local.get $count) (i64.const 0))))
    (i32.store (i32.const 0) (local.get $p))
    (i32.store (i32.const 4) (i32.sub (i32.const 2080) (local.get $p)))
    (drop (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 8)))))
