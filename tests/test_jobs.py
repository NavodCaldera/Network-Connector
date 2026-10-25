import base64
import io

import pytest
from PIL import Image

from network_connector import jobs
from network_connector.jobs import JobError, execute, validate


def test_number_batch():
    assert execute("number_batch", {"start": 1, "count": 4})["sum_of_squares"] == 30


def test_prime_range_known_values():
    assert execute("prime_range", {"start": 0, "end": 100})["primes"] == 25
    assert execute("prime_range", {"start": 0, "end": 1_000_000})["primes"] == 78_498
    assert execute("prime_range", {"start": 90, "end": 100})["primes"] == 1  # 97


def test_split_prime_range_matches_whole():
    parts = jobs.split("prime_range", {"start": 0, "end": 1_000_000}, 7)
    assert len(parts) == 7
    results = [execute("prime_range", p) for p in parts]
    assert jobs.combine("prime_range", results)["primes"] == 78_498


def test_split_number_batch_matches_whole():
    whole = execute("number_batch", {"start": 5, "count": 1000})["sum_of_squares"]
    parts = jobs.split("number_batch", {"start": 5, "count": 1000}, 6)
    assert jobs.combine("number_batch", [execute("number_batch", p) for p in parts])["sum_of_squares"] == whole


def test_monte_carlo_is_reasonable_and_deterministic():
    a = execute("monte_carlo_pi", {"samples": 200_000, "seed": 3})
    assert execute("monte_carlo_pi", {"samples": 200_000, "seed": 3}) == a
    assert 3.1 < a["pi_estimate"] < 3.2


def test_python_function_allowlist():
    assert execute("python_function", {"function": "nqueens", "args": {"n": 6}})["solutions"] == 4
    fib = execute("python_function", {"function": "fibonacci", "args": {"n": 100}})
    assert fib["last_20"] == "54224848179261915075" and fib["digits"] == 21
    with pytest.raises(JobError):
        execute("python_function", {"function": "os.system", "args": {"n": 1}})
    with pytest.raises(JobError):
        execute("python_function", {"function": "nqueens", "args": {"n": 99}})


def test_hash_files():
    data = base64.b64encode(b"hello").decode()
    out = execute("hash_files", {"files": [{"name": "a.txt", "data_b64": data}]})
    assert out["files"][0]["digest"] == "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"


def _png(w=100, h=50) -> str:
    buf = io.BytesIO()
    Image.new("RGB", (w, h), (10, 120, 200)).save(buf, "PNG")
    return base64.b64encode(buf.getvalue()).decode()


def test_image_resize():
    out = execute("image_resize", {"name": "x.png", "image_b64": _png(), "width": 20, "height": 20})
    assert out["source_size"] == [100, 50] and out["output_size"] == [20, 10]
    resized = Image.open(io.BytesIO(base64.b64decode(out["output_b64"])))
    assert resized.size == (20, 10)


def test_image_decompression_bomb_rejected():
    big = _png(9000, 9000)  # 81 megapixels, above the 40 MP limit
    with pytest.raises(JobError):
        execute("image_resize", {"name": "bomb.png", "image_b64": big, "width": 10, "height": 10})


@pytest.mark.parametrize("kind,params", [
    ("shell", {"cmd": "dir"}),
    ("os.system", {}),
    ("number_batch", {"start": 1, "count": 4, "extra": 1}),
    ("number_batch", {"start": True, "count": 4}),
    ("number_batch", {"start": "1", "count": 4}),
    ("number_batch", {"start": 1, "count": 10**12}),
    ("number_batch", {"start": -1, "count": 5}),
    ("prime_range", {"start": 10, "end": 5}),
    ("cpu_burn", {"seconds": 100000}),
    ("hash_files", {"files": [{"name": "../../secret.txt", "data_b64": "aGk="}]}),
    ("hash_files", {"files": [{"name": "C:\\Windows\\win.ini", "data_b64": "aGk="}]}),
    ("hash_files", {"files": [{"name": "ok.txt", "data_b64": "!!notbase64"}]}),
    ("hash_files", {"files": [{"name": "ok.txt", "data_b64": "aGk=", "path": "C:/x"}]}),
    ("image_resize", {"name": "a.png", "image_b64": "aGk=", "width": 0, "height": 5}),
])
def test_invalid_jobs_are_rejected(kind, params):
    with pytest.raises(JobError):
        validate(kind, params)


def test_unsplittable_job_cannot_be_chunked():
    with pytest.raises(JobError):
        jobs.split("hash_files", {"files": [{"name": "a", "data_b64": "aGk="}]}, 3)
