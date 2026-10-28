//! Count the primes in [start, end): the same answer as the native `prime_range` job.
//!
//! stdin : "<start> <end>" as decimal text, for example "0 1000000". 0 <= start <= end <= 4000000000.
//! stdout: the count and a newline, for example "78498\n".
//! exit 2: the input could not be read.
//!
//! Only stdin and stdout are used: no files, no network, no environment. That is all the sandbox offers.
use std::io::{self, Read, Write};
use std::process::exit;

const MAX_END: u64 = 4_000_000_000;
const SEGMENT: u64 = 1 << 20;

fn small_primes(limit: u64) -> Vec<u64> {
    let limit = limit as usize;
    let mut sieve = vec![true; limit + 1];
    let mut primes = Vec::new();
    for i in 2..=limit {
        if sieve[i] {
            primes.push(i as u64);
            let mut j = i * i;
            while j <= limit {
                sieve[j] = false;
                j += i;
            }
        }
    }
    primes
}

fn count_primes(start: u64, end: u64) -> u64 {
    let lo = start.max(2);
    if end <= lo {
        return 0;
    }
    let base = small_primes((end as f64).sqrt() as u64 + 1);
    let mut count = 0;
    let mut pos = lo;
    let mut segment = vec![true; SEGMENT as usize];
    while pos < end {
        let seg_end = (pos + SEGMENT).min(end);
        let len = (seg_end - pos) as usize;
        segment[..len].fill(true);
        for &q in &base {
            if q * q >= seg_end {
                break;
            }
            let mut m = (q * q).max((pos + q - 1) / q * q);
            while m < seg_end {
                segment[(m - pos) as usize] = false;
                m += q;
            }
        }
        count += segment[..len].iter().filter(|&&is_prime| is_prime).count() as u64;
        pos = seg_end;
    }
    count
}

fn main() {
    let mut text = String::new();
    if io::stdin().take(256).read_to_string(&mut text).is_err() {
        exit(2);
    }
    let numbers: Vec<u64> = text.split_whitespace().filter_map(|s| s.parse().ok()).collect();
    let (start, end) = match numbers.as_slice() {
        [start, end, ..] if start <= end && *end <= MAX_END => (*start, *end),
        _ => exit(2),
    };
    let mut out = io::stdout();
    let _ = writeln!(out, "{}", count_primes(start, end));
}
