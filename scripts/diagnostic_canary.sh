#!/usr/bin/env bash
set -euo pipefail

mode="${1:-}"
case "$mode" in
  fixture)
    cargo_mode=(--offline diagnostic_ -- --nocapture)
    ;;
  finder)
    cargo_mode=(diagnostic_live_finder -- --ignored --exact --nocapture)
    ;;
  review-continuation)
    cargo_mode=(diagnostic_live_review_continuation -- --ignored --exact --nocapture)
    ;;
  *)
    printf '%s\n' 'usage: diagnostic mode must be fixture, finder, or review-continuation' >&2
    exit 2
    ;;
esac

temporary_root="$(mktemp -d "${TMPDIR:-/tmp}/gamepulse-diagnostic.XXXXXX")"
cleanup() {
  rm -rf "$temporary_root"
}
trap cleanup EXIT

if ! cargo test --quiet --locked -p gamepulse-worker-source --test live_canary \
  "${cargo_mode[@]}" >"$temporary_root/stdout" 2>"$temporary_root/stderr"; then
  printf '%s\n' 'diagnostic command failed' >&2
  exit 1
fi

report_count="$(awk 'index($0, "{") { count += 1 } END { print count + 0 }' "$temporary_root/stdout")"
if [ "$report_count" -ne 1 ]; then
  printf '%s\n' 'diagnostic command failed' >&2
  exit 1
fi

awk 'index($0, "{") { print substr($0, index($0, "{")) }' "$temporary_root/stdout"
