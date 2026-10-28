# Comparing the computers

The Benchmark tab runs identical fixed work on each selected worker alone and on
all selected machines together. Start the controller and workers and check that
both machines appear online before starting a comparison.

Choose `hash_chain` for CPU-heavy SHA-256 work or `prime_sieve` for a workload
that also uses more memory. Choose Quick, Standard or Long, or enter an explicit
unit count. Presets calibrate on the participating machines; the slowest machine
can take considerably longer than the selected target. Three repeats and shuffled
scenario order help reveal variation caused by temperature or background work.

The Demo tab remains a quick connection and correctness check. Use Benchmark for
performance comparisons.

## Command line

```powershell
.\.venv\Scripts\nc.exe bench run --size quick --scenarios each,all --repeats 3 --wait
.\.venv\Scripts\nc.exe bench list
.\.venv\Scripts\nc.exe bench show <id>
.\.venv\Scripts\nc.exe bench export <id> --format html -o comparison.html
.\.venv\Scripts\nc.exe bench cancel <id>
.\.venv\Scripts\nc.exe worker perf
```

Use `bench run --units 4 --wait` for a small explicit workload. Worker Performance
shows the latest finished comparison that worker participated in. The worker
fetches it through its existing authenticated outbound connection.

## Reading the results

- Wall time comes from job timestamps stored by the controller. It covers the
  first chunk starting through the last chunk finishing.
- Throughput is completed units divided by wall time.
- Combined speedup is the fastest machine's solo time divided by combined time.
  A value of 1.25 means the combined run has 25% higher throughput.
- Ideal speedup adds the measured solo throughputs and divides by the fastest
  solo throughput. Efficiency compares measured speedup with that ideal.
- Work share counts the chunks and units each worker actually completed.
- CPU seconds measure child-process CPU use; memory records peak usage when the
  operating system makes it available. Missing measurements are not zero usage.
- Repeated runs report a median and a minimum-to-maximum range. Answers must
  agree across scenarios for the comparison to be valid.

Keep both laptops plugged into AC power, close competing CPU-heavy applications,
and use comparable limits for each scenario. Read the saved limits, power, link
and latency information with the results. The i7 also runs the controller, which
adds a small amount of work on that machine.

Cancellation stops the benchmark and requests cancellation of its current batch.
The Jobs tab's **Cancel all** also covers unrelated queued and running jobs.
An unfinished benchmark is marked interrupted after a controller restart; start
a fresh comparison instead of mixing timings from different sessions.

JSON preserves structured results, CSV supports further analysis, and HTML is a
self-contained report that opens without internet access.

## Manual acceptance on the i7 and i3

1. Run Standard, each machine and all together, three repeats on AC power.
2. Check that all answers agree and the controller shows times and work share.
3. Open Performance on the i3 and check it shows the same comparison.
4. Export HTML and open it while disconnected from the internet.
5. Stop a running comparison and check its jobs stop. Test Cancel all separately.
6. Restart an isolated test controller during a run and check interruption is
   reported. Never stop a controller being used for other work for this test.
