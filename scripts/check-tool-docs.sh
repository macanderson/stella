#!/usr/bin/env bash
#
# Guard: the committed per-tool reference still describes the declared tools.
#
# `docs/tools/` is one TOML page per dispatchable tool — name, description,
# input schema, output envelope, the catalog's flags, and an observed example
# payload. Nothing about that survives being written by
# hand: #1435 stranded three prose copies of the god-file list behind a
# generated baseline, and #3029 found docs/prompts/worker.md five contracts
# behind its own code with nothing checking it. Both were quiet, and both read
# as authority the whole time they were wrong.
#
# So the pages are generated from the declarations
# (crates/stella-tools/src/catalog.rs plus each tool's own ToolSchema) and this
# guard is the half that makes committing them worth anything: it re-derives
# them and fails on any difference, naming the file.
#
# The property worth the machinery: a tool added to the catalog without
# regenerating turns the gate red. There is no path where a new tool ships
# undocumented.
#
# The derivation lives in `crates/stella-cli/src/tool_docs.rs`, as a test
# rather than an exporter binary, because that is the only scope from which
# all the schemas are reachable at once — the registry's own schemas plus any
# from CLI-internal layers, and stella-cli has no
# `src/lib.rs` for a binary to link (#3061 tracks the seam that would let this
# become an ordinary exporter).
#
# It needs no network, no API key and no model: every input is either source or
# the committed example fixture, which is why it can sit in `make gate`.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

if [ ! -d docs/tools ]; then
  echo "check-tool-docs: FAIL — docs/tools/ does not exist." >&2
  echo "     Generate it: make tool-docs-update" >&2
  exit 1
fi

# Build noise goes to a buffer and is only shown on failure — a green gate
# should say one line.
if ! log="$(cargo test --quiet -p stella-cli --bin stella tool_docs 2>&1)"; then
  echo "check-tool-docs: FAIL — docs/tools/ no longer matches the declarations." >&2
  echo "" >&2
  printf '%s\n' "$log" >&2
  cat >&2 <<'EOF'

That means one of two things, and only you know which:

  * You changed a tool — added one, renamed a field, narrowed an input schema,
    flipped read_only or speculation_safe. Regenerate and commit the pages with
    the change, so the diff is reviewable:

        make tool-docs-update

    Then read the diff as a contract change, not as noise. The description and
    the input schema are the entire basis on which a model decides to call a
    tool, and read_only is what the engine parallelizes on.

  * You did not mean to. Then the diff above is the bug report: something in
    the tool surface changed shape without anyone deciding it should.

EOF
  exit 1
fi

# The verdict is already decided; the write is best-effort. SIGPIPE is ignored
# and the write's failure discarded, so a reader that closed the pipe
# (`| head -1`, `| true`) cannot turn a green verdict into a failure (#1815).
trap '' PIPE
echo "check-tool-docs: OK — docs/tools/ matches the declarations." || true
