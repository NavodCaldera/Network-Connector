# Example job submissions. Run on the i7 from the repository folder, with the controller running.
$nc = ".\.venv\Scripts\nc.exe"

# Count primes below 200 million, shared across all workers
& $nc job submit prime_range -p start=0 -p end=200000000 --chunks 60 --label "primes 2e8" --wait

# Estimate pi with 50 million samples; the key prevents a duplicate if you run this line twice
& $nc job submit monte_carlo_pi -p samples=50000000 --chunks 20 --key "pi-50m" --wait

# Several independent built-in functions
& $nc job submit python_function --params-json '{\"function\":\"nqueens\",\"args\":{\"n\":10}}'
& $nc job submit python_function --params-json '{\"function\":\"collatz_longest\",\"args\":{\"limit\":2000000}}'

# Keep every worker thread at 100% for one minute (18 = total worker slots, adjust to yours)
& $nc job submit cpu_burn -p seconds=60 --chunks 18 --timeout 120

# Prove the i3 does work: pin a batch to it
& $nc job submit number_batch -p start=1 -p count=20000000 --chunks 8 --target i3-laptop --wait

# Resize photos (each file is one job; contents travel with the job)
& $nc job submit image_resize -p width=1280 -p height=1280 -p format=JPEG --files .\photos\*.jpg

& $nc status
