#!/usr/bin/env bash
#
# Guard: the brand's lowercase rule is enforced, not just documented (#1500).
#
# docs/brand/brand-guidelines.html is normative: the wordmark is "stella" —
# lowercase, always; never "Stella" or "STELLA". website/README.md restates
# it, and website/content/docs is the surface where a violation matters most:
# `description:` frontmatter becomes each page's meta description and its
# llms.txt entry, so a capitalised name there is the first thing search
# engines and model crawlers read. Before this guard the tree carried 389
# capitalised uses against 1806 lowercase — a documented rule with no gate
# behind it is an unenforced rule.
#
# Prose-aware, because the rule is about the *name in prose*, never code:
#
#   - fenced code blocks (``` / ~~~) are skipped whole — sample output, Rust
#     paths, and config excerpts are quotations, not brand copy;
#   - inline code spans (`...`) are stripped before matching, so a genuine
#     identifier is exempted by writing it as code, which the docs style
#     wants anyway;
#   - URLs are stripped — a path segment is an address, not prose;
#   - compound identifiers (StellaError, StellaHome) never match: the word
#     boundary is part of the rule, which covers the standalone name only.
#
# Portable POSIX awk/find, no toolchain — runs beside the other early guards.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

docs_dir="website/content/docs"
if [ ! -d "$docs_dir" ]; then
    echo "check-brand-case: $docs_dir is missing — the docs tree moved; update this guard" >&2
    exit 1
fi

violations="$(
    find "$docs_dir" -name '*.mdx' -print | sort | while IFS= read -r file; do
        awk -v file="$file" '
            /^[[:space:]]*(```|~~~)/ { fence = !fence; next }
            fence { next }
            {
                line = $0
                gsub(/`[^`]*`/, "", line)
                gsub(/https?:\/\/[^[:space:])]+/, "", line)
                rest = line
                while (match(rest, /[A-Za-z0-9_]+/)) {
                    word = substr(rest, RSTART, RLENGTH)
                    if (tolower(word) == "stella" && word != "stella") {
                        printf "%s:%d: %s\n", file, FNR, $0
                        break
                    }
                    rest = substr(rest, RSTART + RLENGTH)
                }
            }
        ' "$file"
    done
)"

if [ -n "$violations" ]; then
    count="$(printf '%s\n' "$violations" | wc -l | tr -d ' ')"
    {
        echo "check-brand-case: $count line(s) spell the wordmark capitalised in docs prose:"
        printf '%s\n' "$violations"
        echo
        echo "The wordmark is lowercase, always: \"stella\", never \"Stella\" or \"STELLA\""
        echo "(docs/brand/brand-guidelines.html). Fix the prose — or, for a genuine code"
        echo "identifier, write it as an inline code span: fenced blocks and \`...\` are exempt."
    } >&2
    exit 1
fi

# The verdict is already decided; the write is best-effort. SIGPIPE is ignored
# and the write's failure discarded, so a reader that closed the pipe
# (`| head -1`, `| true`) cannot turn a green verdict into a failure (#1815).
trap '' PIPE
echo "check-brand-case: docs prose spells the wordmark lowercase" || true
