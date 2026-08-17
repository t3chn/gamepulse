#!/usr/bin/env bash
set -euo pipefail

readonly MAX_MUTANTS=3
readonly SOURCE_FILE="crates/gamepulse-application/src/lib.rs"
readonly -a MUTANTS=(
  "skip-first-run-browse"
  "commit-short-selection"
  "emit-duplicate-candidate"
)

if [[ ${#MUTANTS[@]} -ne ${MAX_MUTANTS} ]]; then
  echo "mutation configuration exceeds its declared ceiling" >&2
  exit 2
fi

REPOSITORY_ROOT="$(git rev-parse --show-toplevel)"
if [[ "${REPOSITORY_ROOT}" != "$(pwd)" ]]; then
  echo "run this command from the repository root" >&2
  exit 2
fi

MUTATION_ROOT="$(mktemp -d /tmp/gamepulse-mutation.XXXXXX)"
cleanup() {
  if [[ "${MUTATION_ROOT}" == /tmp/gamepulse-mutation.* && -d "${MUTATION_ROOT}" ]]; then
    rm -rf -- "${MUTATION_ROOT}"
  fi
}
trap cleanup EXIT

mkdir "${MUTATION_ROOT}/repository"
cp -R Cargo.toml Cargo.lock crates "${MUTATION_ROOT}/repository/"

caught=0
noncompiling=0
surviving=0

for mutant in "${MUTANTS[@]}"; do
  mutant_root="${MUTATION_ROOT}/repository-${mutant}"
  cp -R "${MUTATION_ROOT}/repository" "${mutant_root}"
  target="${mutant_root}/${SOURCE_FILE}"

  case "${mutant}" in
    skip-first-run-browse)
      perl -0pi -e 's/CrawlDiscoveryRequest::NewReleases => state\.new_releases_completed\(\),/CrawlDiscoveryRequest::NewReleases => false,/' "${target}"
      ;;
    commit-short-selection)
      perl -0pi -e 's/if selected\.len\(\) != DAILY_CRAWL_SELECTION_LIMIT \{/if selected.len() == DAILY_CRAWL_SELECTION_LIMIT {/g' "${target}"
      ;;
    emit-duplicate-candidate)
      perl -0pi -e 's/&& emitted_ids\.insert\(candidate\.source_product_id\(\)\)/&& { let _ = emitted_ids.insert(candidate.source_product_id()); true }/' "${target}"
      ;;
    *)
      echo "unknown declared mutant: ${mutant}" >&2
      exit 2
      ;;
  esac

  if ! (
    cd "${mutant_root}"
    CARGO_NET_OFFLINE=true CARGO_TARGET_DIR="${MUTATION_ROOT}/target-${mutant}" \
      cargo test -p gamepulse-application --test daily_crawl --locked --no-run --quiet
  ); then
    echo "${mutant}: noncompiling"
    noncompiling=$((noncompiling + 1))
  elif (
    cd "${mutant_root}"
    CARGO_NET_OFFLINE=true CARGO_TARGET_DIR="${MUTATION_ROOT}/target-${mutant}" \
      cargo test -p gamepulse-application --test daily_crawl --locked --quiet
  ); then
    echo "${mutant}: surviving"
    surviving=$((surviving + 1))
  else
    echo "${mutant}: caught"
    caught=$((caught + 1))
  fi
done

echo "mutation summary: caught=${caught} noncompiling=${noncompiling} surviving=${surviving}"
if [[ ${surviving} -ne 0 ]]; then
  exit 1
fi
