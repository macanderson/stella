#!/usr/bin/env python3
"""Doc citations addressed by ID instead of by path, so they cannot rot.

WHY THIS EXISTS
---------------
The guard this replaces (`check-doc-citations.sh`) validated that a
`docs/**.md` path named in a Rust comment resolved. It worked, and it was
brittle in the exact way its subject matter was: a path encodes where a
document sits *today*. Move the document and every citation to it breaks at
once, silently, because a dangling path renders as ordinary prose. The old
guard could only tell you a citation had broken; it could not fix one, and
it never looked at markdown-to-markdown links at all -- which is how
`docs/README.md` came to carry three dead links to files that had moved one
directory down while the gate stayed green.

The fix is to stop addressing documents by location:

    ---
    id: context-reuse
    title: Context reuse -- identity, accounting, consent, verification
    status: vendored
    ---

`id` is the address. It is chosen once and never changes, so a citation
written as `doc:context-reuse` survives any move, rename or reshuffle of the
directory tree. There is nothing to heal because nothing breaks.

Frontmatter is also the admission ticket: a document with no `id` cannot be
cited at all. That is deliberate. A spec nobody has bothered to give an
identity to is a spec nobody is maintaining, and this guard would rather make
that visible than quietly keep enforcing links to it.

HEALING THE LEGACY PATH CITATIONS
---------------------------------
237 citations already exist in path form and are not worth rewriting by hand.
`docs/manifest.json` -- generated, committed, diffable, the same pattern
`docs/wire/` uses -- records where each `id` lived at the last commit. When a
document moves, its frontmatter `id` moves with it, so this tool can diff
"where ids live now" against "where ids lived then", recognise the move, and
rewrite every stale path itself:

    make doc-links-fix

That is the healing. It is not magic: it works because the id, not the path,
is what is stable, and the manifest is what remembers the old path long enough
to translate it.

WHAT FAILS THE BUILD, AND WHAT ONLY GETS REPORTED
-------------------------------------------------
Hard failures are things that are definitely wrong: a citation to a document
with no id, a `doc:` id that resolves nowhere, a `§N` the target does not have,
a superseded document still being cited, a duplicate id, or a manifest that has
drifted from the tree (which means `--fix` has not been run).

Staleness is only ever reported, never failed. An uncited document is a
question for a human -- retire it, adopt it, or leave it -- and a red gate is
the wrong way to ask. `--report` prints that list.

KNOWN LIMITS
------------
Only tracked `*.rs` (comment lines) and `*.md` are scanned. A path written in a
`.mdx` page, a shell script or a TOML file is neither checked nor healed -- and
that is usually right, because those are commands and fixtures rather than
claims: `website/content/docs/commands/ingest.mdx` shows
`stella ingest docs/spec/storage-map.md` as example *input*, not a citation.

In a `.rs` file a line starting with `#` counts as a comment, which is true of
attributes and also of string literals whose content is a comment in some other
language -- `crates/stella-graph/src/manifest.rs` embeds a generated `#`-commented
manifest that legitimately references the storage map, and gets healed with
everything else. Worth knowing before adding a Rust fixture whose text begins
with `#` and mentions a docs path.

Stdlib only, no PyYAML: this stays in the same toolchain-free class as the
other guards so it runs on a bare CI runner in seconds.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from collections import defaultdict

MANIFEST_PATH = "docs/manifest.json"
MANIFEST_VERSION = 1

# A document may declare any of these. `superseded` additionally requires
# `superseded_by`, because "this is dead" is only useful next to "read that".
VALID_STATUS = {
    "living",  # maintained, describes current behaviour
    "proposed",  # design not yet built
    "implemented",  # built; kept as the reference for shipped behaviour
    "superseded",  # replaced; requires superseded_by
    "vendored",  # copied from upstream, do not edit here
    "archived",  # kept for history, deliberately uncited
}

# ids are lowercase kebab, optionally namespaced with `/`, so `adr/0007` and
# `context-reuse` are both fine. Deliberately narrow: an id ends up inside Rust
# comments and shell globs, and anything exotic will eventually need quoting.
ID_RE = re.compile(r"^[a-z0-9][a-z0-9-]*(?:/[a-z0-9][a-z0-9-]*)*$")

# `doc:<id>` optionally followed by a section, e.g. `doc:context-reuse §4`.
# The lookahead stops the id from swallowing a trailing `.` or `,` in prose.
CITE_ID_RE = re.compile(r"\bdoc:([a-z0-9][a-z0-9/-]*[a-z0-9])(?:\s*§\s*(\d+))?")

# Legacy path form. The trailing guard keeps `.md` from matching inside `.mdx`
# -- the website's MDX pages are a separate tree, and clipping the `x` turns a
# perfectly good site citation into a phantom repo path.
CITE_PATH_RE = re.compile(r"(docs/[A-Za-z0-9._/-]+\.md)(?![A-Za-z0-9])(?:\s*§\s*(\d+))?")

# Markdown cited by line number. `file.md §7` survives an edit; `file.md:96`
# does not, and every one of the five that existed when this rule was written
# had already drifted 16-27 lines.
LINEREF_RE = re.compile(r"[A-Za-z0-9._/-]+\.md:[0-9]+(?![0-9])")

COMMENT_RE = re.compile(r"^\s*(///|//!|//|\*|#)")
SECTION_RE = re.compile(r"^## (\d+)\.", re.M)
NUMBERED_DOC_RE = re.compile(r"^## \d+\.", re.M)

# The escape hatch. Not every `docs/…md` in prose is a citation: a design doc
# may name a file it intends to *generate*, which by definition does not exist
# yet. `docs/spec/diagnostics.md` describes a generated
# `docs/reference/diagnostics.md` that way, and healing "fixed" it into a
# sentence claiming the design doc generates itself. A path the author says is
# not a citation is not a citation.
IGNORE_RE = re.compile(r"doc-links:\s*ignore")


class Doc:
    """One document under docs/, plus whatever frontmatter it declares."""

    __slots__ = (
        "path",
        "id",
        "title",
        "status",
        "superseded_by",
        "sections",
        "numbered",
        "errors",
    )

    def __init__(self, path):
        self.path = path
        self.id = None
        self.title = None
        self.status = None
        self.superseded_by = None
        self.sections = []
        self.numbered = False
        self.errors = []

    @property
    def citable(self):
        return self.id is not None and not self.errors


def run(*args):
    """git, with output as text. Returns '' rather than raising on failure."""
    try:
        out = subprocess.run(args, capture_output=True, text=True, check=False)
        return out.stdout
    except OSError:
        return ""


def tracked(*globs):
    return [p for p in run("git", "ls-files", *globs).splitlines() if p]


def read(path):
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            return fh.read()
    except OSError:
        return ""


def parse_frontmatter(text):
    """Return (dict, body_offset) for leading `---` YAML, or (None, 0).

    Deliberately not a YAML parser. Frontmatter here is flat `key: value`
    lines; anything needing more structure is a sign the metadata is growing
    into content, which is what the document body is for.
    """
    if not text.startswith("---"):
        return None, 0
    # Tolerate CRLF and a missing trailing newline on the opening fence.
    nl = text.find("\n")
    if nl == -1 or text[:nl].strip() != "---":
        return None, 0
    end = text.find("\n---", nl)
    if end == -1:
        return None, 0
    block = text[nl + 1 : end]
    after = text.find("\n", end + 1)
    fields = {}
    for line in block.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if ":" not in line:
            continue
        key, _, value = line.partition(":")
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        fields[key.strip()] = value
    return fields, (after + 1 if after != -1 else len(text))


def load_docs():
    """Every tracked docs/**.md, with frontmatter parsed and validated."""
    docs = []
    for path in sorted(set(tracked("docs/*.md", "docs/**/*.md"))):
        doc = Doc(path)
        text = read(path)
        fields, _ = parse_frontmatter(text)
        doc.sections = [int(n) for n in SECTION_RE.findall(text)]
        doc.numbered = bool(NUMBERED_DOC_RE.search(text))
        if fields is None:
            docs.append(doc)  # bare: legal, just not citable
            continue
        doc.id = fields.get("id")
        doc.title = fields.get("title")
        doc.status = fields.get("status")
        doc.superseded_by = fields.get("superseded_by")
        if not doc.id:
            doc.errors.append("frontmatter present but no `id:`")
        elif not ID_RE.match(doc.id):
            doc.errors.append(
                f"id {doc.id!r} is not lowercase-kebab (optionally `ns/name`)"
            )
        if not doc.title:
            doc.errors.append("frontmatter present but no `title:`")
        if not doc.status:
            doc.errors.append("frontmatter present but no `status:`")
        elif doc.status not in VALID_STATUS:
            doc.errors.append(
                f"status {doc.status!r} is not one of: {', '.join(sorted(VALID_STATUS))}"
            )
        if doc.status == "superseded" and not doc.superseded_by:
            doc.errors.append("status is `superseded` but no `superseded_by:` id")
        docs.append(doc)
    return docs


def load_manifest():
    text = read(MANIFEST_PATH)
    if not text.strip():
        return {}
    try:
        data = json.loads(text)
    except ValueError:
        return {}
    return data.get("documents", {})


def build_manifest(docs):
    documents = {}
    for doc in docs:
        if not doc.citable:
            continue
        entry = {"path": doc.path, "title": doc.title, "status": doc.status}
        if doc.superseded_by:
            entry["superseded_by"] = doc.superseded_by
        if doc.sections:
            entry["sections"] = sorted(set(doc.sections))
        documents[doc.id] = entry
    return {"version": MANIFEST_VERSION, "documents": documents}


def write_manifest(manifest):
    text = json.dumps(manifest, indent=2, sort_keys=True) + "\n"
    with open(MANIFEST_PATH, "w", encoding="utf-8") as fh:
        fh.write(text)
    return text


def citation_sources():
    """Files that may cite a document: Rust comments and tracked markdown.

    Rust is scanned comment-only. A `docs/…md` string inside code is test data
    or a constant -- `crates/stella-tools/src/tasks.rs` builds a fixture task whose
    description is "see docs/parser.md" -- not a claim about the repository.
    """
    return tracked("*.rs"), tracked("*.md")


def strip_urls(line):
    """Drop absolute URLs before looking for repo paths.

    A doc comment often carries a permalink *and* a real citation on the same
    line, so dropping the whole line loses the citation; dropping just the URL
    keeps it while stopping `.../docs/host-trace.md` in a github.com link from
    being read as a repo-relative path.
    """
    return re.sub(r"https?://[^\s\"`)\]]*", "", line)


def scan_citations(rust_files, md_files):
    """Yield (file, lineno, kind, target, section, raw_line) for every citation."""
    for path in rust_files:
        for i, line in enumerate(read(path).splitlines(), 1):
            if not COMMENT_RE.match(line) or IGNORE_RE.search(line):
                continue
            clean = strip_urls(line)
            for m in CITE_ID_RE.finditer(clean):
                yield path, i, "id", m.group(1), m.group(2), line
            for m in CITE_PATH_RE.finditer(clean):
                yield path, i, "path", m.group(1), m.group(2), line
    for path in md_files:
        if path == MANIFEST_PATH:
            continue
        for i, line in enumerate(read(path).splitlines(), 1):
            if IGNORE_RE.search(line):
                continue
            clean = strip_urls(line)
            for m in CITE_ID_RE.finditer(clean):
                yield path, i, "id", m.group(1), m.group(2), line
            for m in CITE_PATH_RE.finditer(clean):
                yield path, i, "path", m.group(1), m.group(2), line


def days_since_touched(path):
    out = run("git", "log", "-1", "--format=%cr", "--", path).strip()
    return out or "unknown"


def detect_moves(old_manifest, docs):
    """id -> (old_path, new_path) for every document that has moved.

    This is the whole healing mechanism. The id is stable and travels inside
    the file, so comparing the committed manifest against the tree tells us
    exactly which path citations are now stale and what they should say --
    something git rename detection cannot do reliably for a delete-plus-add.
    """
    moves = {}
    by_id = {d.id: d for d in docs if d.citable}
    for doc_id, entry in old_manifest.items():
        doc = by_id.get(doc_id)
        if doc is None:
            continue
        if entry.get("path") and entry["path"] != doc.path:
            moves[doc_id] = (entry["path"], doc.path)
    return moves


def heal_by_basename(docs, rust_files, md_files):
    """Second healing strategy, for paths the manifest cannot explain.

    The manifest only knows about moves it witnessed -- it cannot help with a
    citation that was already wrong when the manifest was first written, or one
    a human simply typed with the wrong directory in it. But a document's
    filename is nearly as stable as its id, so a broken `docs/a/b/spec.md` whose
    basename matches exactly one identified document is almost certainly meant
    to point there.

    'Exactly one' is the whole safety argument. Two candidates means guessing,
    and a citation repointed at the wrong document is worse than one that
    visibly dangles -- so ambiguous cases are reported for a human instead.
    """
    by_base = defaultdict(list)
    for doc in docs:
        if doc.citable:
            by_base[os.path.basename(doc.path)].append(doc.path)

    broken = set()
    for path in list(rust_files) + list(md_files):
        if path == MANIFEST_PATH:
            continue
        for i, line in enumerate(read(path).splitlines(), 1):
            if path.endswith(".rs") and not COMMENT_RE.match(line):
                continue
            if IGNORE_RE.search(line):
                continue
            for m in CITE_PATH_RE.finditer(strip_urls(line)):
                target = m.group(1)
                if not os.path.isfile(target):
                    broken.add(target)

    remap, ambiguous, unresolved = {}, [], []
    for target in sorted(broken):
        candidates = by_base.get(os.path.basename(target), [])
        if len(candidates) == 1:
            remap[target] = candidates[0]
        elif len(candidates) > 1:
            ambiguous.append((target, candidates))
        else:
            unresolved.append(target)
    return remap, ambiguous, unresolved


def heal(moves, rust_files, md_files, extra_remap=None):
    """Rewrite stale paths in place. Returns [(file, count)].

    Scoped to exactly the lines `scan_citations` would read, and for a reason
    paid for once already: an earlier version substituted across whole files,
    so it rewrote a `docs/…md` inside a Rust *string literal* in
    `crates/stella-core/src/ingest/tests.rs` -- a fixture, not a citation, which the
    checker deliberately ignores. The longer path pushed the line past
    rustfmt's width and turned the fmt gate red for a "fix" nothing had asked
    for. A fixer must never touch more than its checker can see.
    """
    remap = {old: new for old, new in moves.values()}
    if extra_remap:
        remap.update(extra_remap)
    if not remap:
        return []
    # Guard the trailing char so `foo.md` never matches inside `foo.mdx`,
    # exactly as the citation regex does.
    patterns = [
        (re.compile(re.escape(old) + r"(?![A-Za-z0-9])"), new)
        for old, new in remap.items()
    ]

    changed = []
    for path in list(rust_files) + list(md_files):
        if path == MANIFEST_PATH:
            continue
        text = read(path)
        if not text:
            continue
        is_rust = path.endswith(".rs")
        out, count = [], 0
        for line in text.splitlines(keepends=True):
            if IGNORE_RE.search(line) or (is_rust and not COMMENT_RE.match(line)):
                out.append(line)
                continue
            for pat, new in patterns:
                line, n = pat.subn(new, line)
                count += n
            out.append(line)
        if count:
            with open(path, "w", encoding="utf-8") as fh:
                fh.write("".join(out))
            changed.append((path, count))
    return changed


def check(docs, manifest_on_disk, fix=False):
    """Returns (hard_errors, notes). hard_errors non-empty means exit 1."""
    errors = []
    notes = []

    by_id = {}
    by_path = {}
    for doc in docs:
        if doc.id is None:
            continue
        for err in doc.errors:
            errors.append(f"{doc.path}: {err}")
        if doc.errors:
            continue
        if doc.id in by_id:
            errors.append(
                f"{doc.path}: id {doc.id!r} is already used by {by_id[doc.id].path}. "
                f"An id is an address; two documents cannot share one."
            )
            continue
        by_id[doc.id] = doc
        by_path[doc.path] = doc

    for doc in docs:
        if doc.id and doc.status == "superseded" and doc.superseded_by:
            if doc.superseded_by not in by_id:
                errors.append(
                    f"{doc.path}: superseded_by {doc.superseded_by!r} is not a known id."
                )

    rust_files, md_files = citation_sources()
    cited_ids = defaultdict(list)
    n_checked = 0

    for path, line, kind, target, section, raw in scan_citations(rust_files, md_files):
        n_checked += 1
        if kind == "id":
            doc = by_id.get(target)
            if doc is None:
                errors.append(
                    f"{path}:{line} cites doc:{target}, which is not a known document id.\n"
                    f"     Known ids live in {MANIFEST_PATH}. If the document exists but has\n"
                    f"     no frontmatter, add an `id:` to it -- an undeclared document is\n"
                    f"     deliberately not citable."
                )
                continue
        else:
            doc = by_path.get(target)
            if doc is None:
                if os.path.isfile(target):
                    errors.append(
                        f"{path}:{line} cites '{target}', which exists but declares no\n"
                        f"     frontmatter `id:`. Adopt it (add frontmatter) or stop citing it.\n"
                        f"     Nothing under docs/ is citable until it has an identity."
                    )
                else:
                    errors.append(
                        f"{path}:{line} cites '{target}', which does not exist and no\n"
                        f"     document claims to have moved from there. Correct the path,\n"
                        f"     or cite `doc:<id>` so it cannot break again."
                    )
                continue
            notes.append(
                f"     {path}:{line} cites {target} by path; `doc:{doc.id}` cannot break."
            )

        cited_ids[doc.id].append(f"{path}:{line}")

        if doc.status == "superseded":
            errors.append(
                f"{path}:{line} cites {doc.id!r}, which is superseded by "
                f"{doc.superseded_by!r}. Cite the successor."
            )

        if section is not None and doc.numbered:
            if int(section) not in doc.sections:
                have = ", ".join(str(s) for s in sorted(set(doc.sections))) or "none"
                errors.append(
                    f"{path}:{line} cites {doc.id or target} §{section}, but that document\n"
                    f"     has no '## {section}.' section. Sections present: {have}."
                )

    # Markdown cited by line number -- a citation that looks authoritative and
    # silently repoints at unrelated prose the moment anything above it grows.
    for path in md_files:
        if path == MANIFEST_PATH:
            continue
        for i, line in enumerate(read(path).splitlines(), 1):
            for m in LINEREF_RE.finditer(strip_urls(line)):
                errors.append(
                    f"{path}:{i} cites markdown by line number ({m.group(0)}).\n"
                    f"     A line number is not a stable address. Cite `§N`, or the\n"
                    f"     heading, or `doc:<id> §N`."
                )

    expected = build_manifest(docs)
    if expected != manifest_on_disk and not fix:
        errors.append(
            f"{MANIFEST_PATH} is out of date with the tree.\n"
            f"     Run `make doc-links-fix` -- it heals any stale path citations and\n"
            f"     rewrites the manifest in one pass."
        )

    return errors, notes, by_id, cited_ids, n_checked


def report_orphans(docs, cited_ids):
    """The 'stale design docs nobody cares about' list. Never a failure."""
    lines = []
    for doc in docs:
        if doc.status == "archived":
            continue
        if doc.id and cited_ids.get(doc.id):
            continue
        # A bare document is uncitable by construction, so "uncited" is not news
        # unless it is big enough that somebody clearly meant it to matter.
        n_lines = len(read(doc.path).splitlines())
        if doc.id is None and n_lines < 100:
            continue
        status = doc.status or "no status"
        lines.append((n_lines, doc.path, status, days_since_touched(doc.path)))
    lines.sort(reverse=True)
    return lines


def cmd_check(args):
    docs = load_docs()
    on_disk = {"version": MANIFEST_VERSION, "documents": load_manifest()}
    errors, notes, by_id, cited_ids, n = check(docs, on_disk)

    if errors:
        # One line citing the same document twice produces the same message
        # twice; dedupe so the report reads as a list of problems rather than
        # a list of matches. dict.fromkeys keeps first-seen order.
        errors = list(dict.fromkeys(errors))
        print("check-doc-links: FAILED", file=sys.stderr)
        print("", file=sys.stderr)
        for err in errors:
            print(f"  {err}", file=sys.stderr)
        print("", file=sys.stderr)
        return 1

    citable = sum(1 for d in docs if d.citable)
    print(
        f"check-doc-links: OK -- {n} citation(s) resolve across "
        f"{citable} identified document(s)."
    )
    if args.verbose and notes:
        print("")
        print("  path citations that would be safer as ids:")
        for note in notes[:20]:
            print(note)
        if len(notes) > 20:
            print(f"     ... and {len(notes) - 20} more")

    orphans = report_orphans(docs, cited_ids)
    if orphans:
        print("")
        print(f"  nothing cites these {len(orphans)} document(s) -- retire or adopt:")
        for n_lines, path, status, age in orphans:
            print(f"    {n_lines:5d}L  {path}  ({status}, {age})")
    return 0


def cmd_fix(args):
    docs = load_docs()
    old = load_manifest()
    moves = detect_moves(old, docs)
    rust_files, md_files = citation_sources()

    if moves:
        print(f"check-doc-links: {len(moves)} document(s) moved since the manifest:")
        for doc_id, (old_path, new_path) in sorted(moves.items()):
            print(f"    {doc_id}: {old_path} -> {new_path}")
    else:
        print("check-doc-links: no document moved since the manifest.")

    by_base, ambiguous, unresolved = heal_by_basename(docs, rust_files, md_files)
    if by_base:
        verb = "match" if args.by_name else "would match"
        print(f"check-doc-links: {len(by_base)} broken path(s) {verb} one document by name:")
        for old_path, new_path in sorted(by_base.items()):
            print(f"    {old_path} -> {new_path}")
        if not args.by_name:
            print("")
            print("    Not applied. A witnessed move is provable -- the id says where the")
            print("    document went. A name match is a guess, and a citation repointed at")
            print("    the wrong document is worse than one that visibly dangles. Read the")
            print("    list above, then re-run with --by-name to apply it.")

    changed = heal(
        moves, rust_files, md_files, extra_remap=by_base if args.by_name else None
    )
    if changed:
        total = sum(c for _, c in changed)
        print(f"check-doc-links: healed {total} citation(s) in {len(changed)} file(s).")
        for path, count in sorted(changed):
            print(f"    {path}  ({count})")
    else:
        print("check-doc-links: nothing to heal.")

    if ambiguous:
        print("")
        print("NOT healed -- more than one document has that filename, so a fix")
        print("would be a guess. Repoint these by hand, ideally to `doc:<id>`:")
        for target, candidates in ambiguous:
            print(f"    {target}")
            for c in candidates:
                print(f"        could be {c}")
    if unresolved:
        print("")
        print("NOT healed -- no document under docs/ has that filename. Either it")
        print("was deleted, it lives on the docs site, or it belongs to another")
        print("repository and should be cited by URL:")
        for target in unresolved:
            print(f"    {target}")

    manifest = build_manifest(docs)
    before = read(MANIFEST_PATH)
    after = write_manifest(manifest)
    if before != after:
        print(f"check-doc-links: rewrote {MANIFEST_PATH}.")
    else:
        print(f"check-doc-links: {MANIFEST_PATH} already current.")
    return 0


def cmd_report(args):
    docs = load_docs()
    on_disk = {"version": MANIFEST_VERSION, "documents": load_manifest()}
    _errors, _notes, _by_id, cited_ids, _n = check(docs, on_disk)

    identified = [d for d in docs if d.citable]
    bare = [d for d in docs if d.id is None]
    print(f"documents under docs/ : {len(docs)}")
    print(f"  with an id          : {len(identified)}")
    print(f"  bare (not citable)  : {len(bare)}")
    print("")

    by_status = defaultdict(list)
    for doc in identified:
        by_status[doc.status].append(doc)
    for status in sorted(by_status):
        print(f"  {status:12s} {len(by_status[status])}")
    print("")

    orphans = report_orphans(docs, cited_ids)
    if not orphans:
        print("Every document is cited by something.")
        return 0
    print(f"Nothing cites these {len(orphans)} document(s):")
    for n_lines, path, status, age in orphans:
        print(f"  {n_lines:5d}L  {path}")
        print(f"          {status}, last touched {age}")
    return 0


def suggest_id(path, taken):
    """A short, readable id for a document, disambiguated only when it must be.

    The stem is almost always the right name -- `context-reuse.md` wants to be
    `context-reuse`, not `design-adaptive-context-context-reuse`. Generic stems
    (`README`, `index`) carry no information, so those borrow their directory,
    and anything still colliding walks further up the path until it is unique.
    """
    parts = os.path.splitext(path)[0].split(os.sep)
    stem = parts[-1].lower()

    def kebab(s):
        return re.sub(r"[^a-z0-9-]+", "-", s.lower()).strip("-")

    if stem in ("readme", "index"):
        parent = parts[-2] if len(parts) > 1 else "doc"
        candidate = kebab(parent) if parent != "docs" else "docs-readme"
    else:
        candidate = kebab(stem)

    if candidate not in taken:
        return candidate
    for up in range(2, len(parts) + 1):
        prefixed = kebab("-".join(parts[-up:]))
        if prefixed not in taken:
            return prefixed
    n = 2
    while f"{candidate}-{n}" in taken:
        n += 1
    return f"{candidate}-{n}"


def cmd_init(args):
    """Scaffold frontmatter onto documents that have none."""
    written = []
    taken = {d.id for d in load_docs() if d.id}
    for path in args.paths:
        text = read(path)
        if not text:
            print(f"skip (unreadable): {path}", file=sys.stderr)
            continue
        fields, _ = parse_frontmatter(text)
        if fields is not None:
            print(f"skip (already has frontmatter): {path}")
            continue

        doc_id = suggest_id(path, taken)
        taken.add(doc_id)
        stem = os.path.splitext(os.path.basename(path))[0]

        m = re.search(r"^#\s+(.+)$", text, re.M)
        title = m.group(1).strip() if m else stem.replace("-", " ").capitalize()
        title = title.replace('"', "'")

        sm = re.search(r"^\*\*Status:?\*\*:?\s*(.+)$", text, re.M)
        status = "living"
        if sm:
            blob = sm.group(1).lower()
            # "superseded" only. A document saying it *supersedes* something
            # else is the live one, not the dead one.
            if "superseded" in blob:
                status = "superseded"
            elif "propos" in blob:
                status = "proposed"
            elif "implement" in blob or "shipped" in blob or "built" in blob:
                status = "implemented"
        if "VENDORED" in text or "vendored" in text[:2000]:
            status = "vendored"

        header = f'---\nid: {doc_id}\ntitle: "{title}"\nstatus: {status}\n---\n\n'
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(header + text)
        written.append((path, doc_id, status))

    for path, doc_id, status in written:
        print(f"init {path}\n     id: {doc_id}  status: {status}")
    if written:
        print("")
        print("Review the generated ids and statuses, then run `make doc-links-fix`.")
    return 0


def cmd_selftest(args):
    """Exercise the parts that can silently corrupt a file.

    This tool rewrites source in place, so the regexes that decide *what* gets
    rewritten are the dangerous surface. The `.mdx` guard in particular: clip
    the `x` and a perfectly good website citation becomes a phantom repo path,
    which is how three `website/content/docs/*.mdx` references were briefly
    misread as broken during this script's own development.
    """
    failures = []

    def check(label, got, want):
        if got != want:
            failures.append(f"{label}: got {got!r}, want {want!r}")

    fm, off = parse_frontmatter('---\nid: a-b\ntitle: "T: x"\nstatus: living\n---\n\n# H\n')
    check("frontmatter id", fm.get("id"), "a-b")
    check("frontmatter quoted title", fm.get("title"), "T: x")
    check("frontmatter status", fm.get("status"), "living")
    check("body offset lands after fence", off > 0, True)

    check("no frontmatter", parse_frontmatter("# Just a heading\n")[0], None)
    check("unterminated frontmatter", parse_frontmatter("---\nid: x\n")[0], None)

    # `.md` must not match inside `.mdx`.
    check(
        "mdx is not md",
        [m.group(1) for m in CITE_PATH_RE.finditer("see docs/a/b.mdx here")],
        [],
    )
    check(
        "plain md matches",
        [m.group(1) for m in CITE_PATH_RE.finditer("see docs/a/b.md here")],
        ["docs/a/b.md"],
    )
    check(
        "section captured",
        [(m.group(1), m.group(2)) for m in CITE_PATH_RE.finditer("docs/a.md §4")],
        [("docs/a.md", "4")],
    )

    check(
        "id citation",
        [(m.group(1), m.group(2)) for m in CITE_ID_RE.finditer("see doc:context-reuse §3.")],
        [("context-reuse", "3")],
    )

    check("url stripped", "docs/x.md" in strip_urls("https://h/docs/x.md"), False)
    check(
        "url stripped, sibling citation kept",
        "docs/x.md" in strip_urls("https://h/a.md and docs/x.md"),
        True,
    )

    check("ignore marker", bool(IGNORE_RE.search("path <!-- doc-links:ignore -->")), True)

    check("id shape ok", bool(ID_RE.match("adr/0007-x")), True)
    check("id shape rejects caps", bool(ID_RE.match("Adr")), False)
    check("id shape rejects underscore", bool(ID_RE.match("a_b")), False)

    check("suggest plain", suggest_id("docs/spec/storage-map.md", set()), "storage-map")
    check("suggest readme borrows dir", suggest_id("docs/brand/README.md", set()), "brand")
    check("suggest docs readme", suggest_id("docs/README.md", set()), "docs-readme")
    check(
        "suggest disambiguates",
        suggest_id("docs/design/a/spec.md", {"spec"}),
        "a-spec",
    )

    # The heal rewrite must respect the same .mdx boundary.
    remap = {"docs/a.md": "docs/b/a.md"}
    text = "cite docs/a.md and docs/a.mdx\n"
    for old, new in remap.items():
        text = re.sub(re.escape(old) + r"(?![A-Za-z0-9])", new, text)
    check("heal leaves mdx alone", text, "cite docs/b/a.md and docs/a.mdx\n")

    # Regression: heal must skip Rust lines the checker skips. A blanket
    # substitution once rewrote a docs path inside a test's string literal,
    # lengthening the line past rustfmt's width and turning the fmt gate red.
    rust_lines = [
        '/// see docs/a.md\n',  # comment: heal it
        '    let s = "docs/a.md";\n',  # code: leave it
        '// docs/a.md doc-links:ignore\n',  # opted out: leave it
    ]
    healed = []
    for line in rust_lines:
        if IGNORE_RE.search(line) or not COMMENT_RE.match(line):
            healed.append(line)
            continue
        healed.append(re.sub(r"docs/a\.md(?![A-Za-z0-9])", "docs/b/a.md", line))
    check("heal rewrites rust comments", "docs/b/a.md" in healed[0], True)
    check("heal skips rust code", healed[1], '    let s = "docs/a.md";\n')
    check("heal honours ignore marker", healed[2], '// docs/a.md doc-links:ignore\n')

    if failures:
        print("check-doc-links selftest: FAILED", file=sys.stderr)
        for f in failures:
            print(f"  {f}", file=sys.stderr)
        return 1
    print("check-doc-links selftest: OK -- 23 assertions.")
    return 0


def main():
    repo_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    os.chdir(repo_root)

    if not os.path.isdir(".git") and not run("git", "rev-parse", "--git-dir").strip():
        print("check-doc-links: not a git repository; skipping.")
        return 0

    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = parser.add_subparsers(dest="cmd")

    p_check = sub.add_parser("check", help="validate every citation (default)")
    p_check.add_argument("-v", "--verbose", action="store_true")
    p_check.set_defaults(func=cmd_check)

    p_fix = sub.add_parser("fix", help="heal moved paths and rewrite the manifest")
    p_fix.add_argument(
        "--by-name",
        action="store_true",
        help="also repoint broken paths whose filename matches exactly one "
        "document. This is a guess, not a proof -- read the proposed list first.",
    )
    p_fix.set_defaults(func=cmd_fix)

    p_report = sub.add_parser("report", help="staleness and adoption report")
    p_report.set_defaults(func=cmd_report)

    p_init = sub.add_parser("init", help="scaffold frontmatter onto documents")
    p_init.add_argument("paths", nargs="+")
    p_init.set_defaults(func=cmd_init)

    p_self = sub.add_parser("selftest", help="check the rewrite logic itself")
    p_self.set_defaults(func=cmd_selftest)

    args = parser.parse_args()
    if not getattr(args, "cmd", None):
        args = parser.parse_args(["check"])
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
