#!/bin/sh
set -eu
adr='docs/adr/openssh-certificate-identity.md'
plan='TELCHAR_IMPLEMENTATION_PLAN.md'
[ -f "$adr" ] || { echo "missing ADR: $adr" >&2; exit 1; }
for text in \
  '**Status:** Deferred' \
  'CA fingerprint' \
  'certificate key ID' \
  'principals' \
  'real OpenSSH certificate authentication fixture' \
  'client-supplied environment variable' \
  'public-key fingerprint' \
  'Certificate support requires a separate real-sshd fixture'; do
  grep -F -- "$text" "$adr" >/dev/null || { echo "missing certificate deferral evidence: $text" >&2; exit 1; }
done
grep -F -- 'T048 Prototype certificate identity handoff' "$plan" >/dev/null
echo 'OpenSSH certificate identity deferral checklist passed'
