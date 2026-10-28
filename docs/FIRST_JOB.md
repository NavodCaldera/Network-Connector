# Submitting your first job and proving the i3 did the work

For measured comparisons of each computer alone and both together, use the
controller's **Benchmark** tab after checking the connection with the demo below.
See [BENCHMARK.md](BENCHMARK.md) for repeated runs, shared worker results and export.

## The demo (start here)

In the controller window open the **Demo** tab and click **Run demo**. It:

1. lists the online workers with their CPU model, threads, RAM and current usage;
2. submits *count the primes below 100,000,000*, split into 40 chunks;
3. shows live progress and each computer's CPU usage;
4. prints the answer and checks it against the known value **5,761,455**;
5. prints **who did the work**: chunks per worker, busy seconds, CPU model and RAM.

To prove the **i3 alone** can do the work, pick it under **Only on worker** and run again.
Every chunk is then attributed to the i3.

Command line:

```powershell
.\.venv\Scripts\nc.exe demo
.\.venv\Scripts\nc.exe demo --target i3-laptop
```

Example output with both machines (your numbers will differ):

```
=== Connected workers ===
  i3-laptop        Intel(R) Core(TM) i3-...  |  4 threads, 11.8 GB RAM  |  now: CPU 3.1%, RAM 41.0%
  i7-local         13th Gen Intel(R) Core(TM) i7-13620H  |  16 threads, 15.7 GB RAM  |  now: CPU 6.5%, RAM 60.2%

Submitted job batch 5a505405202a: count primes below 100,000,000, split into 40 chunks
  progress: 12/40 done, 18 running, 10 queued, 0 failed   [i3-laptop: CPU  97.0%  i7-local: CPU  98.5%]
  ...
=== Result ===
  Primes below 100,000,000: 5761455  ->  CORRECT (matches the known value 5,761,455)
  Total time: 4.1s   (summed worker time: 31.0s)

=== Who did the work ===
  i3-laptop          8 chunks      9.8s busy  (Intel(R) Core(TM) i3-..., 11.8 GB RAM)
  i7-local          32 chunks     21.2s busy  (13th Gen Intel(R) Core(TM) i7-13620H, 15.7 GB RAM)
```

## Keep both computers at 100% for a while

Use **Submit job** → `cpu_burn`, set *seconds* = `60`, and *Split into chunks* = the total number of
worker slots (e.g. 15 + 3 = 18). Watch the CPU graph on the **Dashboard**. Both lines should reach
about 100%, while the laptops stay usable because jobs run at low priority.

## Submitting jobs from the window

**Submit job** tab:

1. **Job type**, e.g. `prime_range`.
2. **Parameters**, e.g. `start = 0`, `end = 50000000`.
3. **Split into chunks:** more chunks means better sharing. Aim for several times the total slots.
4. **Run on:** any worker, or one specific worker.
5. **Timeout**, **Max attempts**, **Label**.
6. **Duplicate guard key:** filled in for you. Clicking *Submit* twice by accident won't create two jobs.
7. Click **Submit job**, then watch it on the **Jobs** tab. Select a job to see its details and
   attempts. Use **Cancel job**, **Cancel whole batch** or **Retry job** as needed.

For `image_resize` and `hash_files`, click **Choose files…**. Each file becomes one job, and its
contents are sent with the job. For a finished image, **Save image result…** stores the resized copy.

## Submitting from the command line

```powershell
$nc = ".\.venv\Scripts\nc.exe"
& $nc job submit prime_range -p start=0 -p end=200000000 --chunks 60 --wait
& $nc job submit monte_carlo_pi -p samples=50000000 --chunks 20 --key pi-run-1 --wait
& $nc job submit python_function --params-json '{\"function\":\"nqueens\",\"args\":{\"n\":10}}' --wait
& $nc job submit image_resize -p width=800 -p height=600 --files .\photos\a.jpg .\photos\b.jpg
& $nc job submit hash_files --files .\report.pdf .\data.csv
& $nc job list
& $nc job status <job-or-batch-id>
& $nc job cancel <job-or-batch-id>
& $nc job retry <job-id>
& $nc status --watch          # terminal dashboard
& $nc controller shutdown     # stop the controller safely
```
