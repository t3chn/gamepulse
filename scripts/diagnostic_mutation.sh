#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "$0")/.." && pwd)"
temporary_root="$(mktemp -d "${TMPDIR:-/tmp}/gamepulse-m028-mutation.XXXXXX")"

cleanup() {
  rm -rf "$temporary_root"
}
trap cleanup EXIT

prepare_copy() {
  local name="$1"
  local copy_root="$temporary_root/$name"
  mkdir -p "$copy_root"
  (
    cd "$repository_root"
    git ls-files -z | tar --null --files-from=- -cf -
  ) | tar -C "$copy_root" -xf -
  printf '%s\n' "$copy_root"
}

run_mutant() {
  local name="$1"
  local substitution="$2"
  local test_name="$3"
  local copy_root
  copy_root="$(prepare_copy "$name")"
  local target_file="$copy_root/crates/gamepulse-worker-source/tests/live_canary.rs"

  perl -0pi -e "$substitution" "$target_file"
  if (
    cd "$copy_root"
    cargo test --locked --offline -p gamepulse-worker-source --test live_canary \
      "$test_name" -- --exact
  ) >/dev/null 2>&1; then
    printf '%s: surviving\n' "$name"
    return 1
  fi
  printf '%s: caught\n' "$name"
}

run_mutant \
  request-ceiling \
  's/if self\.attempts >= self\.ceiling/if self.attempts > self.ceiling/' \
  diagnostic_fixture_stops_early_and_fails_closed_at_the_request_ceiling
run_mutant \
  parser-rejection \
  's/(ProbeRequest::Finder => \{\n            match parse_listing_page.*?Err\(error\) => \(\n\s*)ParserOutcome::Rejected,/${1}ParserOutcome::Accepted,/s' \
  diagnostic_fixture_reports_valid_and_invalid_link_relations
