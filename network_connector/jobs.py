"""The job allowlist.

The network only ever supplies a job *kind* (a name) and *parameters* (plain data).
Kinds map to fixed functions in this file; nothing received over the network is ever
executed, imported, or used as a file path. Parameters are validated strictly on the
controller (when submitted) and again on the worker (before running).
"""
from __future__ import annotations

import base64
import binascii
import hashlib
import io
import random
import re
import time
import warnings
from dataclasses import dataclass
from math import isqrt
from typing import Any, Callable

Progress = Callable[[float], None]

MAX_FILE_BYTES = 8 * 1024 * 1024
MAX_FILES = 25
MAX_IMAGE_PIXELS = 40_000_000
MAX_INLINE_RESULT_BYTES = 300_000
_SAFE_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._ -]{0,119}$")


class JobError(ValueError):
    """Invalid or disallowed job."""


# ---------------------------------------------------------------- validation helpers
def _only(params: dict[str, Any], allowed: set[str]) -> None:
    extra = set(params) - allowed
    if extra:
        raise JobError(f"unexpected parameter(s): {', '.join(sorted(extra))}")


def _int(params: dict[str, Any], key: str, lo: int, hi: int, default: int | None = None) -> int:
    value = params.get(key, default)
    if value is None:
        raise JobError(f"missing parameter: {key}")
    if isinstance(value, bool) or not isinstance(value, int):
        raise JobError(f"{key} must be an integer")
    if not lo <= value <= hi:
        raise JobError(f"{key} must be between {lo} and {hi}")
    return value


def _choice(params: dict[str, Any], key: str, options: set[str], default: str) -> str:
    value = params.get(key, default)
    if value not in options:
        raise JobError(f"{key} must be one of: {', '.join(sorted(options))}")
    return str(value)


def _safe_name(value: Any) -> str:
    if not isinstance(value, str) or not _SAFE_NAME.match(value) or ".." in value:
        raise JobError("file name must be a plain name (letters, digits, . _ - space), no folders")
    return value


def _b64(value: Any, label: str) -> bytes:
    if not isinstance(value, str):
        raise JobError(f"{label} must be base64 text")
    if len(value) * 3 // 4 > MAX_FILE_BYTES + 4:
        raise JobError(f"{label} larger than {MAX_FILE_BYTES // 1024 // 1024} MB")
    try:
        return base64.b64decode(value, validate=True)
    except (binascii.Error, ValueError) as exc:
        raise JobError(f"{label} is not valid base64") from exc


def partition(total: int, parts: int) -> list[int]:
    parts = max(1, min(parts, total))
    base, extra = divmod(total, parts)
    return [base + (1 if i < extra else 0) for i in range(parts)]


# ---------------------------------------------------------------- number_batch
def _v_number_batch(p: dict[str, Any]) -> dict[str, Any]:
    _only(p, {"start", "count"})
    return {"start": _int(p, "start", 0, 10**12), "count": _int(p, "count", 1, 100_000_000)}


def _r_number_batch(p: dict[str, Any], progress: Progress) -> dict[str, Any]:
    start, count, total, done, step = p["start"], p["count"], 0, 0, 500_000
    while done < count:
        n = min(step, count - done)
        a = start + done
        total += sum(i * i for i in range(a, a + n))
        done += n
        progress(done * 100 / count)
    return {"sum_of_squares": total, "count": count}


def _s_number_batch(p: dict[str, Any], parts: int) -> list[dict[str, Any]]:
    out, cursor = [], p["start"]
    for size in partition(p["count"], parts):
        out.append({"start": cursor, "count": size})
        cursor += size
    return out


# ---------------------------------------------------------------- prime_range
def _v_prime_range(p: dict[str, Any]) -> dict[str, Any]:
    _only(p, {"start", "end"})
    start, end = _int(p, "start", 0, 4_000_000_000), _int(p, "end", 1, 4_000_000_000)
    if end <= start or end - start > 400_000_000:
        raise JobError("end must be greater than start, and the range at most 400,000,000 wide")
    return {"start": start, "end": end}


def _r_prime_range(p: dict[str, Any], progress: Progress) -> dict[str, Any]:
    lo, hi = max(p["start"], 2), p["end"]
    if hi <= lo:
        return {"primes": 0, "start": p["start"], "end": hi}
    limit = isqrt(hi - 1) + 1
    small = bytearray([1]) * (limit + 1)
    small[0:2] = b"\x00\x00"
    for i in range(2, isqrt(limit) + 1):
        if small[i]:
            small[i * i :: i] = bytes(len(range(i * i, limit + 1, i)))
    base = [i for i, flag in enumerate(small) if flag]
    count, pos, seg = 0, lo, 1 << 20
    while pos < hi:
        end = min(pos + seg, hi)
        arr = bytearray([1]) * (end - pos)
        for q in base:
            if q * q >= end:
                break
            first = max(q * q, ((pos + q - 1) // q) * q)
            if first < end:
                arr[first - pos :: q] = bytes(len(range(first - pos, end - pos, q)))
        count += sum(arr)
        pos = end
        progress((pos - lo) * 100 / (hi - lo))
    return {"primes": count, "start": p["start"], "end": hi}


def _s_prime_range(p: dict[str, Any], parts: int) -> list[dict[str, Any]]:
    out, cursor = [], p["start"]
    for size in partition(p["end"] - p["start"], parts):
        out.append({"start": cursor, "end": cursor + size})
        cursor += size
    return out


# ---------------------------------------------------------------- monte_carlo_pi
def _v_monte_carlo(p: dict[str, Any]) -> dict[str, Any]:
    _only(p, {"samples", "seed"})
    return {"samples": _int(p, "samples", 1, 200_000_000), "seed": _int(p, "seed", 0, 2**32, 12345)}


def _r_monte_carlo(p: dict[str, Any], progress: Progress) -> dict[str, Any]:
    rng, total, inside, done, step = random.Random(p["seed"]), p["samples"], 0, 0, 1_000_000
    rand = rng.random
    while done < total:
        n = min(step, total - done)
        inside += sum(1 for _ in range(n) if rand() ** 2 + rand() ** 2 <= 1.0)
        done += n
        progress(done * 100 / total)
    return {"inside": inside, "samples": total, "pi_estimate": 4 * inside / total}


def _s_monte_carlo(p: dict[str, Any], parts: int) -> list[dict[str, Any]]:
    return [{"samples": size, "seed": (p["seed"] + i) % 2**32} for i, size in enumerate(partition(p["samples"], parts))]


# ---------------------------------------------------------------- cpu_burn
def _v_cpu_burn(p: dict[str, Any]) -> dict[str, Any]:
    _only(p, {"seconds"})
    return {"seconds": _int(p, "seconds", 1, 900)}


def _r_cpu_burn(p: dict[str, Any], progress: Progress) -> dict[str, Any]:
    """Keeps one CPU thread busy hashing; used to show full utilisation in the demo."""
    seconds, t0, digest, rounds = p["seconds"], time.monotonic(), b"network-connector", 0
    while True:
        for _ in range(20_000):
            digest = hashlib.sha256(digest).digest()
        rounds += 20_000
        elapsed = time.monotonic() - t0
        progress(min(100.0, elapsed * 100 / seconds))
        if elapsed >= seconds:
            return {"hash_rounds": rounds, "seconds": seconds}


def _s_cpu_burn(p: dict[str, Any], parts: int) -> list[dict[str, Any]]:
    return [dict(p) for _ in range(parts)]


# ---------------------------------------------------------------- hash_files
def _v_hash_files(p: dict[str, Any]) -> dict[str, Any]:
    _only(p, {"files", "algorithm"})
    files = p.get("files")
    if not isinstance(files, list) or not 1 <= len(files) <= MAX_FILES:
        raise JobError(f"files must be a list of 1-{MAX_FILES} items")
    clean = []
    for item in files:
        if not isinstance(item, dict) or set(item) != {"name", "data_b64"}:
            raise JobError("each file needs exactly: name, data_b64")
        _b64(item["data_b64"], "data_b64")
        clean.append({"name": _safe_name(item["name"]), "data_b64": item["data_b64"]})
    return {"files": clean, "algorithm": _choice(p, "algorithm", {"sha256", "sha512", "blake2b"}, "sha256")}


def _r_hash_files(p: dict[str, Any], progress: Progress) -> dict[str, Any]:
    out = []
    for i, item in enumerate(p["files"]):
        raw = _b64(item["data_b64"], "data_b64")
        out.append({"name": item["name"], "bytes": len(raw), "digest": hashlib.new(p["algorithm"], raw).hexdigest()})
        progress((i + 1) * 100 / len(p["files"]))
    return {"algorithm": p["algorithm"], "files": out}


# ---------------------------------------------------------------- image_resize
_IMAGE_FORMATS = ["PNG", "JPEG", "WEBP", "BMP"]


def _v_image_resize(p: dict[str, Any]) -> dict[str, Any]:
    _only(p, {"name", "image_b64", "width", "height", "mode", "format"})
    _b64(p.get("image_b64"), "image_b64")
    return {
        "name": _safe_name(p.get("name")),
        "image_b64": p["image_b64"],
        "width": _int(p, "width", 1, 4096),
        "height": _int(p, "height", 1, 4096),
        "mode": _choice(p, "mode", {"fit", "exact"}, "fit"),
        "format": _choice(p, "format", {"PNG", "JPEG", "WEBP"}, "PNG"),
    }


def _r_image_resize(p: dict[str, Any], progress: Progress) -> dict[str, Any]:
    from PIL import Image

    Image.MAX_IMAGE_PIXELS = MAX_IMAGE_PIXELS
    raw = _b64(p["image_b64"], "image_b64")
    with warnings.catch_warnings():
        warnings.simplefilter("error", Image.DecompressionBombWarning)  # decompression-bomb guard
        try:
            im = Image.open(io.BytesIO(raw), formats=_IMAGE_FORMATS)
            src_size = list(im.size)
            im.load()
        except (OSError, Image.DecompressionBombError, Image.DecompressionBombWarning) as exc:
            raise JobError(f"image rejected: {exc}") from exc
    progress(40)
    size = (p["width"], p["height"])
    if p["mode"] == "fit":
        im.thumbnail(size, Image.Resampling.LANCZOS)
    else:
        im = im.resize(size, Image.Resampling.LANCZOS)
    if p["format"] == "JPEG" and im.mode not in ("RGB", "L"):
        im = im.convert("RGB")
    buf = io.BytesIO()
    im.save(buf, format=p["format"])
    data = buf.getvalue()
    progress(100)
    result: dict[str, Any] = {
        "name": p["name"],
        "source_size": src_size,
        "output_size": list(im.size),
        "output_bytes": len(data),
        "sha256": hashlib.sha256(data).hexdigest(),
    }
    if len(data) <= MAX_INLINE_RESULT_BYTES:
        result["output_b64"] = base64.b64encode(data).decode()
    return result


# ---------------------------------------------------------------- python_function (fixed list)
def _fibonacci(n: int) -> dict[str, Any]:
    def fd(k: int) -> tuple[int, int]:
        if k == 0:
            return 0, 1
        a, b = fd(k >> 1)
        c = a * (2 * b - a)
        d = a * a + b * b
        return (d, c + d) if k & 1 else (c, d)

    value = fd(n)[0]
    text = str(value)
    return {"n": n, "digits": len(text), "last_20": text[-20:]}


def _collatz_longest(limit: int) -> dict[str, Any]:
    cache = {1: 1}
    best, best_len = 1, 1
    for start in range(2, limit + 1):
        n, steps, path = start, 0, []
        while n not in cache:
            path.append(n)
            n = n // 2 if n % 2 == 0 else 3 * n + 1
        length = cache[n]
        for node in reversed(path):
            length += 1
            cache[node] = length
        if cache[start] > best_len:
            best, best_len = start, cache[start]
    return {"limit": limit, "start": best, "chain_length": best_len}


def _nqueens(n: int) -> dict[str, Any]:
    full, count = (1 << n) - 1, 0

    def place(cols: int, d1: int, d2: int) -> None:
        nonlocal count
        if cols == full:
            count += 1
            return
        free = full & ~(cols | d1 | d2)
        while free:
            bit = free & -free
            free ^= bit
            place(cols | bit, ((d1 | bit) << 1) & full, (d2 | bit) >> 1)

    place(0, 0, 0)
    return {"n": n, "solutions": count}


# name -> (callable, {arg: (lo, hi)})
_FUNCTIONS: dict[str, tuple[Callable[..., dict[str, Any]], dict[str, tuple[int, int]]]] = {
    "fibonacci": (_fibonacci, {"n": (1, 2_000_000)}),
    "collatz_longest": (_collatz_longest, {"limit": (2, 3_000_000)}),
    "nqueens": (_nqueens, {"n": (4, 11)}),
}

# Public, read-only view for user interfaces: function name -> {arg: (min, max)}.
FUNCTION_ARGS: dict[str, dict[str, tuple[int, int]]] = {name: dict(spec[1]) for name, spec in _FUNCTIONS.items()}


def _v_python_function(p: dict[str, Any]) -> dict[str, Any]:
    _only(p, {"function", "args"})
    name = p.get("function")
    if name not in _FUNCTIONS:
        raise JobError(f"function not allowlisted; choose one of: {', '.join(sorted(_FUNCTIONS))}")
    args = p.get("args")
    spec = _FUNCTIONS[name][1]
    if not isinstance(args, dict):
        raise JobError("args must be an object")
    _only(args, set(spec))
    return {"function": name, "args": {k: _int(args, k, lo, hi) for k, (lo, hi) in spec.items()}}


def _r_python_function(p: dict[str, Any], progress: Progress) -> dict[str, Any]:
    result = _FUNCTIONS[p["function"]][0](**p["args"])
    progress(100)
    return result


# ---------------------------------------------------------------- registry
HASH_ROUNDS = 2_000_000
SIEVE_UNIT = 10_000_000


def _v_benchmark_total(p: dict[str, Any]) -> dict[str, Any]:
    _only(p, {"workload", "units", "seed", "start"})
    start = _int(p, "start", 0, 19999, 0)
    units = _int(p, "units", 1, 20000)
    if start + units > 20000:
        raise JobError("benchmark range must end at or before 20000 units")
    return {"workload": _choice(p, "workload", {"hash_chain", "prime_sieve"}, "hash_chain"), "units": units, "seed": _int(p, "seed", 0, 2**32, 0), "start": start}


def _v_benchmark(p: dict[str, Any]) -> dict[str, Any]:
    clean = _v_benchmark_total(p)
    if clean["units"] > 500:
        raise JobError("each benchmark chunk must have at most 500 units")
    return clean


def _s_benchmark(p: dict[str, Any], parts: int) -> list[dict[str, Any]]:
    out, cursor = [], p["start"]
    for size in partition(p["units"], max(parts, (p["units"] + 499) // 500)):
        out.append(p | {"start": cursor, "units": size})
        cursor += size
    return out


def _r_benchmark(p: dict[str, Any], progress: Progress) -> dict[str, Any]:
    digest, primes = 0, 0
    for offset in range(p["units"]):
        unit = p["start"] + offset
        if p["workload"] == "hash_chain":
            value = f"{p['seed']}:{unit}".encode("ascii")
            for _ in range(HASH_ROUNDS):
                value = hashlib.sha256(value).digest()
            digest ^= int.from_bytes(value, "big")
        else:
            primes += _r_prime_range({"start": unit * SIEVE_UNIT, "end": (unit + 1) * SIEVE_UNIT}, lambda _: None)["primes"]
        progress((offset + 1) * 100 / p["units"])
    return {"units": p["units"], "digest": f"{digest:064x}"} if p["workload"] == "hash_chain" else {"units": p["units"], "primes": primes}


@dataclass(frozen=True)
class JobSpec:
    name: str
    description: str
    example: dict[str, Any]
    validate: Callable[[dict[str, Any]], dict[str, Any]]
    run: Callable[[dict[str, Any], Progress], dict[str, Any]]
    split: Callable[[dict[str, Any], int], list[dict[str, Any]]] | None = None
    validate_total: Callable[[dict[str, Any]], dict[str, Any]] | None = None


REGISTRY: dict[str, JobSpec] = {
    s.name: s
    for s in (
        JobSpec("benchmark", "Fixed work for comparing machines", {"workload": "hash_chain", "units": 4}, _v_benchmark, _r_benchmark, _s_benchmark, _v_benchmark_total),
        JobSpec("number_batch", "Sum of squares over a range of numbers", {"start": 1, "count": 5_000_000},
                _v_number_batch, _r_number_batch, _s_number_batch),
        JobSpec("prime_range", "Count the primes in a range", {"start": 0, "end": 50_000_000},
                _v_prime_range, _r_prime_range, _s_prime_range),
        JobSpec("monte_carlo_pi", "Estimate pi by random sampling", {"samples": 5_000_000, "seed": 1},
                _v_monte_carlo, _r_monte_carlo, _s_monte_carlo),
        JobSpec("cpu_burn", "Keep a CPU thread fully busy (utilisation demo)", {"seconds": 20},
                _v_cpu_burn, _r_cpu_burn, _s_cpu_burn),
        JobSpec("hash_files", "Hash files whose contents are sent inside the job", {"algorithm": "sha256"},
                _v_hash_files, _r_hash_files),
        JobSpec("image_resize", "Resize one image sent inside the job", {"width": 800, "height": 600},
                _v_image_resize, _r_image_resize),
        JobSpec("python_function", "Run a function from a fixed built-in list",
                {"function": "nqueens", "args": {"n": 9}}, _v_python_function, _r_python_function),
    )
}


def validate(kind: str, params: Any) -> dict[str, Any]:
    spec = REGISTRY.get(kind) if isinstance(kind, str) else None
    if spec is None:
        raise JobError(f"job kind {kind!r} is not allowlisted; allowed: {', '.join(sorted(REGISTRY))}")
    if not isinstance(params, dict):
        raise JobError("params must be an object")
    return spec.validate(params)


def execute(kind: str, params: Any, progress: Progress = lambda _pct: None) -> dict[str, Any]:
    """Validate then run. This is the only code path the worker uses to run a job."""
    clean = validate(kind, params)
    return REGISTRY[kind].run(clean, progress)


def split(kind: str, params: dict[str, Any], parts: int) -> list[dict[str, Any]]:
    if kind not in REGISTRY:
        return [validate(kind, params)]
    spec = REGISTRY[kind]
    clean = spec.validate_total(params) if spec.validate_total else validate(kind, params)
    if spec.validate_total and spec.split:
        return [validate(kind, p) for p in spec.split(clean, parts)]
    if parts == 1:
        return [clean]
    if spec.split is None:
        raise JobError(f"{kind} cannot be split into chunks")
    return spec.split(clean, parts)


def combine(kind: str, results: list[dict[str, Any]]) -> Any:
    """Merge chunk results into one answer where that makes sense."""
    if not results:
        return None
    if kind == "number_batch":
        return {"sum_of_squares": sum(r["sum_of_squares"] for r in results), "count": sum(r["count"] for r in results)}
    if kind == "benchmark":
        units = sum(r["units"] for r in results)
        if "primes" in results[0]:
            return {"units": units, "primes": sum(r["primes"] for r in results)}
        digest = 0
        for r in results:
            digest ^= int(r["digest"], 16)
        return {"units": units, "digest": f"{digest:064x}"}
    if kind == "prime_range":
        return {"primes": sum(r["primes"] for r in results)}
    if kind == "monte_carlo_pi":
        inside, samples = sum(r["inside"] for r in results), sum(r["samples"] for r in results)
        return {"pi_estimate": 4 * inside / samples, "samples": samples}
    if kind == "cpu_burn":
        return {"hash_rounds": sum(r["hash_rounds"] for r in results)}
    return None
