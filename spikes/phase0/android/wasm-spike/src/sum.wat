;; Tiny compute module: wrapping sum of i for i in [start, end).
(module
  (func (export "sum") (param $start i64) (param $end i64) (result i64)
    (local $acc i64)
    (block $done
      (loop $next
        (br_if $done (i64.ge_s (local.get $start) (local.get $end)))
        (local.set $acc (i64.add (local.get $acc) (local.get $start)))
        (local.set $start (i64.add (local.get $start) (i64.const 1)))
        (br $next)))
    (local.get $acc)))
