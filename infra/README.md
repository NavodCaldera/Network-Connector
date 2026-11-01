# infra

Infrastructure as code and deployment scripts. Owner: Net agent (task 0.7).

## Local Phase 0 stack (Docker, no cloud)

`docker-compose.yml` runs PostgreSQL 17, `nc-coordinator` and `nc-relay` from one image built by `Dockerfile`
(build context: repository root). Every published port binds **127.0.0.1** only; PostgreSQL is not published at all.

1. Make `infra/.env` (git-ignored) with fresh random values:

   ```powershell
   $hex = { -join ((1..32) | ForEach-Object { '{0:x2}' -f (Get-Random -Maximum 256) }) }
   "NC_PG_PASSWORD=$(& $hex)`nNC_ENROLL_SECRET=$(& $hex)`nNC_RELAY_TOKEN_KEY=$(& $hex)" |
     Set-Content -Encoding ascii infra\.env
   ```

2. Start: `docker compose -f infra/docker-compose.yml --env-file infra/.env up -d --build`
3. Check: `curl http://127.0.0.1:8080/healthz` returns `{"ok":true}`.
4. Lease-cycle test against the stack (reads the enrolment secret from the env file; never echo it):

   ```powershell
   $env:NC_TEST_ENROLL = (Select-String -Path infra\.env -Pattern '^NC_ENROLL_SECRET=(.*)$').Matches[0].Groups[1].Value
   cargo test --manifest-path server/Cargo.toml -p nc-coordinator --test lease_cycle -- --ignored --nocapture
   ```

5. Stop: `docker compose -f infra/docker-compose.yml down` (add `-v` to delete the database volume).

Secrets: the three values in `infra/.env` are secrets. Do not commit, paste or log them.

## Not here yet

- TLS: the coordinator speaks HTTPS when `NC_TLS_CERT`/`NC_TLS_KEY` are set; the local stack uses plain HTTP on
  loopback. A real deployment terminates TLS at the coordinator or a reverse proxy.
- Cloud (Singapore VPS + relay VM, PLAN.md §2): needs the owner's approval before anything is created.
