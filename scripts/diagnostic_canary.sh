#!/usr/bin/env bash
set -euo pipefail

safe_failure() {
  printf '%s\n' 'diagnostic command failed' >&2
  exit 1
}

mode="${1:-}"
case "$mode" in
  fixture)
    expected_mode='fixture'
    test_name='diagnostic_fixture_report'
    offline_fixture='true'
    ;;
  finder)
    expected_mode='finder'
    test_name='diagnostic_live_finder'
    offline_fixture='false'
    ;;
  review-continuation)
    expected_mode='review_continuation'
    test_name='diagnostic_live_review_continuation'
    offline_fixture='false'
    ;;
  *)
    printf '%s\n' 'usage: diagnostic mode must be fixture, finder, or review-continuation' >&2
    exit 2
    ;;
esac

command -v jq >/dev/null 2>&1 || safe_failure

temporary_root=''
if ! temporary_root="$(mktemp -d "${TMPDIR:-/tmp}/gamepulse-diagnostic.XXXXXX" 2>/dev/null)"; then
  safe_failure
fi
[ -n "$temporary_root" ] && [ -d "$temporary_root" ] || safe_failure

cleanup() {
  if [ -n "${temporary_root:-}" ]; then
    rm -rf -- "$temporary_root" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

stdout_path="$temporary_root/stdout"
stderr_path="$temporary_root/stderr"
if ! (: >"$stdout_path" && : >"$stderr_path") >/dev/null 2>&1; then
  safe_failure
fi

cargo_arguments=(test --quiet --locked -p gamepulse-worker-source --test live_canary)
if [ "$offline_fixture" = 'true' ]; then
  cargo_arguments+=(--offline)
fi
cargo_arguments+=("$test_name" -- --exact --nocapture)
if ! (cargo "${cargo_arguments[@]}" >"$stdout_path" 2>"$stderr_path") 2>/dev/null; then
  safe_failure
fi

[ -f "$stdout_path" ] && [ -f "$stderr_path" ] || safe_failure
[ ! -s "$stderr_path" ] || safe_failure

report=''
line_number=0
while IFS= read -r line || [ -n "$line" ]; do
  case "$line_number" in
    0)
      [ -z "$line" ] || safe_failure
      ;;
    1)
      [ "$line" = 'running 1 test' ] || safe_failure
      ;;
    2)
      [[ "$line" == \{* ]] || safe_failure
      report="$line"
      ;;
    3)
      [ "$line" = '.' ] || safe_failure
      ;;
    4)
      [[ "$line" =~ ^test\ result:\ ok\.\ 1\ passed\;\ 0\ failed\;\ 0\ ignored\;\ 0\ measured\;\ [0-9]+\ filtered\ out\;\ finished\ in\ [0-9]+(\.[0-9]+)?s$ ]] || safe_failure
      ;;
    5)
      [ -z "$line" ] || safe_failure
      ;;
    *)
      safe_failure
      ;;
  esac
  line_number=$((line_number + 1))
done <"$stdout_path"

[ "$line_number" -eq 6 ] || safe_failure

if ! jq -n -e --stream '
  reduce inputs as $entry
    ([]; if (($entry | length) == 2 and ($entry[0] | type == "array"))
         then . + [($entry[0] | @json)] else . end)
  | . as $paths
  | ($paths | length) == ($paths | unique | length)
' >/dev/null 2>&1 <<<"$report"; then
  safe_failure
fi

if ! terminal_verdict="$(jq -er --arg expected_mode "$expected_mode" '
  def object_with_keys($expected):
    type == "object" and ((keys | sort) == ($expected | sort));
  def integer:
    type == "number" and . == floor;
  def link_checks_valid:
    object_with_keys(["scheme", "host", "path", "query", "progression", "limit", "total_boundary"])
    and ([.scheme, .host, .path, .query, .progression, .limit, .total_boundary]
         | all(type == "boolean"));
  def link_checks_false:
    [.scheme, .host, .path, .query, .progression, .limit, .total_boundary] | all(. == false);
  def link_checks_true:
    [.scheme, .host, .path, .query, .progression, .limit, .total_boundary] | all(. == true);
  def review_request:
    .request == "critic_review" or .request == "user_review";
  def rejected_presence_valid:
    if .continuation_presence == "not_checked" then
      .href_presence == "not_applicable" and (.link_checks | link_checks_false)
    elif (.continuation_presence == "missing" or .continuation_presence == "null"
          or .continuation_presence == "other") then
      .href_presence == "not_applicable" and (.link_checks | link_checks_false)
    elif .continuation_presence == "object" then
      if (.href_presence == "missing" or .href_presence == "null" or .href_presence == "other") then
        .link_checks | link_checks_false
      elif .href_presence == "string" then
        true
      else
        false
      end
    else
      false
    end;
  def exchange_valid:
    object_with_keys([
      "request", "status_category", "expected_content_type", "utf8", "json",
      "item_count", "numeric_total", "continuation_presence", "href_presence",
      "link_checks", "parser", "safe_category"
    ])
    and (.request == "finder" or .request == "critic_review" or .request == "user_review")
    and (.status_category == "ok" or .status_category == "forbidden"
         or .status_category == "rate_limited" or .status_category == "other"
         or .status_category == "not_attempted")
    and ([.expected_content_type, .utf8, .json, .numeric_total] | all(type == "boolean"))
    and (.item_count | integer and . >= 0)
    and (.continuation_presence == "missing" or .continuation_presence == "null"
         or .continuation_presence == "object" or .continuation_presence == "other"
         or .continuation_presence == "not_checked")
    and (.href_presence == "missing" or .href_presence == "null"
         or .href_presence == "string" or .href_presence == "other"
         or .href_presence == "not_applicable")
    and (.link_checks | link_checks_valid)
    and (.parser == "accepted" or .parser == "rejected")
    and (.safe_category == "review_continuation_link"
         or .safe_category == "other_mandatory_stage")
    and (
      if .parser == "accepted" then
        .status_category == "ok" and .expected_content_type and .utf8 and .json
        and .numeric_total and .safe_category == "other_mandatory_stage"
        and (
          (.continuation_presence == "missing" and .href_presence == "not_applicable"
           and (.link_checks | link_checks_false))
          or (.continuation_presence == "object" and .href_presence == "missing"
              and review_request and (.link_checks | link_checks_false))
          or (.continuation_presence == "object" and .href_presence == "string"
              and (.link_checks | link_checks_true))
        )
      else
        .status_category != "not_attempted"
        and (
          if .status_category != "ok" then
            (.utf8 | not) and (.json | not) and (.numeric_total | not)
            and .item_count == 0 and .continuation_presence == "not_checked"
            and .href_presence == "not_applicable" and (.link_checks | link_checks_false)
            and .safe_category == "other_mandatory_stage"
          else
            (
              if .continuation_presence == "not_checked" then
                (.json | not) and (.numeric_total | not) and .item_count == 0
              else
                .expected_content_type and .utf8 and .json
              end
            )
            and rejected_presence_valid
            and (
              .safe_category == "other_mandatory_stage"
              or (.safe_category == "review_continuation_link" and review_request
                  and .continuation_presence == "object" and .expected_content_type
                  and .utf8 and .json and .numeric_total)
            )
          end
        )
      end
    );
  def all_accepted($report):
    ($report.exchanges | all(.parser == "accepted"));
  def prior_exchanges_accepted($report):
    ($report.exchanges[0:-1] | all(.parser == "accepted"));
  . as $report
  | object_with_keys([
      "schema_version", "mode", "request_count", "request_ceiling",
      "terminal_verdict", "exchanges"
    ])
  and (.schema_version == "gamepulse.diagnostic.v1")
  and (.mode == $expected_mode)
  and (.mode == "fixture" or .mode == "finder" or .mode == "review_continuation")
  and (.request_ceiling | integer)
  and (.request_count | integer)
  and (.request_ceiling == (if .mode == "finder" then 1 else 3 end))
  and (.request_count > 0 and .request_count <= .request_ceiling)
  and (.exchanges | type == "array" and length == $report.request_count)
  and (($report.exchanges | all(exchange_valid)))
  and (["finder", "critic_review", "user_review"][0:$report.request_count]
       == ($report.exchanges | map(.request)))
  and (.terminal_verdict == "fixture_validated" or .terminal_verdict == "contract_ready"
       or .terminal_verdict == "access_denied" or .terminal_verdict == "rate_limited"
       or .terminal_verdict == "source_rejected" or .terminal_verdict == "no_candidate"
       or .terminal_verdict == "request_budget_exhausted")
  and (
    if .terminal_verdict == "fixture_validated" then
      .mode == "fixture" and .request_count == 3 and all_accepted($report)
      and .exchanges[0].item_count > 0
    elif .terminal_verdict == "contract_ready" then
      .mode != "fixture" and .request_count == .request_ceiling and all_accepted($report)
      and .exchanges[0].item_count > 0
    elif .terminal_verdict == "access_denied" then
      prior_exchanges_accepted($report)
      and .exchanges[-1].status_category == "forbidden"
      and .exchanges[-1].parser == "rejected"
    elif .terminal_verdict == "rate_limited" then
      prior_exchanges_accepted($report)
      and .exchanges[-1].status_category == "rate_limited"
      and .exchanges[-1].parser == "rejected"
    elif .terminal_verdict == "source_rejected" then
      prior_exchanges_accepted($report)
      and .exchanges[-1].parser == "rejected"
      and (.exchanges[-1].status_category == "ok" or .exchanges[-1].status_category == "other")
    elif .terminal_verdict == "no_candidate" then
      .request_count == 1 and .exchanges[0].request == "finder"
      and .exchanges[0].parser == "accepted" and .exchanges[0].item_count == 0
    else
      .request_count == .request_ceiling and all_accepted($report)
    end
  )
  | if . then $report.terminal_verdict else empty end
' 2>/dev/null <<<"$report")"; then
  safe_failure
fi

printf '%s\n' "$report"
case "$terminal_verdict" in
  fixture_validated|contract_ready)
    exit 0
    ;;
  access_denied|rate_limited|source_rejected|no_candidate|request_budget_exhausted)
    exit 3
    ;;
  *)
    safe_failure
    ;;
esac
