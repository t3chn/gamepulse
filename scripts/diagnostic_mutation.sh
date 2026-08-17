#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "$0")/.." && pwd)"
temporary_root=''
if ! temporary_root="$(mktemp -d "${TMPDIR:-/tmp}/gamepulse-m028-mutation.XXXXXX" 2>/dev/null)"; then
  printf '%s\n' 'diagnostic mutation infrastructure failure' >&2
  exit 1
fi

cleanup() {
  if [ -n "${temporary_root:-}" ]; then
    rm -rf -- "$temporary_root" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

prepare_copy() {
  local name="$1"
  local copy_root="$temporary_root/$name"
  mkdir -p "$copy_root" || return 1
  if ! (
    cd "$repository_root"
    git ls-files -z | tar --null --files-from=- -cf -
  ) | tar -C "$copy_root" -xf -; then
    return 1
  fi
  printf '%s\n' "$copy_root"
}

literal_count() {
  local file="$1"
  local literal="$2"
  MUTATION_LITERAL="$literal" perl -0777 -ne '
    $literal = $ENV{"MUTATION_LITERAL"};
    $count = 0;
    $offset = 0;
    while (($offset = index($_, $literal, $offset)) >= 0) {
      $count += 1;
      $offset += length($literal);
    }
    END { print $count; }
  ' "$file"
}

apply_exact_mutation() {
  local target_file="$1"
  local original="$2"
  local replacement="$3"
  local before_file="$target_file.before"
  local before_count
  before_count="$(literal_count "$target_file" "$original")" || return 1
  [ "$before_count" = '1' ] || return 1
  cp "$target_file" "$before_file" || return 1
  if ! MUTATION_ORIGINAL="$original" MUTATION_REPLACEMENT="$replacement" \
    perl -0777 -i -pe '
      BEGIN {
        $original = $ENV{"MUTATION_ORIGINAL"};
        $replacement = $ENV{"MUTATION_REPLACEMENT"};
      }
      s/\Q$original\E/$replacement/;
    ' "$target_file"; then
    return 1
  fi
  [ "$(literal_count "$target_file" "$original")" = '0' ] || return 1
  ! cmp -s "$before_file" "$target_file"
}

run_named_test() {
  local copy_root="$1"
  local test_name="$2"
  local output_file="$3"
  (
    cd "$copy_root"
    cargo test --locked --offline -p gamepulse-worker-source --test live_canary \
      "$test_name" -- --exact
  ) >"$output_file" 2>&1
}

is_expected_named_failure() {
  local output_file="$1"
  local test_name="$2"
  rg -F -- "test $test_name ... FAILED" "$output_file" >/dev/null \
    && rg -F -- 'failures:' "$output_file" >/dev/null \
    && rg -F -- "    $test_name" "$output_file" >/dev/null
}

run_mutant() {
  local name="$1"
  local target_path="$2"
  local original="$3"
  local replacement="$4"
  local test_name="$5"
  local copy_root
  if ! copy_root="$(prepare_copy "$name")"; then
    printf '%s: infrastructure_failed\n' "$name"
    return 2
  fi
  local target_file="$copy_root/$target_path"
  local baseline_output="$copy_root/baseline-output"
  local mutant_output="$copy_root/mutant-output"

  if ! run_named_test "$copy_root" "$test_name" "$baseline_output"; then
    printf '%s: baseline_failed\n' "$name"
    return 2
  fi
  if ! apply_exact_mutation "$target_file" "$original" "$replacement"; then
    printf '%s: mutation_setup_failed\n' "$name"
    return 2
  fi
  if run_named_test "$copy_root" "$test_name" "$mutant_output"; then
    printf '%s: surviving\n' "$name"
    return 1
  fi
  if ! is_expected_named_failure "$mutant_output" "$test_name"; then
    printf '%s: infrastructure_failed\n' "$name"
    return 2
  fi
  printf '%s: caught\n' "$name"
}

run_mutant \
  request-ceiling \
  crates/gamepulse-worker-source/tests/live_canary.rs \
  'if self.attempts >= self.ceiling' \
  'if self.attempts > self.ceiling' \
  diagnostic_fixture_stops_early_and_fails_closed_at_the_request_ceiling
run_mutant \
  parser-rejection \
  crates/gamepulse-worker-source/tests/live_canary.rs \
  $'Err(error) => (\n                    ParserOutcome::Rejected,\n                    safe_category_for(None, &error),\n                    None,\n                ),' \
  $'Err(error) => (\n                    ParserOutcome::Accepted,\n                    safe_category_for(None, &error),\n                    None,\n                ),' \
  diagnostic_fixture_reports_valid_and_invalid_link_relations
run_mutant \
  fail-closed-exit \
  scripts/diagnostic_canary.sh \
  '    exit 3' \
  '    exit 0' \
  diagnostic_wrapper_preserves_every_fail_closed_verdict_with_exit_three
