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

VAULT="$HOME/Documents/Obsidian/_logs/_research"
DASHBOARD_JSON="$HOME/Documents/Obsidian/_emera_components/ダッシュボード/_data/runs.json"
RUNVAULT="$HOME/.cargo/bin/runvault"

# Roots that hold one directory per replication.
SEARCH_ROOTS=(
  "$HOME/Documents/workspace/social-simulation-replications/replications"
  "$HOME/Documents/workspace/edit-books/traffic-anomaly-detection-the-book/replications"
)

log() { printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*"; }

if [[ ! -x "$RUNVAULT" ]]; then
  log "FATAL: $RUNVAULT is not executable. Run: cargo install --path crates/runvault-cli"
  exit 1
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
LOCK="${TMPDIR:-/tmp}/runvault-refresh.lock"
if ! mkdir "$LOCK" 2>/dev/null; then
  if [[ -f "$LOCK/pid" ]] && kill -0 "$(cat "$LOCK/pid" 2>/dev/null)" 2>/dev/null; then
    log "another refresh is running (pid $(cat "$LOCK/pid")); exiting without doing anything"
    exit 0
  fi
  # The holder is gone: a previous run was killed before it could clean up.
  log "removing a stale lock at $LOCK"
  rm -rf "$LOCK"
  mkdir "$LOCK" || { log "FATAL: cannot take the lock at $LOCK"; exit 1; }
fi
echo $$ > "$LOCK/pid"
trap 'rm -rf "$LOCK"' EXIT

failed=()
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
    if ! "$RUNVAULT" gc --results-root "$results" >/dev/null 2>&1; then
      log "WARNING: gc failed for $repo_id (continuing to sync)"
    fi

    # One repository failing must not take the others down with it. That is the
    # whole point of collecting failures instead of `set -e`.
    #
    # The run count is pulled out of sync's own summary rather than just logging
    # "synced". A repository whose results/ holds nothing but a .gitkeep exits 0
    # and copies nothing, and "synced logistello" would read as if data moved.
    # Saying "0 runs" is the difference between a quiet success and a quiet
    # nothing, which is the confusion this whole script exists to remove.
    if out="$("$RUNVAULT" sync --repo-id "$repo_id" --results-root "$results" --vault "$VAULT" 2>&1)"; then
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
if ! "$RUNVAULT" query --refresh --vault "$VAULT" >/dev/null; then
  log "FATAL: query --refresh failed"
  exit 1
fi
log "index rebuilt"

if ! "$RUNVAULT" report --obsidian --vault "$VAULT" -o "$DASHBOARD_JSON"; then
  log "FATAL: report --obsidian failed"
  exit 1
fi
log "wrote $DASHBOARD_JSON"

if (( ${#failed[@]} > 0 )); then
  log "exiting non-zero because these repositories did not sync: ${failed[*]}"
  exit 1
fi

log "all done"
