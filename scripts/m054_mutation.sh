#!/usr/bin/env bash
set -euo pipefail

readonly MAX_MUTANTS=5
readonly -a MUTANTS=(
  'stale-reclaim-can-settle'
  'post-deadline-can-settle'
  'exhausted-source-retries'
  'ninth-browse-page-schedules'
  'successful-rejection-loses-observation'
)

if [[ ${#MUTANTS[@]} -ne ${MAX_MUTANTS} ]]; then
  printf '%s\n' 'M054 mutation configuration exceeds its declared ceiling' >&2
  exit 2
fi

repository_root="$(cd "$(dirname "$0")/.." && pwd)"
temporary_root=''
temporary_root="$(mktemp -d "${TMPDIR:-/tmp}/gamepulse-m054-mutation.XXXXXX")"
cleanup() {
  if [[ -n "${temporary_root}" && -d "${temporary_root}" ]]; then
    rm -rf -- "${temporary_root}"
  fi
}
trap cleanup EXIT

prepare_copy() {
  local name="$1"
  local copy_root="${temporary_root}/${name}"
  mkdir -p "${copy_root}"
  (
    cd "${repository_root}"
    git ls-files --cached --others --exclude-standard -z | tar --null --files-from=- -cf -
  ) | tar -C "${copy_root}" -xf -
  printf '%s\n' "${copy_root}"
}

literal_count() {
  local file="$1"
  local literal="$2"
  MUTATION_LITERAL="${literal}" perl -0777 -ne '
    $literal = $ENV{"MUTATION_LITERAL"};
    $count = 0; $offset = 0;
    while (($offset = index($_, $literal, $offset)) >= 0) {
      $count += 1; $offset += length($literal);
    }
    END { print $count; }
  ' "${file}"
}

apply_mutation() {
  local file="$1" original="$2" replacement="$3"
  [[ "$(literal_count "${file}" "${original}")" == '1' ]] || return 1
  MUTATION_ORIGINAL="${original}" MUTATION_REPLACEMENT="${replacement}" perl -0777 -i -pe '
    BEGIN { $original = $ENV{"MUTATION_ORIGINAL"}; $replacement = $ENV{"MUTATION_REPLACEMENT"}; }
    s/\Q$original\E/$replacement/;
  ' "${file}"
  [[ "$(literal_count "${file}" "${original}")" == '0' ]]
}

run_test() {
  local root="$1" package="$2" test_target="$3" test_name="$4" output="$5"
  (
    cd "${root}"
    CARGO_NET_OFFLINE=true CARGO_TARGET_DIR="${root}/target" \
      cargo test --locked --offline -p "${package}" ${test_target} "${test_name}" -- --exact
  ) >"${output}" 2>&1
}

run_mutant() {
  local name="$1" source_file="$2" package="$3" test_target="$4" test_name="$5" original="$6" replacement="$7"
  local root baseline mutated
  root="$(prepare_copy "${name}")"
  baseline="${root}/baseline.out"
  mutated="${root}/mutated.out"
  if ! run_test "${root}" "${package}" "${test_target}" "${test_name}" "${baseline}"; then
    printf '%s: baseline_failed\n' "${name}"
    return 2
  fi
  if ! apply_mutation "${root}/${source_file}" "${original}" "${replacement}"; then
    printf '%s: mutation_setup_failed\n' "${name}"
    return 2
  fi
  if run_test "${root}" "${package}" "${test_target}" "${test_name}" "${mutated}"; then
    printf '%s: surviving\n' "${name}"
    return 1
  fi
  if ! rg -F -- "test ${test_name} ... FAILED" "${mutated}" >/dev/null; then
    printf '%s: infrastructure_failed\n' "${name}"
    return 2
  fi
  printf '%s: caught\n' "${name}"
}

run_mutant \
  'stale-reclaim-can-settle' \
  'crates/gamepulse-storage-sqlite/src/run_progress.rs' \
  'gamepulse-storage-sqlite' \
  '' \
  'run_progress::tests::stale_reclaimed_claim_cannot_change_run_item_or_schedule_state' \
  $'               AND claim_token = ?2\n               AND lease_expires_at = ?3\n               AND lease_expires_at > ?4' \
  $'               AND claim_token >= 0\n               AND ?2 = ?2\n               AND ?3 = ?3\n               AND lease_expires_at > ?4'
run_mutant \
  'post-deadline-can-settle' \
  'crates/gamepulse-storage-sqlite/src/run_progress.rs' \
  'gamepulse-storage-sqlite' \
  '' \
  'run_progress::tests::post_deadline_item_settlement_fails_run_without_mutating_candidate_state' \
  $'    if now > run.deadline_at {\n        fail_deadline(&transaction, &run, now)?;\n        transaction\n            .commit()\n            .map_err(RunProgressStoreError::database)?;\n        return Ok(DurableRunProgressOutcome::DeadlineExceeded);\n    }\n    let item = transaction' \
  $'    if false {\n        fail_deadline(&transaction, &run, now)?;\n        transaction\n            .commit()\n            .map_err(RunProgressStoreError::database)?;\n        return Ok(DurableRunProgressOutcome::DeadlineExceeded);\n    }\n    let item = transaction'
run_mutant \
  'exhausted-source-retries' \
  'crates/gamepulse-worker-source/src/lib.rs' \
  'gamepulse' \
  '--test m054_durable_runs' \
  'source_exhaustion_persists_failure_and_current_source_job_settles_without_retry' \
  '| Ok(DurableRunProgressOutcome::SourceExhausted) => JobHandlerResult::Succeeded,' \
  '| Ok(DurableRunProgressOutcome::SourceExhausted) => JobHandlerResult::Failed(JobHandlerFailure::new(DURABLE_RUN_DISCOVERY_FAILURE)),'
run_mutant \
  'ninth-browse-page-schedules' \
  'crates/gamepulse-storage-sqlite/src/run_progress.rs' \
  'gamepulse-storage-sqlite' \
  '' \
  'run_progress::tests::browse_page_limit_is_eight_and_survives_restart_without_a_ninth_page' \
  'const MAX_BROWSE_PAGES: i64 = 8;' \
  'const MAX_BROWSE_PAGES: i64 = 9;'
run_mutant \
  'successful-rejection-loses-observation' \
  'crates/gamepulse-worker-source/src/lib.rs' \
  'gamepulse' \
  '--test m054_durable_runs' \
  'missing_video_is_a_terminal_rejection_and_later_candidates_fill_exact_target' \
  'Some(observation) => JobHandlerResult::SucceededWithObservation(observation),' \
  'Some(_observation) => JobHandlerResult::Succeeded,'
