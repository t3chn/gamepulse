#!/usr/bin/env bash
set -euo pipefail

readonly project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly demo_host="127.0.0.1"
readonly demo_port="3000"
readonly demo_address="${demo_host}:${demo_port}"
readonly demo_url="http://${demo_address}"
readonly temp_root="${TMPDIR:-/tmp}"

demo_dir=""
demo_pid=""

cleanup() {
  local status=$?
  trap - EXIT INT TERM

  if [[ -n "${demo_pid}" ]] && kill -0 "${demo_pid}" 2>/dev/null; then
    kill -INT "${demo_pid}" 2>/dev/null || true
    wait "${demo_pid}" || true
  fi

  if [[ -n "${demo_dir}" && "${demo_dir}" == "${temp_root}/gamepulse-demo."* ]]; then
    rm -rf -- "${demo_dir}"
    printf 'demo: removed temporary fixture data\n'
  fi

  exit "${status}"
}

trap cleanup EXIT
trap 'exit 130' INT TERM

if ! command -v lsof >/dev/null 2>&1; then
  printf 'demo: lsof is required to verify that %s is free\n' "${demo_address}" >&2
  exit 1
fi

if ! command -v curl >/dev/null 2>&1; then
  printf 'demo: curl is required to verify local readiness\n' >&2
  exit 1
fi

if lsof -nP -iTCP:"${demo_port}" -sTCP:LISTEN >/dev/null 2>&1; then
  printf 'demo: %s is already occupied; leave the existing process untouched\n' "${demo_address}" >&2
  exit 1
fi

demo_dir="$(mktemp -d "${temp_root}/gamepulse-demo.XXXXXX")"
readonly demo_database="${demo_dir}/gamepulse.sqlite3"

cd "${project_root}"
cargo build --release --locked --offline -p gamepulse
GAMEPULSE_M019_FIXTURE_PATH="${demo_database}" \
  cargo test --locked --offline -p gamepulse --test m010_catalogue_http \
  seeds_deterministic_visual_fixture_at_requested_path -- --ignored --exact

readonly release_binary="${project_root}/target/release/gamepulse"
if [[ ! -x "${release_binary}" ]]; then
  printf 'demo: release binary was not created at %s\n' "${release_binary}" >&2
  exit 1
fi

GAMEPULSE_DATABASE_PATH="${demo_database}" \
GAMEPULSE_HTTP_ADDRESS="${demo_address}" \
GAMEPULSE_LOG_FORMAT="human" \
GAMEPULSE_SOURCE_WORK_ENABLED="false" \
  "${release_binary}" &
demo_pid=$!

ready="false"
for _ in {1..20}; do
  if ! kill -0 "${demo_pid}" 2>/dev/null; then
    wait "${demo_pid}" || true
    demo_pid=""
    printf 'demo: source-disabled release UI failed before readiness at %s; loopback binding may be denied\n' "${demo_url}" >&2
    exit 1
  fi

  if curl --silent --show-error --fail --max-time 1 "${demo_url}/health/ready" >/dev/null 2>&1; then
    ready="true"
    break
  fi

  sleep 0.1
done

if [[ "${ready}" != "true" ]]; then
  printf 'demo: source-disabled release UI was not ready at %s; loopback binding may be denied\n' "${demo_url}" >&2
  exit 1
fi

if ! lsof -nP -a -p "${demo_pid}" -iTCP:"${demo_port}" -sTCP:LISTEN >/dev/null 2>&1; then
  printf 'demo: release UI did not retain ownership of %s\n' "${demo_address}" >&2
  exit 1
fi

printf 'demo: GamePulse is ready at %s/games\n' "${demo_url}"
printf 'demo: press Ctrl-C to stop the server and remove the temporary fixture data\n'

wait "${demo_pid}"
demo_pid=""
