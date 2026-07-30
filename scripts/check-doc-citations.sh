#!/usr/bin/env bash
#
# Guard: every `docs/**.md` path cited from Rust source must resolve, and every
# cited `§N` must name a section that actually exists in that document.
#
# Before this guard, `docs/context-reuse.md` did not exist in the repository at
# all, yet 23 rustdoc comments across five crates cited it as the normative
# contract — including section-level claims like "the query payload is never
# transmitted (docs/context-reuse.md §3.5)". `cargo doc` renders a dangling
# path as ordinary text, so nothing surfaced it. A reader who went looking for
# §3.5 found no file.
#
# The section half is not hypothetical either. When that document was vendored,
# 22 of the 23 citations pointed at §1, §2 or §4 and resolved cleanly, but the
# §3.5 one did not: the document has no §3 at all, and the claim it was making
# is stated in §4. A path-only check would have called that citation fine.
#
# The `§N` check is skipped for any document with no numbered `## N.` headings,
# so prose docs that never adopted the convention are not failed for it.
#
# Uses portable POSIX grep/sed so it runs on a bare CI runner.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "check-doc-citations: not a git repository; skipping."
  exit 0
fi

status=0
checked=0

# Tracked Rust sources only: keeps target/ and vendored build output out, and
# means a file has to be added to the index before it can break the build.
rust_files="$(git ls-files '*.rs' || true)"
if [ -z "$rust_files" ]; then
  echo "check-doc-citations: no tracked .rs files; skipping."
  exit 0
fi

# Only comment lines. A `docs/…md` string inside code is test data or a
# constant, not a claim about the repository — stella-tools/src/tasks.rs builds
# a fixture task whose description is "see docs/parser.md", and
# stella-tools/tests/docs_in_sync.rs holds website paths in consts. Neither is
# a citation, and failing them would make the guard un-landable.
#
# The trailing `([^A-Za-z0-9]|$)` keeps `.md` from matching inside `.mdx`; the
# website's MDX pages are a separate tree with separate paths.
# URLs are stripped from each line rather than the line being dropped. An
# absolute upstream link (stella-cli/src/arena.rs cites CGP's host-trace.md by
# permalink) must not be read as a repo-relative path — but plenty of doc
# comments carry a URL *and* a real citation on the same line, and discarding
# those lines wholesale silently skipped 28 of the 46 citations.
# shellcheck disable=SC2086
hits="$(grep -nE '^[[:space:]]*(///|//!|//|\*)' $rust_files 2>/dev/null \
  | sed 's#https\{0,1\}://[^[:space:]"`)]*##g' \
  | grep -E 'docs/[A-Za-z0-9._/-]+\.md([^A-Za-z0-9]|$)' || true)"

if [ -z "$hits" ]; then
  echo "check-doc-citations: no docs/*.md citations found in Rust source."
  exit 0
fi

while IFS= read -r hit; do
  [ -n "$hit" ] || continue
  file="${hit%%:*}"
  rest="${hit#*:}"
  line="${rest%%:*}"
  text="${rest#*:}"

  # `|| true` on every one of these: under `set -o pipefail` a grep that finds
  # nothing fails the whole substitution and, with `set -e`, kills the script
  # silently — which is exactly what a citation carrying no `§` looks like.
  path="$(printf '%s\n' "$text" \
    | grep -oE 'docs/[A-Za-z0-9._/-]+\.md([^A-Za-z0-9]|$)' \
    | head -n1 | sed 's/[^A-Za-z0-9]$//' || true)"
  [ -n "$path" ] || continue
  checked=$((checked + 1))

  if [ ! -f "$path" ]; then
    echo "FAIL $file:$line cites '$path', which does not exist." >&2
    echo "     Add the document, correct the path, or — if this is an" >&2
    echo "     illustrative example rather than a citation — name a file that" >&2
    echo "     does exist. A comment that points at a phantom path reads as" >&2
    echo "     authority and cannot be checked by anyone." >&2
    status=1
    continue
  fi

  # Section reference on the same line, e.g. "§4", "§3.5", "§1." -> major number.
  section="$(printf '%s\n' "$text" | grep -oE '§[0-9]+' | head -n1 | sed 's/§//' || true)"
  [ -n "$section" ] || continue

  # Only enforce for docs that actually number their sections.
  if ! grep -qE '^## [0-9]+\.' "$path"; then
    continue
  fi

  if ! grep -qE "^## ${section}\." "$path"; then
    echo "FAIL $file:$line cites '$path' §${section}, but that document has no '## ${section}.' section." >&2
    echo "     sections present: $(grep -oE '^## [0-9]+\.' "$path" | sed 's/^## //;s/\.$//' | tr '\n' ' ')" >&2
    status=1
  fi
done <<EOF
$hits
EOF

if [ "$status" -eq 0 ]; then
  echo "check-doc-citations: OK — all $checked docs/*.md citations in Rust source resolve."
fi

# ---------------------------------------------------------------------------
# Second guard: no markdown document may cite another by LINE NUMBER (#561).
#
# `file.md §7` survives an edit; a line number does not. Inserting a paragraph
# above the cited line silently repoints the citation at unrelated prose, and
# both the old and the new target render as ordinary text, so nothing surfaces
# the drift — the citation still reads as authority. That is strictly worse
# than no citation, which is why this is a hard failure rather than a warning.
#
# Not hypothetical: every one of the five citations this guard was written to
# retire had ALREADY drifted by 16-27 lines. `docs/adr/0009` cited
# `context-frame-spec.md:96` for the portable Origin values, which had moved to
# :123 — out of "§3. System model" and into "§4. Common types" entirely.
#
# Scope is markdown-to-markdown on purpose. A `.rs:NN` citation is checked by
# the compiler's own blast radius and moves with the code in the same PR; a
# prose document has neither property.
md_files="$(git ls-files '*.md' || true)"
if [ -n "$md_files" ]; then
  # shellcheck disable=SC2086
  linerefs="$(grep -nE '[A-Za-z0-9._/-]+\.md:[0-9]+' $md_files 2>/dev/null || true)"

  if [ -n "$linerefs" ]; then
    echo "" >&2
    echo "check-doc-citations: FAILED — markdown cited by line number." >&2
    printf '%s\n' "$linerefs" | sed 's/^/  /' >&2
    echo "" >&2
    echo "     Cite the section instead: '§7' for a document with numbered" >&2
    echo "     headings, or the heading itself for one without —" >&2
    echo "     'docs/adr/0001-semantic-taxonomy.md § Open questions'." >&2
    echo "     See docs/README.md. A line number is not a stable address." >&2
    status=1
  else
    echo "check-doc-citations: OK — no markdown document cites another by line number."
  fi
fi

exit "$status"
