#!/bin/bash
#
# Install the runvault CLI and sign it with a stable identity.
#
# JIRA MYTASK-3215.
#
# Run this instead of `cargo install --path crates/runvault-cli`.
#
# `cargo install` leaves an ad-hoc, linker-signed binary. Ad-hoc means it has no
# signing identity, so the only stable name macOS has for it is its CDHash --
# and that changes on every rebuild. TCC pins a granted permission to exactly
# that hash: the stored requirement reads `cdhash H"..."`, nothing else.
#
# So rebuilding the binary silently revokes its own permissions. On 2026-09-07
# that is what happened: the binary was reinstalled at 12:56 the previous day,
# and the 5:30 refresh job then sat for three and a half hours inside its first
# read of ~/Documents, waiting on a consent dialog nobody was awake to answer.
# It held the refresh lock the whole time and wrote nothing to its log, so from
# the outside it looked like a job that had merely started late.
#
# Signing with a real identity replaces the cdhash requirement with a
# certificate one, which the next rebuild does not invalidate. The dialog
# appears once more after the first signed install -- answer it, and that is the
# last of it.

set -uo pipefail

IDENTITY="${RUNVAULT_SIGN_IDENTITY:-Local Code Signing (akitenkrad)}"
BIN="$HOME/.cargo/bin/runvault"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

say() { printf '%s\n' "$*"; }

# Fail before building rather than after. A twelve-minute build that ends in
# "no such identity" teaches nothing that could not have been said up front.
if ! security find-identity -v -p codesigning 2>/dev/null | grep -qF "$IDENTITY"; then
  say "FATAL: no code signing identity named: $IDENTITY"
  say ""
  say "Available:"
  security find-identity -v -p codesigning 2>&1 | sed 's/^/  /'
  say ""
  say "Create one in Keychain Access (Certificate Assistant > Create a"
  say "Certificate, type 'Code Signing', self-signed), or point this script at"
  say "an existing one with RUNVAULT_SIGN_IDENTITY."
  exit 1
fi

# Extra arguments go to cargo, so `install_cli.sh --force` still works.
say "==> cargo install --path $REPO/crates/runvault-cli $*"
if ! cargo install --path "$REPO/crates/runvault-cli" "$@"; then
  say "FATAL: cargo install failed"
  exit 1
fi

if [[ ! -x "$BIN" ]]; then
  say "FATAL: cargo install reported success but $BIN is not executable"
  exit 1
fi

# --force because the binary always arrives already ad-hoc signed, and because
# re-signing an already correctly signed binary has to be a no-op: this script
# is meant to be safe to re-run when you are not sure whether it was run.
say "==> codesign --sign '$IDENTITY'"
if ! codesign --force --sign "$IDENTITY" "$BIN"; then
  say "FATAL: codesign failed"
  exit 1
fi

# Verify the thing that actually matters. `codesign -v` would pass on an ad-hoc
# signature too, and an ad-hoc signature is the whole problem -- so check the
# designated requirement instead, and insist it names a certificate rather than
# a hash of these particular bytes.
requirement="$(codesign -d -r- "$BIN" 2>/dev/null | sed -n 's/^designated => //p')"
if [[ -z "$requirement" ]]; then
  say "FATAL: could not read the designated requirement from $BIN"
  exit 1
fi
if [[ "$requirement" != *"certificate"* ]]; then
  say "FATAL: the signature is still not tied to a certificate."
  say "  designated => $requirement"
  say "  A requirement naming only a cdhash is invalidated by the next rebuild,"
  say "  which is the failure this script exists to prevent."
  exit 1
fi

say ""
say "installed and signed: $BIN"
say "  designated => $requirement"
say ""
say "If macOS asks for access to your Documents folder on the next run, that is"
say "expected once: the requirement changed, so the old grant no longer matches."
say "Approve it and later rebuilds will reuse the grant."
