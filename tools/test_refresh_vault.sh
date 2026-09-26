#!/bin/bash

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REFRESH="$SCRIPT_DIR/refresh_vault.sh"
TMPDIR="${TMPDIR:-/tmp}"
export TMPDIR
NODE="$(command -v node 2>/dev/null || true)"
if [[ -z "$NODE" ]]; then
  for candidate in /opt/homebrew/bin/node /usr/local/bin/node /usr/bin/node; do
    if [[ -x "$candidate" ]]; then
      NODE="$candidate"
      break
    fi
  done
fi
if [[ -z "$NODE" ]]; then
  printf 'FATAL: node is required to validate the refresh status JSON\n' >&2
  exit 1
fi
TEST_ROOT="$(mktemp -d "$TMPDIR/runvault-refresh-test.XXXXXX")"
PASS=0
FAIL=0

cleanup() {
  rm -rf "$TEST_ROOT"
}
trap cleanup EXIT

pass() {
  PASS=$((PASS + 1))
  printf 'ok %d - %s\n' "$((PASS + FAIL))" "$1"
}

fail() {
  FAIL=$((FAIL + 1))
  printf 'not ok %d - %s\n' "$((PASS + FAIL))" "$1"
}

run_test() {
  local name="$1"
  shift
  if "$@"; then
    pass "$name"
  else
    fail "$name"
  fi
}

SOURCE_REPO="$TEST_ROOT/source"
mkdir -p "$SOURCE_REPO"
git -C "$SOURCE_REPO" init -q
git -C "$SOURCE_REPO" -c user.name=Test -c user.email=test@example.invalid \
  commit --allow-empty -qm first
PREVIOUS_COMMIT="$(git -C "$SOURCE_REPO" rev-parse HEAD)"
git -C "$SOURCE_REPO" -c user.name=Test -c user.email=test@example.invalid \
  commit --allow-empty -qm second
SOURCE_HEAD="$(git -C "$SOURCE_REPO" rev-parse HEAD)"

STUB="$TEST_ROOT/runvault"
cat > "$STUB" <<'STUB'
#!/bin/bash
set -u
case "${1:-}" in
  --version)
    printf '%s\n' "${STUB_VERSION:-runvault 0.1.0}"
    ;;
  gc)
    ;;
  sync)
    repo_id=""
    while (( $# > 0 )); do
      if [[ "$1" == "--repo-id" ]]; then
        repo_id="$2"
        break
      fi
      shift
    done
    if [[ "$repo_id" == "${STUB_FAIL_REPO:-}" ]]; then
      printf 'skip run.json: fixture rejected\n'
      exit 1
    fi
    printf '1 run を同期しました\n'
    ;;
  query)
    ;;
  report)
    ;;
  *)
    printf 'unexpected command: %s\n' "$*" >&2
    exit 2
    ;;
esac
STUB
chmod +x "$STUB"

NOTIFIER="$TEST_ROOT/notify"
cat > "$NOTIFIER" <<'NOTIFIER'
#!/bin/bash
printf '%s\t%s\n' "$1" "$2" >> "$NOTIFY_LOG"
NOTIFIER
chmod +x "$NOTIFIER"

make_case() {
  local name="$1"
  local dir="$TEST_ROOT/$name"
  mkdir -p "$dir/vault" "$dir/search/repo-ok/results" "$dir/tmp"
  : > "$dir/vault/runvault-vault.toml"
  : > "$dir/notifications"
  printf '%s\n' "$dir"
}

run_refresh() {
  local dir="$1"
  shift
  env -i \
    HOME="$dir/home" \
    PATH=/usr/bin:/bin \
    TMPDIR="$dir/tmp" \
    RUNVAULT="$STUB" \
    VAULT="$dir/vault" \
    DASHBOARD_JSON="$dir/runs.json" \
    RUNVAULT_REFRESH_SEARCH_ROOTS="$dir/search" \
    RUNVAULT_REFRESH_STATUS="$dir/status.json" \
    RUNVAULT_SOURCE_REPO="$SOURCE_REPO" \
    RUNVAULT_REFRESH_NOTIFY_CMD="$NOTIFIER" \
    RUNVAULT_REFRESH_NOTIFY=1 \
    RUNVAULT_REFRESH_LOG="$dir/refresh.log" \
    NOTIFY_LOG="$dir/notifications" \
    STUB_VERSION="runvault 0.1.0 (commit $SOURCE_HEAD)" \
    "$@" \
    /bin/bash "$REFRESH" > "$dir/stdout" 2>&1
}

test_success() {
  local dir
  dir="$(make_case success)"
  run_refresh "$dir"
  local rc=$?
  (( rc == 0 )) || return 1
  [[ ! -s "$dir/notifications" ]] || return 1
  "$NODE" -e '
    const value = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
    const timestamp = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}[+-]\d{2}:\d{2}$/;
    if (value.schema !== "runvault-refresh-status/1") process.exit(1);
    if (!timestamp.test(value.started_at) || !timestamp.test(value.finished_at)) process.exit(1);
    if (value.outcome !== "ok" || value.exit_code !== 0) process.exit(1);
    if (value.failed_repos.length !== 0 || value.warnings.length !== 0) process.exit(1);
    if (value.message !== "all done") process.exit(1);
    if (value.binary_commit !== process.argv[2] || value.source_head !== process.argv[2]) process.exit(1);
    if (value.log !== process.argv[3]) process.exit(1);
  ' "$dir/status.json" "$SOURCE_HEAD" "$dir/refresh.log"
}

test_sync_failure() {
  local dir
  dir="$(make_case sync-failure)"
  mkdir -p "$dir/search/repo-bad/results"
  run_refresh "$dir" STUB_FAIL_REPO=repo-bad
  local rc=$?
  (( rc == 1 )) || return 1
  [[ "$(wc -l < "$dir/notifications" | tr -d ' ')" == 1 ]] || return 1
  grep -q $'^runvault refresh failed\t' "$dir/notifications" || return 1
  "$NODE" -e '
    const value = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
    if (value.outcome !== "failed" || value.exit_code !== 1) process.exit(1);
    if (JSON.stringify(value.failed_repos) !== JSON.stringify(["repo-bad"])) process.exit(1);
  ' "$dir/status.json" || return 1
  printf '%s\n' '--- test 2 status.json ---'
  cat "$dir/status.json"
  printf '%s\n' '--- end test 2 status.json ---'
}

test_commit_mismatch() {
  local dir
  dir="$(make_case mismatch)"
  run_refresh "$dir" STUB_VERSION="runvault 0.1.0 (commit $PREVIOUS_COMMIT)"
  local rc=$?
  (( rc == 0 )) || return 1
  [[ "$(wc -l < "$dir/notifications" | tr -d ' ')" == 1 ]] || return 1
  grep -q $'^runvault refresh: warning\tbinary [0-9a-f]\{7\} ' "$dir/notifications" || return 1
  "$NODE" -e '
    const value = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
    if (value.outcome !== "warn" || value.warnings.length < 1) process.exit(1);
    if (value.message !== value.warnings[0]) process.exit(1);
    if (value.binary_commit !== process.argv[2] || value.source_head !== process.argv[3]) process.exit(1);
  ' "$dir/status.json" "$PREVIOUS_COMMIT" "$SOURCE_HEAD"
}

test_old_version_format() {
  local dir
  dir="$(make_case old-version)"
  run_refresh "$dir" STUB_VERSION="runvault 0.1.0"
  local rc=$?
  (( rc == 0 )) || return 1
  "$NODE" -e '
    const value = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
    if (value.outcome !== "warn" || value.warnings.length < 1) process.exit(1);
    if (value.binary_commit !== null || value.source_head !== process.argv[2]) process.exit(1);
  ' "$dir/status.json" "$SOURCE_HEAD"
}

test_live_lock_preserves_status() {
  local dir
  dir="$(make_case live-lock)"
  printf 'previous failure\n' > "$dir/status.json"
  mkdir "$dir/tmp/runvault-refresh.lock"
  printf '%s\n' "$$" > "$dir/tmp/runvault-refresh.lock/pid"
  run_refresh "$dir"
  local rc=$?
  (( rc == 0 )) || return 1
  [[ "$(cat "$dir/status.json")" == "previous failure" ]]
}

test_fatal_message_is_json_safe() {
  local dir vault expected
  dir="$(make_case json-escape)"
  vault="$dir/vault-\"-\\-"$'\t'"-end"
  mkdir -p "$vault"
  env -i \
    HOME="$dir/home" \
    PATH=/usr/bin:/bin \
    TMPDIR="$dir/tmp" \
    RUNVAULT="$STUB" \
    VAULT="$vault" \
    DASHBOARD_JSON="$dir/runs.json" \
    RUNVAULT_REFRESH_SEARCH_ROOTS="$dir/search" \
    RUNVAULT_REFRESH_STATUS="$dir/status.json" \
    RUNVAULT_SOURCE_REPO="$SOURCE_REPO" \
    RUNVAULT_REFRESH_NOTIFY_CMD="$NOTIFIER" \
    RUNVAULT_REFRESH_NOTIFY=0 \
    RUNVAULT_REFRESH_LOG="$dir/refresh.log" \
    NOTIFY_LOG="$dir/notifications" \
    STUB_VERSION="runvault 0.1.0 (commit $SOURCE_HEAD)" \
    /bin/bash "$REFRESH" > "$dir/stdout" 2>&1
  local rc=$?
  (( rc == 1 )) || return 1
  expected="FATAL: $vault/runvault-vault.toml is missing; the vault is not declared"
  EXPECTED_MESSAGE="$expected" "$NODE" -e '
    const value = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
    const keys = ["schema", "started_at", "finished_at", "exit_code", "outcome",
      "failed_repos", "warnings", "message", "binary_commit", "source_head", "log"];
    if (JSON.stringify(Object.keys(value)) !== JSON.stringify(keys)) process.exit(1);
    if (value.message !== process.env.EXPECTED_MESSAGE) process.exit(1);
    if (value.outcome !== "failed" || value.exit_code !== 1) process.exit(1);
  ' "$dir/status.json"
}

test_japanese_message_survives_launchd_environment() {
  local dir vault expected
  dir="$(make_case japanese-message)"
  vault="$dir/日本語の保管庫"
  mkdir -p "$vault"
  env -i \
    HOME="$dir/home" \
    PATH=/usr/bin:/bin \
    TMPDIR="$dir/tmp" \
    RUNVAULT="$STUB" \
    VAULT="$vault" \
    DASHBOARD_JSON="$dir/runs.json" \
    RUNVAULT_REFRESH_SEARCH_ROOTS="$dir/search" \
    RUNVAULT_REFRESH_STATUS="$dir/status.json" \
    RUNVAULT_SOURCE_REPO="$SOURCE_REPO" \
    RUNVAULT_REFRESH_NOTIFY_CMD="$NOTIFIER" \
    RUNVAULT_REFRESH_NOTIFY=0 \
    RUNVAULT_REFRESH_LOG="$dir/refresh.log" \
    NOTIFY_LOG="$dir/notifications" \
    STUB_VERSION="runvault 0.1.0 (commit $SOURCE_HEAD)" \
    /bin/bash "$REFRESH" > "$dir/stdout" 2>&1
  local rc=$?
  (( rc == 1 )) || return 1
  expected="FATAL: $vault/runvault-vault.toml is missing; the vault is not declared"
  EXPECTED_MESSAGE="$expected" "$NODE" -e '
    const value = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
    if (value.message !== process.env.EXPECTED_MESSAGE) process.exit(1);
  ' "$dir/status.json"
}

test_source_repo_is_temporary_git_repo() {
  [[ "$SOURCE_REPO" == "$TEST_ROOT"/* ]] || return 1
  [[ "$(git -C "$SOURCE_REPO" rev-parse HEAD)" == "$SOURCE_HEAD" ]]
}

run_test 'success writes ok status without notifying' test_success
run_test 'sync failure writes failed repository and notifies once' test_sync_failure
run_test 'commit mismatch warns without failing' test_commit_mismatch
run_test 'old version format warns' test_old_version_format
run_test 'live lock leaves the prior status untouched' test_live_lock_preserves_status
run_test 'fatal message is escaped into valid JSON' test_fatal_message_is_json_safe
run_test 'Japanese message survives the launchd environment' test_japanese_message_survives_launchd_environment
run_test 'source comparison uses a temporary git repository' test_source_repo_is_temporary_git_repo

printf '%d passed; %d failed\n' "$PASS" "$FAIL"
(( FAIL == 0 ))
