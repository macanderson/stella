#!/usr/bin/env bash
#
# Guard: deny.toml's `[licenses] allow` list and
# dependency-review.yml's `allow-licenses` input name the same licenses (#920).
#
# The two gates enforce the same policy at two different points (PR-diff vs.
# whole-lockfile) and there is nothing else keeping them in sync — a license
# added to one and not the other is a silent policy split: either a real
# problem license slips past dependency-review and is only caught later by
# `cargo deny`, or dependency-review rejects something deny.toml already
# permits.
#
# Uses portable POSIX tools so it runs on a bare CI runner (no ripgrep, no
# yq — neither is a dependency of this repo).
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

deny_toml="deny.toml"
workflow=".github/workflows/dependency-review.yml"

for f in "$deny_toml" "$workflow"; do
  if [ ! -f "$f" ]; then
    echo "check-license-allowlist-parity: $f not found; skipping." >&2
    exit 0
  fi
done

# deny.toml: lines between `allow = [` and the closing `]`, one quoted SPDX
# id per line (a trailing `# comment` is allowed and stripped).
deny_list="$(
  awk '/^allow = \[/{flag=1; next} flag && /^\]/{flag=0} flag' "$deny_toml" \
    | sed -E 's/#.*$//; s/^[[:space:]]*"([^"]+)".*/\1/' \
    | sed '/^[[:space:]]*$/d' \
    | sort
)"

if [ -z "$deny_list" ]; then
  echo "check-license-allowlist-parity: could not find 'allow = [' in $deny_toml." >&2
  exit 1
fi

# dependency-review.yml: the (possibly folded, possibly multi-line)
# `allow-licenses:` scalar, comma-separated.
review_list="$(
  awk '
    /^[[:space:]]*allow-licenses:/{flag=1; sub(/^[[:space:]]*allow-licenses:[[:space:]]*>-?[[:space:]]*$/, ""); if (length($0)) print; next}
    flag && /^[[:space:]]{2,}[A-Za-z0-9]/{print; next}
    flag {flag=0}
  ' "$workflow" \
    | tr ',' '\n' \
    | sed -E 's/^[[:space:]]+//; s/[[:space:]]+$//' \
    | sed '/^[[:space:]]*$/d' \
    | sort
)"

if [ -z "$review_list" ]; then
  echo "check-license-allowlist-parity: could not find 'allow-licenses:' in $workflow." >&2
  exit 1
fi

if [ "$deny_list" != "$review_list" ]; then
  echo "check-license-allowlist-parity: $deny_toml and $workflow license allow-lists disagree." >&2
  echo >&2
  echo "diff (deny.toml on the left, dependency-review.yml on the right):" >&2
  diff <(printf '%s\n' "$deny_list") <(printf '%s\n' "$review_list") >&2 || true
  exit 1
fi

echo "check-license-allowlist-parity: OK — deny.toml and dependency-review.yml agree on $(printf '%s\n' "$deny_list" | wc -l | tr -d '[:space:]') licenses."
