#!/bin/bash
#
# Sync every replication repository into the aggregation vault, rebuild the
# index, and write the payload the Obsidian dashboard reads.
#
# JIRA MYTASK-3158.
#
# There are three stages between a run on disk and the dashboard:
#
#   results/ --sync--> vault --query --refresh--> index/*.parquet
#            --report --obsidian--> _data/runs.json --> the dashboard
#
# All three used to be typed by hand, and what that cost was not hypothetical:
# 464 runs in mccanne1993 and javitz1991 were never synced at all, and because
# `results/` is gitignored they had no backup anywhere. This script exists so
# that "someone forgot to run it" stops being a way to lose data.
#
# The repositories are DISCOVERED, not listed. A hand-maintained list is what
# failed before: two repositories were added and nobody added them to the list.
# Anything matching <root>/*/results/ is a source.

set -uo pipefail

VAULT="${VAULT:-$HOME/Documents/Obsidian/_logs/_research}"
DASHBOARD_JSON="${DASHBOARD_JSON:-$HOME/Documents/Obsidian/_emera_components/ダッシュボード/_data/runs.json}"
RUNVAULT="${RUNVAULT:-$HOME/.cargo/bin/runvault}"
RUNVAULT_REFRESH_STATUS="${RUNVAULT_REFRESH_STATUS:-$HOME/Documents/Obsidian/_emera_components/ダッシュボード/_data/runs.refresh.json}"
RUNVAULT_SOURCE_REPO="${RUNVAULT_SOURCE_REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
RUNVAULT_REFRESH_NOTIFY="${RUNVAULT_REFRESH_NOTIFY:-1}"
RUNVAULT_REFRESH_LOG="${RUNVAULT_REFRESH_LOG:-$HOME/Library/Logs/obsidian-runvault-refresh.log}"
if [[ "$RUNVAULT_REFRESH_LOG" != /* ]]; then
  RUNVAULT_REFRESH_LOG="$(pwd)/$RUNVAULT_REFRESH_LOG"
fi

# Roots that hold one directory per replication.
SEARCH_ROOTS=(
  "$HOME/Documents/workspace/social-simulation-replications/replications"
  "$HOME/Documents/workspace/edit-books/traffic-anomaly-detection-the-book/replications"
)
if [[ -n "${RUNVAULT_REFRESH_SEARCH_ROOTS+x}" ]]; then
  IFS=: read -r -a SEARCH_ROOTS <<< "$RUNVAULT_REFRESH_SEARCH_ROOTS"
fi

started_at=""
LAST_LOG_LINE=""
LAST_FATAL_LINE=""
LOCK="${TMPDIR:-/tmp}/runvault-refresh.lock"
LOCK_ACQUIRED=0
WRITE_STATUS=1
binary_commit=""
source_head=""
failed=()
warnings=()

log() {
  LAST_LOG_LINE="$*"
  if [[ "$LAST_LOG_LINE" == FATAL:* ]]; then
    LAST_FATAL_LINE="$LAST_LOG_LINE"
  fi
  printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$LAST_LOG_LINE"
}

warn() {
  warnings+=("$*")
  log "WARNING: $*"
}

iso_time() {
  local value
  value="$(date '+%Y-%m-%dT%H:%M:%S%z')"
  printf '%s:%s' "${value%??}" "${value: -2}"
}

json_escape() {
  local value="$1"
  local char code i
  for ((i = 0; i < ${#value}; i++)); do
    char="${value:i:1}"
    case "$char" in
      '"') printf '\\"' ;;
      \\) printf '\\\\' ;;
      $'\b') printf '\\b' ;;
      $'\f') printf '\\f' ;;
      $'\n') printf '\\n' ;;
      $'\r') printf '\\r' ;;
      $'\t') printf '\\t' ;;
      *)
        printf -v code '%d' "'$char"
        if (( code >= 0 && code < 32 )); then
          printf '\\u%04x' "$code"
        else
          printf '%s' "$char"
        fi
        ;;
    esac
  done
}

json_string() {
  printf '"'
  json_escape "$1"
  printf '"'
}

json_array() {
  local separator=""
  local value
  printf '['
  for value in "$@"; do
    printf '%s' "$separator"
    json_string "$value"
    separator=","
  done
  printf ']'
}

write_status() {
  local exit_code="$1"
  local outcome="$2"
  local message="$3"
  local finished_at="$4"
  local directory temporary
  directory="$(dirname "$RUNVAULT_REFRESH_STATUS")"
  temporary="${RUNVAULT_REFRESH_STATUS}.tmp.$$"

  if ! mkdir -p "$directory" 2>/dev/null; then
    log "WARNING: could not create the refresh status directory: $directory"
    return
  fi

  if ! {
    printf '{\n  "schema": "runvault-refresh-status/1",\n'
    printf '  "started_at": '; json_string "$started_at"; printf ',\n'
    printf '  "finished_at": '; json_string "$finished_at"; printf ',\n'
    printf '  "exit_code": %d,\n' "$exit_code"
    printf '  "outcome": '; json_string "$outcome"; printf ',\n'
    printf '  "failed_repos": '
    if (( ${#failed[@]} > 0 )); then json_array "${failed[@]}"; else printf '[]'; fi
    printf ',\n'
    printf '  "warnings": '
    if (( ${#warnings[@]} > 0 )); then json_array "${warnings[@]}"; else printf '[]'; fi
    printf ',\n'
    printf '  "message": '; json_string "$message"; printf ',\n'
    if [[ -n "$binary_commit" ]]; then
      printf '  "binary_commit": '; json_string "$binary_commit"; printf ',\n'
    else
      printf '  "binary_commit": null,\n'
    fi
    if [[ -n "$source_head" ]]; then
      printf '  "source_head": '; json_string "$source_head"; printf ',\n'
    else
      printf '  "source_head": null,\n'
    fi
    printf '  "log": '; json_string "$RUNVAULT_REFRESH_LOG"; printf '\n}\n'
  } > "$temporary"; then
    log "WARNING: could not write refresh status: $RUNVAULT_REFRESH_STATUS"
    rm -f "$temporary"
    return
  fi
  if ! mv "$temporary" "$RUNVAULT_REFRESH_STATUS"; then
    log "WARNING: could not replace refresh status: $RUNVAULT_REFRESH_STATUS"
    rm -f "$temporary"
  fi
}

# A wall-clock limit for every runvault call. macOS ships neither timeout(1) nor
# gtimeout, so it is built here.
#
# The point is not to bound how long real work may take -- the whole refresh
# runs in about a minute -- but to make a stall end. A TCC consent dialog puts
# the call to sleep for as long as nobody answers it, and an indefinite sleep
# inside a daily job is worse than a failure: the lock stays held, the next
# morning's run exits immediately and quietly on it, and the job goes on looking
# like it runs every day while syncing nothing. That is the accident this script
# was written to prevent, reached from a different direction.
#
# Callers do the logging: a message written here would be swallowed by whatever
# redirection the call site applies to this function.
run_limited() {
  local limit="$1"
  shift
  "$@" &
  local pid=$!
  local waited=0
  while kill -0 "$pid" 2>/dev/null; do
    if (( waited >= limit )); then
      kill -TERM "$pid" 2>/dev/null
      sleep 2
      kill -KILL "$pid" 2>/dev/null
      wait "$pid" 2>/dev/null
      return 124
    fi
    sleep 1
    waited=$((waited + 1))
  done
  wait "$pid"
}

notify() {
  local title="$1"
  local message="$2"
  local rc
  if [[ -n "${RUNVAULT_REFRESH_NOTIFY_CMD:-}" ]]; then
    run_limited 10 "$RUNVAULT_REFRESH_NOTIFY_CMD" "$title" "$message" >/dev/null 2>&1
  else
    run_limited 10 osascript \
      -e 'on run argv' \
      -e 'display notification (item 2 of argv) with title (item 1 of argv)' \
      -e 'end run' \
      "$title" "$message" >/dev/null 2>&1
  fi
  rc=$?
  if (( rc != 0 )); then
    log "WARNING: refresh notification failed (exit $rc)"
  fi
}

# One exit path owns the status snapshot as well as lock cleanup. Before
# MYTASK-3379 the log was the only evidence that this unattended job failed,
# so a failure could repeat every morning without reaching the dashboard or the
# user. The original exit code always wins over reporting failures here.
on_exit() {
  local exit_code=$?
  local outcome message finished_at title
  trap - EXIT

  if (( exit_code != 0 )); then
    outcome="failed"
    title="runvault refresh failed"
  elif (( ${#warnings[@]} > 0 )); then
    outcome="warn"
    title="runvault refresh: warning"
  else
    outcome="ok"
    title=""
  fi

  # A clean exit's last line is "all done", which is what a warning would
  # otherwise put in the notification: say what the warning was instead.
  if [[ "$outcome" == "warn" ]]; then
    message="${warnings[0]}"
    if (( ${#warnings[@]} > 1 )); then
      message="$message (+$(( ${#warnings[@]} - 1 )) more)"
    fi
  else
    message="${LAST_FATAL_LINE:-$LAST_LOG_LINE}"
  fi
  finished_at="$(iso_time)"
  if (( WRITE_STATUS )); then
    write_status "$exit_code" "$outcome" "$message" "$finished_at"
    if [[ "$outcome" != "ok" && "$RUNVAULT_REFRESH_NOTIFY" != "0" ]]; then
      notify "$title" "$message"
    fi
  fi
  if (( LOCK_ACQUIRED )); then
    rm -rf "$LOCK"
  fi
  exit "$exit_code"
}

trap on_exit EXIT
started_at="$(iso_time)"

# Say so before anything can block. Until 2026-09-07 the first line this script
# wrote came after the first repository had finished syncing, so a stall before
# that point left no trace at all: a run that started at 5:30 and did nothing
# until 8:58 opened its log with an 8:58 timestamp and read as a job that had
# merely started late (MYTASK-3215).
log "starting (pid $$)"

# Generous on purpose: these catch a hang, not slowness. The longest real stage
# is `query --refresh` at about 40s over 1,200 runs. Overridable from the
# environment so that the timeout path can be exercised without waiting for it.
LIMIT_GC="${LIMIT_GC:-300}"
LIMIT_SYNC="${LIMIT_SYNC:-900}"
LIMIT_QUERY="${LIMIT_QUERY:-1800}"
LIMIT_REPORT="${LIMIT_REPORT:-600}"

# A timeout says the binary is wedged, not that this particular repository is
# bad, so the remaining ones would each wedge in turn. Stop while the log still
# says something useful.
abort_wedged() {
  log "FATAL: $1 did not return within ${2}s; aborting"
  log "  the binary is wedged rather than this repository being at fault."
  log "  Look for a permission dialog: macOS blocks the first read of"
  log "  ~/Documents until someone answers it, and a rebuilt binary has to ask"
  log "  again unless it was signed by tools/install_cli.sh (MYTASK-3215)."
  exit 124
}

if [[ ! -x "$RUNVAULT" ]]; then
  log "FATAL: $RUNVAULT is not executable. Run: tools/install_cli.sh"
  exit 1
fi

# A successful command is not enough here: the installed CLI may predate the
# input format currently written by the source tree. That exact mismatch went
# unnoticed for 18 daily runs because each failure only reached the log
# (MYTASK-3379).
binary_version="$(run_limited 10 "$RUNVAULT" --version 2>/dev/null)"
version_rc=$?
commit_pattern='commit[[:space:]]([0-9a-f]+)(-dirty)?[)]'
if (( version_rc == 0 )) \
  && [[ "$binary_version" =~ $commit_pattern ]] \
  && (( ${#BASH_REMATCH[1]} == 40 )); then
  binary_commit="${BASH_REMATCH[1]}"
  binary_dirty="${BASH_REMATCH[2]:-}"
else
  binary_dirty=""
  warn "binary does not report its commit; reinstall with tools/install_cli.sh"
fi

candidate_head="$(git -C "$RUNVAULT_SOURCE_REPO" rev-parse HEAD 2>/dev/null || true)"
if [[ "$candidate_head" =~ ^[0-9a-f]+$ ]] && (( ${#candidate_head} == 40 )); then
  source_head="$candidate_head"
fi

if [[ -n "$binary_commit" && -n "$source_head" && "$binary_commit" != "$source_head" ]]; then
  if git -C "$RUNVAULT_SOURCE_REPO" cat-file -e "${binary_commit}^{commit}" 2>/dev/null; then
    behind="$(git -C "$RUNVAULT_SOURCE_REPO" rev-list --count "${binary_commit}..${source_head}" 2>/dev/null || true)"
    if [[ "$behind" =~ ^[0-9]+$ ]] && (( behind > 0 )); then
      warn "binary ${binary_commit:0:7} is $behind commits behind HEAD ${source_head:0:7}"
    else
      warn "binary ${binary_commit:0:7} differs from HEAD ${source_head:0:7}"
    fi
  else
    warn "binary ${binary_commit:0:7} is unknown to this repository (HEAD ${source_head:0:7})"
  fi
fi
if [[ -n "$binary_dirty" ]]; then
  warn "binary was built from a dirty worktree"
fi
if (( ${#warnings[@]} > 0 )); then
  log "reinstall: tools/install_cli.sh"
fi

if [[ ! -f "$VAULT/runvault-vault.toml" ]]; then
  # sync would refuse anyway (it is fail-closed on the declaration), but saying
  # so here names the actual problem instead of repeating it once per repo.
  log "FATAL: $VAULT/runvault-vault.toml is missing; the vault is not declared"
  exit 1
fi

# Only one refresh at a time. Two concurrent runs were observed to block: a
# launchd run started while a hand-run copy of this script was still going, and
# `sync` stopped dead inside open() on the first repository until the other side
# finished. A daily job will not normally overlap a manual run, but "normally"
# is what left 464 runs unsynced in the first place.
#
# mkdir is the lock because it is atomic on every filesystem this will meet.

# How long a live holder may hold the lock before it is called stuck rather than
# busy. A refresh takes about a minute.
STUCK_LOCK_MINUTES=60

if ! mkdir "$LOCK" 2>/dev/null; then
  holder="$(cat "$LOCK/pid" 2>/dev/null)"
  if [[ -n "$holder" ]] && kill -0 "$holder" 2>/dev/null; then
    # Age matters, and reporting its absence is what made the 2026-09-07 stall
    # invisible. A holder blocked on a consent dialog is alive, so this branch
    # answered "another refresh is running" and exited 0 -- a hand-run refresh
    # during those three and a half hours would have reported success while
    # copying nothing (MYTASK-3215).
    held_min=$(( ( $(date +%s) - $(stat -f %m "$LOCK" 2>/dev/null || echo 0) ) / 60 ))
    if (( held_min >= STUCK_LOCK_MINUTES )); then
      log "FATAL: pid $holder has held the lock for ${held_min}m; that is stuck, not busy"
      log "  Look for a permission dialog first -- answering it may let it finish."
      log "  Otherwise: kill $holder && rm -rf $LOCK"
      exit 1
    fi
    log "another refresh is running (pid $holder, holding for ${held_min}m); exiting without doing anything"
    WRITE_STATUS=0
    exit 0
  fi
  # The holder is gone: a previous run was killed before it could clean up.
  log "removing a stale lock at $LOCK"
  rm -rf "$LOCK"
  mkdir "$LOCK" || { log "FATAL: cannot take the lock at $LOCK"; exit 1; }
fi
echo $$ > "$LOCK/pid"
LOCK_ACQUIRED=1
synced=0
empty=0

for root in "${SEARCH_ROOTS[@]}"; do
  if [[ ! -d "$root" ]]; then
    log "skip root (not present): $root"
    continue
  fi
  for results in "$root"/*/results; do
    [[ -d "$results" ]] || continue
    repo_id="$(basename "$(dirname "$results")")"

    # Reap first, sync second. A run whose process was killed leaves a lock and
    # no status.json, and left alone it reads as "still running" forever -- so
    # syncing it would copy that non-answer into the index. `gc` turns it into a
    # recorded failure, which is the true statement about it.
    #
    # `gc` refuses to reap a lock whose heartbeat is under five minutes old, so
    # that it cannot kill a run that merely started. That grace period is why
    # this belongs in a daily job and not in the hands of whoever remembers:
    # by the time this runs, anything killed during the day is long past it.
    #
    # Its failure is not fatal. A repository that cannot be reaped can still be
    # synced, and saying so is better than skipping the copy.
    run_limited "$LIMIT_GC" "$RUNVAULT" gc --results-root "$results" >/dev/null 2>&1
    rc=$?
    if (( rc == 124 )); then
      abort_wedged "gc for $repo_id" "$LIMIT_GC"
    elif (( rc != 0 )); then
      warn "gc failed for $repo_id (continuing to sync)"
    fi

    # One repository failing must not take the others down with it. That is the
    # whole point of collecting failures instead of `set -e`.
    #
    # The run count is pulled out of sync's own summary rather than just logging
    # "synced". A repository whose results/ holds nothing but a .gitkeep exits 0
    # and copies nothing, and "synced logistello" would read as if data moved.
    # Saying "0 runs" is the difference between a quiet success and a quiet
    # nothing, which is the confusion this whole script exists to remove.
    out="$(run_limited "$LIMIT_SYNC" "$RUNVAULT" sync --repo-id "$repo_id" --results-root "$results" --vault "$VAULT" 2>&1)"
    rc=$?
    if (( rc == 124 )); then
      abort_wedged "sync for $repo_id" "$LIMIT_SYNC"
    elif (( rc == 0 )); then
      n="$(printf '%s\n' "$out" | sed -n 's/^\([0-9][0-9]*\) run.*同期しました.*/\1/p' | tail -1)"
      if [[ -z "$n" ]]; then
        # The summary line changed shape. Do not guess a number.
        log "synced $repo_id (run count not parsed)"
      elif [[ "$n" == "0" ]]; then
        log "synced $repo_id — 0 runs (nothing to copy)"
        empty=$((empty + 1))
      else
        log "synced $repo_id — $n runs"
      fi
      synced=$((synced + 1))
    else
      # Say WHY, not just that. `tail -5` was here and it was useless: the last
      # lines of sync's output are the per-file listing of the runs that DID
      # copy, plus the summary. The reason sits on the `skip` lines far above
      # it, so the log recorded a failure with no cause and the only way to
      # learn one was to re-run sync by hand (MYTASK-3202).
      log "FAILED to sync $repo_id"
      reasons="$(printf '%s\n' "$out" | grep -E '^skip[[:space:]]' | head -20)"
      [[ -n "$reasons" ]] || reasons="$(printf '%s\n' "$out" | tail -5)"
      while IFS= read -r line; do
        [[ -n "$line" ]] && log "  $line"
      done <<< "$reasons"
      failed+=("$repo_id")
    fi
  done
done

log "sync done: $synced ok (${empty} with 0 runs), ${#failed[@]} failed"

# The index and the report are rebuilt even when some repository failed: the
# ones that did sync should still reach the dashboard. Their own failures are
# fatal to this run, though, because a stale index is what this script exists
# to prevent.
run_limited "$LIMIT_QUERY" "$RUNVAULT" query --refresh --vault "$VAULT" >/dev/null
rc=$?
if (( rc == 124 )); then
  abort_wedged "query --refresh" "$LIMIT_QUERY"
elif (( rc != 0 )); then
  log "FATAL: query --refresh failed"
  exit 1
fi
log "index rebuilt"

run_limited "$LIMIT_REPORT" "$RUNVAULT" report --obsidian --vault "$VAULT" -o "$DASHBOARD_JSON"
rc=$?
if (( rc == 124 )); then
  abort_wedged "report --obsidian" "$LIMIT_REPORT"
elif (( rc != 0 )); then
  log "FATAL: report --obsidian failed"
  exit 1
fi
log "wrote $DASHBOARD_JSON"

if (( ${#failed[@]} > 0 )); then
  log "exiting non-zero because these repositories did not sync: ${failed[*]}"
  exit 1
fi

log "all done"
