#!/usr/bin/env bash
#
# Guard: no private key material is tracked in this repository.
#
# .gitignore cannot do this job on its own. It is advisory about files that are
# not yet tracked and silent about a file added explicitly or added before the
# rule existed -- which is exactly how an EC2 rig key (tb909-key.pem) came to be
# committed under one of the work-in-flight design subtrees while a .gitignore
# rule written to prevent it sat one path segment away, matching nothing.
#
# The path is described rather than spelled: check-design-refs.sh rejects the
# literal design-tree prefix outside docs/, and this comment is a note about
# where a key once sat, not a citation of a document.
#
# This guard reads what git actually tracks, so it cannot be fooled that way.
# A repository is public: a key that lands here is compromised the moment it is
# pushed, and deleting it later does not un-publish it. Fail the build instead.

set -euo pipefail

cd "$(dirname "$0")/.."

fail=0

# 1. Key material by filename.
by_name=$(git ls-files | grep -iE '\.(pem|p12|pfx|jks|keystore)$|(^|/)(id_rsa|id_ed25519|id_ecdsa|id_dsa)$' || true)
if [ -n "$by_name" ]; then
  echo "check-no-secrets: private key material is tracked:"
  printf '  %s\n' "$by_name"
  fail=1
fi

# 2. Key material by content, for a file whose name gives nothing away.
#    -I skips binary files; the header is the same in every PEM encoding.
by_content=$(git grep -I -l -E '^-----BEGIN [A-Z ]*PRIVATE KEY-----' -- . ':!scripts/check-no-secrets.sh' 2>/dev/null || true)
if [ -n "$by_content" ]; then
  echo "check-no-secrets: a tracked file carries a PEM private key header:"
  printf '  %s\n' "$by_content"
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  cat <<'EOF'

Remove it from the index and rotate the credential:

  git rm --cached <file>

Rotating is not optional. Anything pushed to a public remote is disclosed
permanently -- revoke the key at the provider rather than assuming a later
deletion undid it.
EOF
  exit 1
fi

echo "check-no-secrets: no private key material tracked ✔"
