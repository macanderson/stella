#!/usr/bin/env python3
"""`stella-research` — the pipeline's research and recall stages, as a plugin.

Standard library only, deliberately: `doc:pipeline-as-plugins` §9 rule 3 is
that "if a plugin CANNOT be written without an SDK, the protocol is too
complicated", and the first *first-party* plugin is the worst possible place
to make an exception. If you are reading this to learn the protocol, the whole
protocol is here.

# The wire

`crates/stella-plugin/src/wire.rs` is the contract; these are its shapes as a
plugin author meets them. The host spawns `[runtime].argv` directly — no shell
— writes one JSON request on stdin and reads the response from stdout:

    {"point": "before_turn", "body": {...BeforeTurnRequest}}
 -> {"point": "before_turn", "body": {...BeforeTurnResponse}}

That is the whole exchange unless the plugin asks the host for something in
between, which is the host-call channel below. Stdin stays open for as long as
the host can answer such a call and is shut down when it cannot, so a plugin
reads its input as documents rather than to end-of-file (`read_json`).

Every table on that wire **denies unknown fields**, the envelope included, so
this program does too, at every level. A field the host does not know, at a
version the host accepts, is a typo, and a message that quietly does nothing is
worse than one that refuses. `protocol_version` rides on every message and the
contract is additive-only.

There is no error variant in `BeforeTurnResponse`. A plugin that cannot answer
*fails* — non-zero exit, one line on stderr — and the host runs the turn
without the contribution: a wrapper that cannot speak has nothing to say, and
that is not the user's fault. So stdout carries a valid response or no response
at all.

# What it contributes, and why it is only ever a message

Everything this plugin returns rides as [`VolatileContext`], whose only exit on
the host side is `into_message` — a *user* message after the byte-stable
system-prompt prefix (invariant 7). That is not politeness: prompt-cache hits
are a feature, and a plugin that could write into the stable prefix would make
itself a per-turn cost regression for every user who installed it.

# What it does not do, and what replaced it

The built-in stage this extracts (`crates/stella-pipeline/src/research.rs` and
`pipeline/research_stage.rs`, deleted with `stella-pipeline` in #3865)
answered triage's questions by fanning out **read-only model sub-agents**.
This plugin cannot: `doc:wrapper-socket` §7 is explicit that the socket hands
a plugin no engine, no provider and no
credential, and the request carries no `[roles]` call it could spend. So it
does deterministic grounding instead of model research — it reports what a
literal scan of the granted workspace actually contains, which is the half of
a research finding that is checkable.

It publishes no signals for a related reason: the `Signal` vocabulary is
closed, and `StageName::Research.publishes()` is empty in the host today, so
there is no signal a research stage may honestly publish.

# Recall: a plugin may ask, never reach

The `recall` stage is the other half of `doc:pipeline-as-plugins` §3's
stella-research row, and it reads the context plane — materialized memories,
episodes, facts, code-graph symbols — which no filesystem root contains. This
plugin does not reach for it. It **asks**, over the host-call channel
`doc:wrapper-socket` §6b adds, and the host performs the retrieval, applies the
gate and returns only what the grant permits:

    host   {"point": "before_turn", "body": {...}}
 -> plugin {"call": "recall", "id": 1, "args": {"goal": "...", "limit": 8}}
 -> host   {"result": 1, "ok": {"frames": [...]}}
 -> plugin {"point": "before_turn", "body": {"context": [...]}}   <- ends it

**What may be asked for is declared in the manifest, never negotiated here.**
`[loop] calls = ["recall"]` is the grant a human read at install and
`LoopGrant::permits_call` is the filter the host applies — the same
authoritative filter an undeclared hook meets. So this program does not ask a
host what it is allowed to do. It asks for the one capability its own manifest
declares, and reads the answer.

Every way that answer can fail to be frames — the host offers no channel at all
(`unavailable`), the manifest declares no such call (`undeclared`), this host
does not implement it (`unsupported`), the per-point allowance is spent
(`allowance-spent`), the host tried and failed (`failed`), or nobody answered —
degrades to the empty contribution rather than a fabricated one, and says so on
stderr. That is §6b's third bound taken from the plugin's side: a refused call
is *delivered* to the plugin instead of killing it, and a degradation nobody
can see is the silence this project exists to refuse.

One consequence of a conversation that a single exchange did not have: a
refusal raised *after* a call has been made leaves that call on stdout. The
host reading it is by definition the host that answered it, and the rule holds
in the form that matters — stdout carries no *response* a refusing plugin did
not mean.

The three shapes it speaks are the host's own, not a reading of the design:
`crates/stella-plugin/src/host_call.rs` holds `HostCallRequest`,
`HostCallResponse` and `RecallFrame`, and the harness beside these vectors
decodes this program's calls and encodes its answers with exactly those types,
so a divergence is a failing test rather than a silent refusal in the field.

# Every capability arrives in the request

`candidate` is a `CandidateGrant`: the handle, the canonical workspace `root`,
and the test the host would run there (which this plugin ignores — it runs
nothing). `calls` is the other grant, and it is the same idea pointed at a
capability the plugin cannot hold at all: it names what this program may ask
the host to do, and how often. This program reaches for nothing outside those
two: no environment, no working directory, no git checkout, no terminal. That
is what lets it run unchanged under `stella-cli`, under `stella-serve`, and
inside an application that embedded the loop. With no grant there is nothing to
read and nothing to ask, and a stage with neither contributes nothing.
"""

import json
import os
import re
import sys

# The version every message on this wire carries (`wire::PROTOCOL_VERSION`).
PROTOCOL_VERSION = 1

# The one point this plugin answers, matching `[loop].points`. `WrapperPoint`
# has exactly two, and an undeclared point is never dispatched — but a host
# that asks anyway gets a refusal rather than an answer to a question this
# plugin did not agree to answer.
POINT = "before_turn"

# The two stages it contributes at, and they contribute from different
# sources: `research` reads the granted workspace itself, `recall` asks the
# host for the context plane. Every other declared stage gets an empty
# contribution, which must be byte-identical to the one a host that never
# installed this plugin would have used.
RESEARCH_STAGE = "research"
RECALL_STAGE = "recall"

# The fields each table on the request declares. Anything else is a typo, per
# the deny-unknown-fields rule the whole wire contract is written under.
BEFORE_TURN_REQUEST_FIELDS = {
    "protocol_version",
    "wrapper",
    "stage",
    "round",
    "goal",
    "candidate",
    "published",
}
CANDIDATE_GRANT_FIELDS = {"handle", "root", "test"}
PUBLISHED_SIGNAL_FIELDS = {"signal", "value"}

# ── The host-call channel (`doc:wrapper-socket` §6b) ─────────────────────────
#
# The one call this plugin makes, and the one its manifest declares. `HostCall`
# is a closed set — a capability is a value the host enumerates, never a string
# a plugin invents — and `[loop] calls` is where a human consented to this one.
CALL_RECALL = "recall"

# The answer's tables (`host_call::HostCallResponse`), read under the same rule
# as the request. `ok` and `err` are exclusive: the host's own decoder refuses
# an answer carrying both or neither, and so does this one.
CALL_ANSWER_FIELDS = {"result", "ok", "err"}
RECALL_OK_FIELDS = {"frames"}
# What a refusal calls the document it was reading, since by then it is the
# host's answer rather than the host's question.
RECALL_ANSWER = "the recall answer"
# `host_call::RecallFrame` field for field. Deliberately a *view* of the host's
# own frame rather than a copy: the record id, the token cost and the content
# digest are the host's accounting, and a plugin that cannot act on them has no
# business holding them. What is here is what it takes to build context a human
# can trace — the label to attribute it, the kind and source to weigh it, the
# uri to point at it, and the text.
RECALL_FRAME_FIELDS = {"label", "kind", "source", "uri", "content"}

# How many frames this stage asks for — §6b's own example number, and an *ask*:
# the host clamps it against its own ceiling (`DEFAULT_RECALL_FRAMES`), which is
# why there is no character cap here to match the scan's. What this program
# bounds is only what it will render, so an answer past the ask is named rather
# than silently trimmed.
RECALL_FRAME_LIMIT = 8

# The prefix `stella_core::receipts::user_block_kind` recognises a recalled
# context block by (`receipts::RECALL_MARKER`), and the line format under it is
# `receipts::render_recall_line`'s. Both are mirrored on purpose: without the
# marker the host's receipts file these frames as the *person's* words, which
# is the misattribution #3243 D4 removed from the built-in path. That makes
# this program a second producer of a format whose only other producer is in
# Rust; the goldens beside it are what make a drift between the two visible
# instead of silent.
#
# What it deliberately does not mirror is the `[id]` half of that line. A
# `RecallFrame` carries no record id, so this plugin has nothing to join a
# receipt back to — and minting one from the label would be fabricating the one
# field the write→citation loop trusts.
RECALL_MARKER = "[auto-recalled context]"

# `StageName`, closed. A stage the host cannot dispatch is exactly the manifest
# that quietly does nothing, and a stage *name* this plugin does not recognise
# is the same defect arriving over the wire: it would be indistinguishable from
# "some stage that is not research", and this plugin would silently contribute
# nothing to a turn it was installed to ground.
STAGE_NAMES = (
    "triage",
    "recall",
    "research",
    "plan",
    "scope",
    "execute",
    "witness",
    "verify",
    "verdict",
    "reflect",
    "contextwrite",
    "complete",
)

# ── The bounds, and why each one is where it is ──────────────────────────────
#
# The built-in stage bounds its findings by characters — 4,000 per finding and
# 12,000 per turn (`RESEARCH_FINDING_CHARS`, `RESEARCH_PROMPT_BUDGET_CHARS` in
# `crates/stella-pipeline/src/research.rs`), because a model's answer has no
# other shape to bound. A deterministic scan does: the caps below bound the
# contribution *structurally*, which is stronger, because the ceiling is
# reached the same way on every run instead of depending on how chatty a
# sub-agent was. Their product is deliberately inside the built-in's per-turn
# budget: 6 terms x 5 matches x 200 chars is under 7,000 characters, so this
# stage cannot displace the work it is grounding.
#
# Every cap is *visible* when it binds. A bounded findings section that reads
# as the whole story is the failure the built-in's drop markers exist to
# prevent, and it is worse here — absence in a scan reads as proof of absence.
MAX_TERMS = 6
MAX_MATCHES_PER_TERM = 5
MAX_LINE_CHARS = 200
MAX_ORIENTATION_ENTRIES = 40
# Files opened before the scan stops and says so. A backstop for a tree nobody
# expected, not the thing that makes the output small.
MAX_FILES_SCANNED = 2000
# A file larger than this is not grounding, it is a dataset.
MAX_FILE_BYTES = 262_144
# A NUL in the head of a file is the portable "this is not text" test.
BINARY_SNIFF_BYTES = 4096

# Directory names the scan never descends into: build output and dependency
# trees restate the source in generated form, so a match inside one is noise
# that reads exactly like a finding. Hidden entries (`.git`, `.venv`, and the
# rest) are skipped by the leading-dot rule beside this, which is why they are
# not listed here.
SKIP_DIRS = frozenset(
    (
        "target",
        "node_modules",
        "dist",
        "build",
        "vendor",
        "venv",
        "__pycache__",
    )
)

# A token that is only a word has to be long enough and ordinary enough to be
# worth searching for. Anything that looks like an identifier or a path skips
# this list entirely — `id` is a stopword, `id_map` is a symbol.
MIN_TERM_CHARS = 3
MIN_WORD_CHARS = 5
STOPWORDS = frozenset(
    (
        "about",
        "after",
        "again",
        "against",
        "along",
        "already",
        "also",
        "always",
        "another",
        "because",
        "been",
        "before",
        "being",
        "below",
        "between",
        "both",
        "cannot",
        "could",
        "currently",
        "does",
        "doing",
        "done",
        "during",
        "each",
        "either",
        "else",
        "enough",
        "even",
        "ever",
        "every",
        "from",
        "further",
        "hence",
        "here",
        "however",
        "instead",
        "into",
        "itself",
        "just",
        "like",
        "make",
        "makes",
        "many",
        "might",
        "more",
        "most",
        "much",
        "must",
        "need",
        "needs",
        "never",
        "not",
        "nothing",
        "only",
        "other",
        "over",
        "please",
        "rather",
        "really",
        "same",
        "should",
        "since",
        "some",
        "still",
        "such",
        "than",
        "that",
        "their",
        "them",
        "then",
        "there",
        "these",
        "they",
        "thing",
        "things",
        "this",
        "those",
        "through",
        "under",
        "until",
        "upon",
        "very",
        "want",
        "wants",
        "were",
        "what",
        "when",
        "where",
        "whether",
        "which",
        "while",
        "will",
        "with",
        "within",
        "without",
        "would",
        "your",
    )
)

# A token starts with a letter or underscore and may carry the punctuation a
# symbol or a path carries. Trailing punctuation is prose, not part of the
# name, so it is stripped after the match.
TOKEN_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_./:-]*")
TOKEN_TRIM = ".:/-"
# A dotted token is a filename when its tail looks like an extension.
EXTENSION_RE = re.compile(r"^[A-Za-z]{1,5}$")


class Refusal(Exception):
    """The plugin cannot answer. Exits non-zero; the host runs without it."""


def refuse(reason):
    raise Refusal(reason)


def deny_unknown(table, allowed, subject="the request"):
    """Refuse a table carrying a key the contract does not declare.

    One message shape for every table, matching the reference plugins in
    `stella-examples`: a refusal line is part of the contract a conformance
    harness grades, and Rust's `serde` reports the offending key without a
    path to the table it was found in, so neither does this.

    `subject` names which side of the conversation the table came from, and
    that is the only distinction drawn: a request is the host asking, a host
    call's answer is the host replying, and an author reading the refusal needs
    to know which document to go and look at.
    """
    unknown = sorted(set(table) - allowed)
    if unknown:
        refuse(
            "{} denies unknown fields; got {}".format(subject, ", ".join(unknown))
        )
    return table


def read_json(stream):
    """The next JSON document on `stream`, or `None` when there is not one.

    Line-oriented *and* whole-document tolerant, because the two hosts this
    program meets frame differently and both are legitimate. A host that asks
    one question writes the request, a newline, and shuts stdin down
    (`SubprocessWrapper::exchange`); a host that can answer a call keeps stdin
    open and writes one document per line, because it has to be able to send a
    second one. Reading to EOF deadlocks against the second host and reading
    exactly one line truncates a pretty-printed request from the first, so this
    accumulates lines and stops at the first *complete* document.

    A document followed by anything but whitespace is not returned: one message
    is one document, and `json.loads` — what this replaced — refused the same
    trailing bytes.
    """
    decoder = json.JSONDecoder()
    buffered = ""
    while True:
        text = buffered.strip()
        if text:
            try:
                document, end = decoder.raw_decode(text)
            except ValueError:
                pass  # Not yet complete, or never will be. EOF decides which.
            else:
                if not text[end:].strip():
                    return document
        line = stream.readline()
        if not line:
            return None
        buffered += line


def write_json(stream, document):
    """One JSON document, on one line, flushed.

    The **flush** is the load-bearing word: a host answering a call is blocked
    reading this pipe, and a call still sitting in this process's buffer is a
    conversation that ends at the point timeout with neither side at fault. The
    newline is courtesy in both directions — the host ends a message where its
    JSON value ends rather than at a line break (`SubprocessWrapper::converse`),
    and its own writer terminates every answer with one anyway.
    """
    stream.write(json.dumps(document) + "\n")
    stream.flush()


def report(reason):
    """Say on stderr what this stage degraded to, and why.

    Not a refusal — the program still answers, with the honest empty
    contribution. §6b's bound is that a refused or failed host call is
    *delivered* to the plugin so it can degrade honestly rather than being
    killed, and the other half of that bargain is that the degradation is
    reported: a stage that quietly contributed nothing is a fact the plugin's
    author has to be able to see, and the reason is the only thing that tells
    them whether to fix a manifest, a host, or nothing at all.
    """
    sys.stderr.write("stella-research: {}\n".format(reason))


def read_request(stdin):
    """Decode `{"point": ..., "body": ...}` and return the `before_turn` body."""
    envelope = read_json(stdin)
    if not isinstance(envelope, dict):
        refuse("stdin was not a single JSON object")
    if set(envelope) - {"point", "body"}:
        refuse("the request envelope carried a field outside {point, body}")

    point = envelope.get("point")
    if point != POINT:
        refuse(
            "this plugin answers {} only; the host asked for {}".format(
                POINT, json.dumps(point)
            )
        )

    body = envelope.get("body")
    if not isinstance(body, dict):
        refuse("the request envelope carried no object body")
    deny_unknown(body, BEFORE_TURN_REQUEST_FIELDS)

    version = body.get("protocol_version")
    # `isinstance(True, int)` is True in Python, so a JSON `true` would
    # otherwise compare equal to version 1. It does not, and this says so.
    if isinstance(version, bool) or version != PROTOCOL_VERSION:
        refuse(
            "this plugin speaks wrapper protocol_version {}; the host asked for {}".format(
                PROTOCOL_VERSION, json.dumps(version)
            )
        )

    stage = body.get("stage")
    if stage not in STAGE_NAMES:
        refuse(
            "StageName is a closed set {{{}}}; got {}".format(
                ", ".join(STAGE_NAMES), json.dumps(stage)
            )
        )

    for published in body.get("published", []) or []:
        if not isinstance(published, dict):
            refuse("published must be an array of PublishedSignal objects")
        deny_unknown(published, PUBLISHED_SIGNAL_FIELDS)

    goal = body.get("goal")
    if not isinstance(goal, str):
        refuse("the request carried no goal")
    return body


def grant_root(body):
    """The candidate workspace's root, or `None` when the host granted none.

    `CandidateGrant` is the capability: `root` is where this plugin's own reads
    happen, and there is no other path it may use. A stage with no grant has
    nothing to read and contributes nothing — reaching for a working directory
    instead is exactly the ambient authority the socket exists to refuse.
    """
    candidate = body.get("candidate")
    if candidate is None:
        return None
    if not isinstance(candidate, dict):
        refuse("candidate must be a CandidateGrant object")
    deny_unknown(candidate, CANDIDATE_GRANT_FIELDS)
    root = candidate.get("root")
    if not isinstance(root, str) or not root:
        refuse("the candidate grant carried no root")
    return root


class HostCalls:
    """This plugin's end of the host-call conversation (§6b).

    A plugin may ask the host for a capability; it may never reach for one. So
    there is nothing here but "write a question, read its answer". Nothing is
    negotiated: what this plugin may ask for was settled by the manifest a
    human consented to, and how often is a ceiling the host clamps — neither is
    a number this program gets to hold an opinion about.

    The `id` is this plugin's own correlation number and the host echoes it
    back as `result`, so an answer to a different call is not an answer to
    this one.
    """

    def __init__(self, stdin, stdout):
        self.stdin = stdin
        self.stdout = stdout
        self.asked = 0

    def ask(self, call, args):
        """Make one host call and return its `ok` payload, or `None`.

        `None` is every way a call can fail to produce one: refused for any of
        the five closed reasons, answered for a different call, or not answered
        at all. Each degrades the caller to the contribution it would have made
        with no channel, and each is reported — the host records its own half
        (`HostCallGate::refusals`) and this is the plugin's.

        It does **not** branch on which refusal it was. Every one of them
        leaves this stage with no frames, and a `match` whose arms all do the
        same thing is a claim to be handling something. The code is *reported*,
        because that is what the author debugging a silent stage needs.

        What is deliberately not degraded here is a malformed payload, and that
        line is the one this whole wire is written on: the **outcome** of a
        call is a fact about this call, while the **shape** of an answer is a
        fact about the contract — so a table that decodes to something the
        contract does not describe refuses out loud, because a message that
        quietly does nothing is worse than one that refuses.
        """
        self.asked += 1
        call_id = self.asked
        write_json(self.stdout, {"call": call, "id": call_id, "args": args})

        answer = read_json(self.stdin)
        if not isinstance(answer, dict):
            report("the host did not answer the {} call".format(call))
            return None
        # A `true` result would otherwise compare equal to call 1.
        answered = answer.get("result")
        if isinstance(answered, bool) or answered != call_id:
            report(
                "the host answered call {} while this plugin asked {}".format(
                    json.dumps(answered), call_id
                )
            )
            return None
        deny_unknown(answer, CALL_ANSWER_FIELDS, RECALL_ANSWER)
        ok = answer.get("ok")
        failure = answer.get("err")
        # Exactly one, which is what the host's own decoder enforces on the
        # other side of this pipe — an answer carrying both is two claims about
        # what happened, and believing either is a guess.
        if (ok is None) == (failure is None):
            refuse("a host-call answer carries either ok or err, never both")
        if failure is not None:
            report(
                "the host did not serve the {} call: {}".format(call, refusal(failure))
            )
            return None
        if not isinstance(ok, dict):
            refuse("the recall answer's ok must be an object")
        return ok


def refusal(failure):
    """A failed call's `err`, as the one line this plugin logs about it.

    `HostCallFailure` is `{"refusal": <closed code>, "detail": <words>}`, and
    that shape is read leniently *here alone*: an error report is prose about
    something that has already gone wrong, and refusing to parse it would trade
    a degradation the author can read for a refusal they cannot. Anything else
    is echoed verbatim, sorted — JSON key order is not a fact about the
    refusal, and a report that reordered itself between two hosts saying the
    same thing could not be a golden.
    """
    if isinstance(failure, dict):
        code = failure.get("refusal")
        detail = failure.get("detail")
        if isinstance(code, str) and isinstance(detail, str):
            return "{}: {}".format(code, detail)
    return json.dumps(failure, sort_keys=True)


def is_symbolish(token):
    """Whether a token is a symbol or a path rather than an English word.

    Symbols and paths are searched whatever their length; ordinary words have
    to clear [`MIN_WORD_CHARS`] and the stopword list. The test is deliberately
    syntactic — a plugin that tried to *understand* the goal would be doing the
    job the model does, at the point of least evidence.
    """
    if "_" in token or "/" in token or "::" in token:
        return True
    if token != token.lower() and token != token.upper():
        return True  # camelCase or CamelCase
    head, _, tail = token.rpartition(".")
    return bool(head) and bool(EXTENSION_RE.match(tail))


def search_terms(goal):
    """The terms to look for, in the order they will be reported.

    Symbols and paths first, then ordinary words, each in the order the goal
    named them — a stable ranking, because a contribution that reordered itself
    between two runs on the same goal would be a prompt-cache miss with no
    cause a reader could see.
    """
    ranked = {}
    for raw in TOKEN_RE.findall(goal):
        token = raw.strip(TOKEN_TRIM)
        if len(token) < MIN_TERM_CHARS:
            continue
        key = token.lower()
        if key in ranked:
            continue
        symbolish = is_symbolish(token)
        if not symbolish and (key in STOPWORDS or len(token) < MIN_WORD_CHARS):
            continue
        ranked[key] = (0 if symbolish else 1, len(ranked), token)
    return [token for _, _, token in sorted(ranked.values())[:MAX_TERMS]]


def scannable_files(root):
    """Every file the scan will read, workspace-relative, in sorted order.

    Returns `(paths, truncated)`. Deterministic by construction: `os.walk` is
    handed sorted directory and file names at every level, so two runs over the
    same tree visit the same files in the same order — which is what makes a
    golden possible at all.

    Symlinks are skipped rather than followed. The host fences the paths a
    plugin *names* on the way back; a link is how a read would leave the
    granted root without either side saying so.
    """
    paths = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(
            name
            for name in dirnames
            if not name.startswith(".")
            and name not in SKIP_DIRS
            and not os.path.islink(os.path.join(dirpath, name))
        )
        for name in sorted(filenames):
            if name.startswith("."):
                continue
            full = os.path.join(dirpath, name)
            if os.path.islink(full):
                continue
            if len(paths) == MAX_FILES_SCANNED:
                return paths, True
            paths.append(os.path.relpath(full, root).replace(os.sep, "/"))
    return paths, False


def read_text(path):
    """A file's text, or `None` when it is not text this stage should read."""
    try:
        with open(path, "rb") as handle:
            blob = handle.read(MAX_FILE_BYTES + 1)
    except OSError:
        return None
    if len(blob) > MAX_FILE_BYTES:
        return None
    if b"\0" in blob[:BINARY_SNIFF_BYTES]:
        return None
    return blob.decode("utf-8", "replace")


def clamp_line(line):
    """One matched line, trimmed and clamped, with the clamp made visible."""
    line = line.strip()
    if len(line) > MAX_LINE_CHARS:
        return line[:MAX_LINE_CHARS] + " […]"
    return line


def scan(root, terms):
    """Match `terms` against the granted tree.

    Returns `(found, truncated)`, where `found[term]` is
    `(matches, total, files)`: the first [`MAX_MATCHES_PER_TERM`] matches as
    `(path, line number, line)`, the *total* number of matches, and the number
    of distinct files carrying one. The totals are counted past the cap on
    purpose — "5 of 41" and "5 of 5" are different facts, and a reader who
    cannot tell them apart has been told the truncated half was the whole
    story. `files` is counted over every match rather than over the shown ones
    for the same reason.
    """
    matches = {term: [] for term in terms}
    totals = {term: 0 for term in terms}
    files = {term: set() for term in terms}
    needles = [(term, term.lower()) for term in terms]
    paths, truncated = scannable_files(root)
    for relative in paths:
        text = read_text(os.path.join(root, relative))
        if text is None:
            continue
        for number, line in enumerate(text.splitlines(), start=1):
            lowered = line.lower()
            for term, needle in needles:
                if needle in lowered:
                    totals[term] += 1
                    files[term].add(relative)
                    if len(matches[term]) < MAX_MATCHES_PER_TERM:
                        matches[term].append((relative, number, clamp_line(line)))
    found = {
        term: (matches[term], totals[term], len(files[term])) for term in terms
    }
    return found, truncated


def counted(count, singular, plural):
    """`3 matches`, `1 file`. English, spelled out rather than derived, because
    a contribution a person reads should not say "1 matches"."""
    return "{} {}".format(count, singular if count == 1 else plural)


def orientation(root):
    """What the granted root holds at its top level, or `None` if unreadable.

    The first question anyone asks of an unfamiliar workspace, answered without
    a model call. It is also the honest floor of this stage: a turn whose goal
    names nothing findable still learns where it is.
    """
    try:
        entries = sorted(os.listdir(root))
    except OSError:
        return None
    lines = []
    for name in entries:
        if name.startswith("."):
            continue
        suffix = "/" if os.path.isdir(os.path.join(root, name)) else ""
        lines.append("- {}{}".format(name, suffix))
    shown = lines[:MAX_ORIENTATION_ENTRIES]
    if len(lines) > len(shown):
        shown.append(
            "- [… {} not shown …]".format(
                counted(len(lines) - len(shown), "further entry", "further entries")
            )
        )
    if not shown:
        return None
    return {
        "label": "research:workspace",
        "text": "## Research: workspace orientation\n\n"
        "The candidate root holds these top-level entries (hidden entries "
        "omitted):\n"
        + "\n".join(shown)
        # Said every time, because the alternative is a listing that shows a
        # directory beside findings that never looked inside it: generated
        # trees restate the source, and a match in one reads exactly like a
        # finding. Sorted, because a frozenset has no order and a contribution
        # that reordered itself between runs is a cache miss with no cause.
        + "\n\nNot scanned, wherever they appear: hidden entries, symlinks, "
        "and {}.".format(", ".join(sorted(SKIP_DIRS))),
    }


def term_context(term, hits, count, files):
    """One term's findings, as the contribution the model will read."""
    header = (
        "## Research: `{}`\n\n{} in {} under the candidate root, as "
        "workspace-relative paths:".format(
            term,
            counted(count, "match", "matches"),
            counted(files, "file", "files"),
        )
    )
    lines = ["- {}:{}: {}".format(path, number, line) for path, number, line in hits]
    if count > len(hits):
        lines.append(
            "[… {} not shown; this stage reports the first {} …]".format(
                counted(count - len(hits), "further match", "further matches"),
                MAX_MATCHES_PER_TERM,
            )
        )
    return {
        "label": "research:{}".format(term),
        "text": header + "\n" + "\n".join(lines),
    }


def unmatched_context(terms):
    """The terms nothing matched, said plainly.

    The built-in stage's own system prompt tells its sub-agents to "say plainly
    when you could not find an answer", and a deterministic scan can say it
    with more authority than a model can: nothing under this root contains this
    string.
    """
    return {
        "label": "research:unmatched",
        "text": "## Research: nothing found\n\n"
        "No occurrences under the candidate root of: {}.".format(
            ", ".join("`{}`".format(term) for term in terms)
        ),
    }


def bounded_context():
    """Said whenever the file cap bound, because a bounded scan must never read
    as the whole story — absence is otherwise indistinguishable from proof of
    absence."""
    return {
        "label": "research:scan-bounded",
        "text": "## Research: the scan was bounded\n\n"
        "This stage read the first {} files under the candidate root and "
        "stopped. Anything reported above is present; nothing above is "
        "evidence that something is absent.".format(MAX_FILES_SCANNED),
    }


def frame_text(frame, field, required):
    """One string field of a recalled frame.

    Absent and `null` are the same answer for an optional field — `RecallFrame`
    carries `uri` as an `Option<String>`, and a host writing `null` and a host
    omitting it are saying the same thing. A value of another type is not a
    missing field, it is a frame this program cannot render, so it refuses.
    """
    value = frame.get(field)
    if value is None:
        if required:
            refuse("a recalled frame carried no {}".format(field))
        return None
    if not isinstance(value, str):
        refuse("a recalled frame's {} must be a string".format(field))
    return value


def distinct_label(label, content):
    """The citation label, when it says something the content does not.

    `RecalledFrame::distinct_label`'s rule, mirrored. Memory and episode nodes
    mint their label FROM their content — verbatim at 80 characters or under,
    its first 79 plus `…` above that — so rendering both ships the same
    sentence twice into a rationed recall budget, which is #2476. It models
    that mint rather than similarity, which is what keeps a label a human
    chose from ever being swallowed for merely resembling its content.
    """
    label = label.strip()
    body = content.strip()
    if not label or label == body:
        return None
    stem = label[:-1] if label.endswith("…") else ""
    if stem and body.startswith(stem):
        return None
    return label


def recall_line(frame):
    """One frame as the `- ...` line `receipts::render_recall_line` writes.

    The label is separated from the body by the em-dash the host's
    `parse_recall_item` splits on, and the source rides last so it lands in the
    body half of that split rather than corrupting the label a receipt records.
    A frame whose label says nothing its content does not renders as its body
    alone.
    """
    content = frame_text(frame, "content", required=True)
    line = "- "
    label = distinct_label(frame_text(frame, "label", required=True), content)
    if label is not None:
        line += "{} — ".format(label)
    line += content.strip()
    source = frame_text(frame, "source", required=True)
    if source:
        line += " ({})".format(source)
    return line


def recalled_frames(host_calls, goal):
    """The frames the host recalled for `goal`; `[]` when it recalled none.

    The plugin does not retrieve. It asks, and the host queries the context
    plane, applies the gate, and returns only what this plugin's grant permits
    — which is what makes a retrieval plugin possible without handing it the
    plane (§6b).
    """
    ok = host_calls.ask(CALL_RECALL, {"goal": goal, "limit": RECALL_FRAME_LIMIT})
    if ok is None:
        return []
    deny_unknown(ok, RECALL_OK_FIELDS, RECALL_ANSWER)
    # Absent is empty: `RecallResult` skips the list when it has nothing in it,
    # and an empty recall is an ordinary answer — nothing was relevant — never
    # an error to handle specially (the `ContextRecallPort` discipline, L-C6).
    frames = ok.get("frames", [])
    if not isinstance(frames, list):
        refuse("the recall answer's frames must be an array")
    # Names here, types where they are rendered (`frame_text`): a field this
    # program never reads is still checked for being *named*, because an
    # unrecognised name is the host and this plugin disagreeing about the
    # contract, while an unreadable value is only ever a frame that cannot be
    # written into a line.
    for frame in frames:
        if not isinstance(frame, dict):
            refuse("a recalled frame must be an object")
        deny_unknown(frame, RECALL_FRAME_FIELDS, RECALL_ANSWER)
    return frames


def recall_context(frames):
    """The recalled frames, as the one contribution this stage makes.

    Assembled the way `pipeline/user_message.rs::assemble_recall_message`
    assembles it, marker included — see `RECALL_MARKER` for why a second
    producer of that format is the right call and what the goldens are for.
    """
    shown = frames[:RECALL_FRAME_LIMIT]
    text = RECALL_MARKER + "\n\nRelevant context:\n"
    for frame in shown:
        text += recall_line(frame) + "\n"
    if len(frames) > len(shown):
        # The host was asked for this many and answered with more, so the
        # bound that binds here is this program's own — and a cap that binds
        # in silence turns "we did not show it" into "it was not recalled".
        text += "[… {} not shown; this stage asked for the first {} …]\n".format(
            counted(len(frames) - len(shown), "further frame", "further frames"),
            RECALL_FRAME_LIMIT,
        )
    return {"label": "recall", "text": text}


def contribute(body, host_calls):
    """The context this stage contributes, which may be nothing.

    Nothing is a complete answer and the common case: a stage that is neither
    `recall` nor `research`, a request with no candidate grant, a root that
    cannot be read, a recall the host would not serve. Every one of those
    returns an empty contribution — byte-identical to the one a host that never
    installed this plugin would have used, which is the advisory contract the
    built-in stages hold too (zero findings, or zero frames, leave the prompt
    exactly as it was).

    The two contributing stages read different things and the split is the
    point: `research` reads the workspace the request granted, `recall` reads
    nothing at all and asks the host instead.
    """
    if body["stage"] == RECALL_STAGE:
        frames = recalled_frames(host_calls, body["goal"])
        return [recall_context(frames)] if frames else []
    if body["stage"] != RESEARCH_STAGE:
        return []
    root = grant_root(body)
    if root is None or not os.path.isdir(root):
        return []

    context = []
    where = orientation(root)
    if where is not None:
        context.append(where)

    terms = search_terms(body["goal"])
    found, truncated = scan(root, terms) if terms else ({}, False)
    unmatched = []
    for term in terms:
        hits, count, files = found[term]
        if count:
            context.append(term_context(term, hits, count, files))
        else:
            unmatched.append(term)
    if unmatched:
        context.append(unmatched_context(unmatched))
    if truncated:
        context.append(bounded_context())
    return context


def main():
    try:
        body = read_request(sys.stdin)
        context = contribute(body, HostCalls(sys.stdin, sys.stdout))
    except Refusal as refusal:
        sys.stderr.write("stella-research: {}\n".format(refusal))
        return 1

    response = {"protocol_version": PROTOCOL_VERSION}
    if context:
        # Absent rather than empty when there is nothing to say: the host's own
        # `BeforeTurnResponse` skips empty collections when it serializes, so
        # an empty contribution is the same bytes on both sides of the wire.
        response["context"] = context
    write_json(sys.stdout, {"point": POINT, "body": response})
    return 0


if __name__ == "__main__":
    sys.exit(main())
