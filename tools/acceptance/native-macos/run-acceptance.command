#!/bin/bash
# Portable serial runner for the precompiled CueNote native acceptance kit.
# Requires only macOS userland tools: bash, shasum, grep, sed, awk, mktemp, cp, cat, tee, head, chmod, uname.
set -u -o pipefail

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

cd -- "$(dirname -- "$0")" || fail "cannot enter kit directory"
kit="$(pwd -P)"

[ "$(uname -s)" = "Darwin" ] || fail "this kit runs on macOS only"
[ "$(uname -m)" = "arm64" ] || fail "this kit requires an ARM64 Mac (M1 or later)"
[ -x "./debug/deps/storage_interruptions" ] || fail "missing precompiled test harnesses"
[ -x "./debug/examples/storage_fault_probe" ] || fail "missing precompiled fault probes"
[ -f "./receipt.json" ] || fail "missing receipt.json"
[ -f "./suites.tsv" ] || fail "missing suites.tsv"
[ -f "./SHA256SUMS" ] || fail "missing SHA256SUMS"

grep -q '"sourceDirty": false' ./receipt.json || fail "receipt is not from clean source"
grep -q '"debug": true' ./receipt.json || fail "receipt is not a debug harness build"
grep -q '"platform": "aarch64-apple-darwin"' ./receipt.json ||
  fail "receipt targets the wrong platform"
grep -q '"storage-fault-injection"' ./receipt.json ||
  fail "receipt is missing the fault-injection feature"

printf '%s\n' "Verifying kit hashes..."
(cd -- "$kit" && shasum -a 256 -c SHA256SUMS) ||
  fail "kit hash verification failed; do not run modified or incomplete files"

suite_count="$(awk 'NF { count++ } END { print count + 0 }' suites.tsv)"
[ "$suite_count" -eq 9 ] || fail "expected exactly 9 suites, found $suite_count"
awk 'NF { if (seen[$1]++) exit 1 }' suites.tsv || fail "duplicate suite in manifest"
for probe in storage_fault_probe backup_fault_probe backup_restore_fault_probe library_restore_fault_probe; do
  [ -x "./debug/examples/$probe" ] || fail "missing probe $probe"
done

while IFS=$'\t' read -r suite expected remainder; do
  [ -n "${suite:-}" ] || continue
  [ -z "${remainder:-}" ] || fail "malformed suites.tsv row: $suite"
  case "$suite" in
    storage_interruptions|backup_failures|backup_restore_failures|library_restore_failures|save_failures|backups|backup_restore|library_restore|storage) ;;
    *) fail "unexpected suite in manifest: $suite" ;;
  esac
  case "$expected" in
    ''|*[!0-9]*) fail "invalid test count for $suite" ;;
  esac
  [ "$expected" -gt 0 ] || fail "suite $suite contains zero tests"
  [ -x "./debug/deps/$suite" ] || fail "missing suite executable: $suite"
done < suites.tsv

root="$(mktemp -d "${HOME:?}/CueNote-native-acceptance-XXXXXXXX")" ||
  fail "cannot create a fresh acceptance root"
root="$(cd -- "$root" && pwd -P)"
printf 'CueNote isolated resource tests v1\n' > "$root/.cuenote-resource-tests" ||
  fail "cannot write acceptance root marker"
chmod 600 "$root/.cuenote-resource-tests"
results="$root/results"
mkdir -p "$results"
cp receipt.json suites.tsv "$results/"
source_commit="$(sed -n 's/.*"sourceCommit": "\([^"]*\)".*/\1/p' receipt.json | head -n 1)"
printf 'CueNote native acceptance\nroot=%s\nsourceCommit=%s\n\n' "$root" "$source_commit" |
  tee "$results/console.log"

failed_suites=0
unset CUENOTE_FAULT_ROOT CUENOTE_FAULT_STAGE CUENOTE_FAULT_ACTION CUENOTE_PROBE_CANCEL_PHASE
while IFS=$'\t' read -r suite expected remainder; do
  [ -n "${suite:-}" ] || continue
  log="$results/$suite.log"
  printf '\n===== %s (expected %s tests) =====\n' "$suite" "$expected" | tee -a "$results/console.log"
  CUENOTE_ACCEPTANCE_TEST_ROOT="$root" "./debug/deps/$suite" \
    --test-threads=1 --nocapture >"$log" 2>&1
  exit_code=$?
  cat "$log"
  cat "$log" >> "$results/console.log"

  result_lines="$(grep -c '^test result: ' "$log" || true)"
  result_line="$(grep '^test result: ' "$log" | tail -n 1 || true)"
  passed=""
  failed=""
  ignored=""
  numeric=1
  if [ "$result_lines" -eq 1 ] && [ -n "$result_line" ]; then
    passed="$(printf '%s\n' "$result_line" |
      sed -E 's/^test result: (ok|FAILED)\. ([0-9]+) passed; ([0-9]+) failed; ([0-9]+) ignored;.*$/\2/')"
    failed="$(printf '%s\n' "$result_line" |
      sed -E 's/^test result: (ok|FAILED)\. ([0-9]+) passed; ([0-9]+) failed; ([0-9]+) ignored;.*$/\3/')"
    ignored="$(printf '%s\n' "$result_line" |
      sed -E 's/^test result: (ok|FAILED)\. ([0-9]+) passed; ([0-9]+) failed; ([0-9]+) ignored;.*$/\4/')"
  fi
  for value in "$passed" "$failed" "$ignored"; do
    case "$value" in
      ''|*[!0-9]*) numeric=0 ;;
    esac
  done

  verdict=FAIL
  reason=""
  if [ "$result_lines" -ne 1 ]; then
    reason="missing or duplicate test result summary"
  elif [ "$numeric" -eq 0 ]; then
    reason="unparseable test result summary"
  elif [ "$passed" -eq 0 ]; then
    reason="zero tests ran"
  elif [ "$passed" -ne "$expected" ]; then
    reason="expected $expected tests, saw $passed"
  elif [ "$failed" -ne 0 ]; then
    reason="$failed tests failed"
  elif [ "$ignored" -ne 0 ]; then
    reason="$ignored tests were skipped"
  elif [ "$exit_code" -ne 0 ]; then
    reason="$exit_code exit status"
  else
    verdict=PASS
  fi
  if [ "$verdict" != "PASS" ]; then
    failed_suites=$((failed_suites + 1))
  fi
  printf '%s\texit=%s\texpected=%s\tpassed=%s\tfailed=%s\tignored=%s\t%s\t%s\n' \
    "$suite" "$exit_code" "$expected" "${passed:--}" "${failed:--}" "${ignored:--}" \
    "$verdict" "$reason" >> "$results/summary.txt"
done < suites.tsv

printf '\n===== summary =====\n'
cat "$results/summary.txt" | tee -a "$results/console.log"
if [ "$failed_suites" -ne 0 ]; then
  printf 'FAIL: %s of 9 suites did not pass. Preserve %s\n' "$failed_suites" "$root" >&2
  exit 1
fi
printf 'PASS: all 9 suites passed. Preserve %s\n' "$root"
