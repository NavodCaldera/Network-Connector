#!/usr/bin/env bash
# Linux counterpart of verify.ps1: disposable credentials, real stack, unconditional cleanup.
set +x
set -Eeuo pipefail

skip_build=false
check_cache=false
for option in "$@"; do
  case "$option" in
    --skip-build) skip_build=true ;;
    --check-cache) check_cache=true ;;
    *) printf 'Unknown option: %s\n' "$option" >&2; exit 2 ;;
  esac
done
if "$skip_build" && "$check_cache"; then
  printf '%s\n' '--skip-build and --check-cache cannot be combined.' >&2
  exit 2
fi
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
project_name="${COMPOSE_PROJECT_NAME:-nc-infra-check-$(openssl rand -hex 4)}"
compose=(docker compose --project-name "$project_name" --file infra/docker-compose.yml --progress plain)

# Actual service credentials exist only in this process and its children. No .env/GITHUB_ENV files.
for variable in NC_PG_PASSWORD NC_ENROLL_SECRET NC_RELAY_TOKEN_KEY; do
  secret="$(openssl rand -hex 32)"
  if [[ "${GITHUB_ACTIONS:-false}" == true ]]; then
    printf '::add-mask::%s\n' "$secret"
  fi
  printf -v "$variable" '%s' "$secret"
  export "$variable"
done
unset secret
export NC_TEST_ENROLL="$NC_ENROLL_SECRET"
export NC_TEST_URL=http://127.0.0.1:8080

cleanup() {
  result=$?
  trap - EXIT
  printf '%s\n' 'Stack cleanup: down --volumes --remove-orphans'
  if ! "${compose[@]}" down --volumes --remove-orphans; then
    printf '%s\n' 'Stack cleanup failed.' >&2
    result=1
  fi
  unset NC_PG_PASSWORD NC_ENROLL_SECRET NC_RELAY_TOKEN_KEY NC_TEST_ENROLL
  exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

docker info --format '{{.ServerVersion}}'
if ! "$skip_build"; then
  "${compose[@]}" build
  if "$check_cache"; then "${compose[@]}" build; fi
fi
"${compose[@]}" up -d --wait --wait-timeout 120

healthy=false
for ((attempt=0; attempt<120; attempt++)); do
  if curl --fail --silent --max-time 2 "$NC_TEST_URL/healthz" | jq -e '.ok == true' >/dev/null; then
    healthy=true
    break
  fi
  sleep 1
done
if ! "$healthy"; then printf '%s\n' 'Coordinator health check timed out.' >&2; exit 1; fi
printf '%s\n' 'Coordinator /healthz: ok'
for service in coordinator relay; do
  runtime_uid="$("${compose[@]}" exec -T "$service" id -u)"
  if [[ ! "$runtime_uid" =~ ^[0-9]+$ ]] || [[ "$runtime_uid" -eq 0 ]]; then
    printf '%s must run as non-root.\n' "$service" >&2
    exit 1
  fi
  printf '%s uid: %s\n' "$service" "$runtime_uid"
done
# Keep test stdout captured; credentials and signed requests must not be printed.
cargo test --manifest-path server/Cargo.toml -p nc-coordinator --test lease_cycle -- --ignored
