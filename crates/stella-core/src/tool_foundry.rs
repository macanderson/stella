//! Tool Foundry — the capability-gap detector: pure synchronous analysis of
//! recent shell invocations that proposes a reusable, parameterized tool when
//! the same shell *shape* keeps getting reconstructed by hand.
//!
//! This is the first slice of self-improvement issue #830 ("stella authors,
//! tests, and installs its own tools at runtime"): the **gap detector only**,
//! which *proposes* a tool spec and installs nothing. Witness proofs and
//! adoption gating are the later slices, and they landed outside this crate
//! (`stella-tools`' `foundry_witness`/`foundry_gate`, driven by
//! `stella tools --adopt` / `--enable`). The slice between them — turning a
//! proposal into a staged manifest+script pair — was written, never called,
//! and retired in #3629; staging is a hand step, and this detector's shipped
//! consumer remains the offline replay report.
//!
//! It is a sibling of [`crate::loop_detect`] and follows the same discipline:
//! plain synchronous functions over owned data, no I/O, no provider SDK, no
//! `regex` — easy to property-test against fakes. The caller hands a window
//! of recorded shell commands in as [`ShellInvocation`]s — today the
//! trace-replay harness (`stella-cli`'s `memory::replay`) is the shipped
//! feeder — and surfaces any [`ProposedTool`] it detects. This module never
//! reads the store and never writes a manifest.
//!
//! **How it differs from loop detection.** [`crate::loop_detect`] flags a
//! *stuck* turn — the same call, byte-identical, producing byte-identical
//! output, over and over — so the driver can abort early. The foundry looks
//! for the opposite of stuck: a command shape reused *with variation* often
//! enough that a parameterized tool would replace the hand-reconstruction.
//! The two are deliberately complementary — an exact repeat is loop
//! detection's territory, so a foundry proposal requires **at least two
//! distinct argument sets** (see [`GapDetectionConfig::min_distinct_arguments`]).
//!
//! **Normalization is the whole game.** "Near-identical" is decided by
//! reducing each command to a *signature*: a skeleton of the invariant parts
//! (program name, subcommands, flag names, shell operators) with the varying
//! *values* (quoted strings, paths, numbers) replaced by typed holes. Two
//! commands with the same signature are the same shape. The holes become the
//! proposed tool's parameters. A small hand-rolled tokenizer does this — a
//! full shell parser is neither available (no `regex` in this crate) nor
//! warranted for a *proposal* whose job is to start a conversation, not to
//! execute.
//!
//! **Precision over recall, on purpose.** A false proposal erodes trust in
//! the inbox, so the detector only templatizes tokens that are *structurally*
//! value-like — quoted, path-like, or numeric. A plain bareword (`git status`,
//! `kubectl get pods`) stays part of the skeleton, so distinct subcommands are
//! never merged. The cost is that a value carried as a plain bareword
//! (`-n my-namespace`) is not recognized as a hole; that is an accepted
//! first-slice limitation, not a bug.
//!
//! **Variation is required, but unbounded variation is not evidence.**
//! [`GapDetectionConfig::min_distinct_arguments`] is a *floor* on variation,
//! and the reason it exists is sound: one argument set is an exact repeat, and
//! that is loop detection's territory. But a floor alone made the two ends of
//! the same argument asymmetric — a shape invoked 1,954 times with 1,938
//! distinct argument sets cleared it a thousand times over while being
//! maximally *un*-reusable. That is not a tool; that is `sed`, and minting a
//! "tool" for it would wrap the shell in the shell. Pointed at a real 81,684-
//! command history, that asymmetry produced 2,073 proposals — an inbox nobody
//! would ever read (#2378).
//!
//! [`GapDetectionConfig::min_reuse_ratio`] is the missing upper half: uses per
//! distinct argument set. A recurring *incantation* — the same command with
//! the same handful of arguments, retyped — scores high; the shell wearing a
//! costume scores 1.0. It is both a gate and the **primary sort key**, so the
//! top of the inbox is the shape a human is most likely to accept.

use std::cmp::Ordering;
use std::collections::BTreeMap;

/// The kind of value a parameter hole stands for. Kept deliberately small:
/// these are the only three token shapes the detector recognizes as
/// structurally "a value that varies," and each renders to one placeholder
/// in a signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParamKind {
    /// A quoted argument (`'.items[]'`, `"error: %s"`) — almost always a
    /// value, whatever its contents.
    Str,
    /// A filesystem-path-shaped argument: contains `/`, starts with `~`, or
    /// carries a short file extension (`a.json`, `src/lib.rs`, `~/notes`).
    /// URLs land here too — they contain `/` — which is fine for a proposal.
    Path,
    /// A numeric argument, including a leading-sign form used as a flag value
    /// (`-5`, `10`, `12.3`).
    Number,
}

impl ParamKind {
    /// The placeholder this kind renders to inside a [`ProposedTool::signature`].
    pub fn placeholder(self) -> &'static str {
        match self {
            ParamKind::Str => "<str>",
            ParamKind::Path => "<path>",
            ParamKind::Number => "<num>",
        }
    }
}

/// One recorded shell invocation the detector mines — the command string and
/// whether it succeeded. Owned so the analysis stays a pure function over data
/// the caller already holds (materialized `tool_calls` rows, or the in-turn
/// event stream); the detector never learns where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellInvocation {
    /// The verbatim command line, as recorded by whatever shell-shaped
    /// surface produced it.
    pub command: String,
    /// Whether the invocation exited successfully. A shape that never once
    /// worked is not a tool worth minting (see
    /// [`GapDetectionConfig::require_success`]).
    pub succeeded: bool,
}

impl ShellInvocation {
    /// Convenience constructor for a successful invocation (the common case in
    /// tests and callers that only kept the command string).
    pub fn ok(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            succeeded: true,
        }
    }
}

/// One parameter of a [`ProposedTool`] — a hole in the command shape, with the
/// concrete values observed at that position as evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolParameter {
    /// Positional name, `p1`, `p2`, … — the order the holes appear in the
    /// command. Mechanical on purpose; a human/author step renames them.
    pub name: String,
    /// What kind of value this position carries.
    pub kind: ParamKind,
    /// A few distinct concrete values seen at this position (capped by
    /// [`GapDetectionConfig::max_examples`]), so the reviewer can see what the
    /// hole actually stood for.
    pub examples: Vec<String>,
}

/// A proposed reusable tool, synthesized from a cluster of near-identical
/// shell invocations. **Nothing is installed** — this is a suggestion carried
/// to the inbox for a human to accept, refine, or dismiss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedTool {
    /// A valid-by-construction candidate tool name (`^[a-z][a-z0-9_]{1,63}$`),
    /// derived mechanically from the skeleton. It does **not** check the
    /// built-in reserved names — this crate cannot depend on `stella-tools`;
    /// the caller is responsible for collision handling before any authorship.
    pub name: String,
    /// A short, human- and model-readable explanation of the gap and what the
    /// tool would replace.
    pub description: String,
    /// The normalized command skeleton with typed placeholders — the cluster
    /// key that made these invocations "the same shape" (`jq <str> <path>`).
    pub signature: String,
    /// The skeleton rendered with numbered `{p1}`-style holes, a sketch of the
    /// command the tool would run (`jq {p1} {p2}`).
    pub command_template: String,
    /// The holes, in command order.
    pub parameters: Vec<ToolParameter>,
    /// Total matching invocations observed (evidence strength).
    pub occurrences: usize,
    /// How many *distinct* argument sets were seen — the proof the shape is
    /// reused with variation rather than stuck-repeated.
    pub distinct_arguments: usize,
    /// A few distinct example command lines (capped by
    /// [`GapDetectionConfig::max_examples`]).
    pub examples: Vec<String>,
}

impl ProposedTool {
    /// Uses per distinct argument set — how *reusable* the shape is, as
    /// opposed to how often it was typed. `1.0` means every invocation carried
    /// arguments never seen before (the shell itself, not a tool); a high
    /// ratio means the same incantation keeps getting reconstructed, which is
    /// the whole reason the foundry exists.
    ///
    /// Always finite: a proposal is only ever built from a cluster with at
    /// least one observation, so `distinct_arguments` is at least `1`. The
    /// guard is there so a hand-built [`ProposedTool`] cannot produce a NaN.
    pub fn reuse_ratio(&self) -> f64 {
        if self.distinct_arguments == 0 {
            return 0.0;
        }
        self.occurrences as f64 / self.distinct_arguments as f64
    }
}

/// Thresholds for [`detect_tool_gaps`].
///
/// **One shipped value, and today no way to change it.** Every production call
/// site constructs `GapDetectionConfig::default()` — today that is the
/// trace-replay harness (`stella-cli`'s `memory::replay`). No settings key
/// reads any field, so what this struct
/// holds is the shipped policy rather than a preference, and the numbers below
/// are the ones every workspace gets. Exposing the two knobs a workspace would
/// actually want to move is #2471.
///
/// It is a parameter rather than a constant because this crate does no I/O
/// (invariant #2): thresholds are data the caller owns, which is what lets the
/// property tests sweep them across ranges no shipped configuration would take.
/// That is the seam a settings surface would attach to — it is not evidence
/// that one exists.
///
/// Not `Eq`: [`GapDetectionConfig::min_reuse_ratio`] is a ratio, and a ratio
/// whose useful range is 1.0–5.0 has nothing to say in integers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GapDetectionConfig {
    /// Matching invocations required before a shape is worth proposing. `0`
    /// or `1` disable detection — a shape used once is not a pattern.
    pub min_occurrences: usize,
    /// Distinct argument sets required. `2` is the decisive default: it is
    /// what separates a *reusable, parameterized shape* (the foundry's job)
    /// from an *exact repeat* (loop detection's job). `min_distinct_arguments`
    /// of `2` also guarantees a proposal always has at least one parameter —
    /// zero-hole commands can only ever produce one distinct (empty) argument
    /// set.
    pub min_distinct_arguments: usize,
    /// Uses per distinct argument set required — the *ceiling* on variation
    /// that [`GapDetectionConfig::min_distinct_arguments`] is the floor of, and
    /// the primary ranking key.
    ///
    /// `occurrences / distinct_arguments`: `1.0` is a shape whose arguments
    /// were never once repeated (`sed -n <str> <path>` seen 1,954 times with
    /// 1,938 argument sets — that is `sed`), while a recurring incantation
    /// scores far higher (`sleep <num> && echo done`, 342 uses across 5
    /// argument sets, 68×). Without it the detector rewards exactly the
    /// commands least worth minting a tool from (#2378).
    ///
    /// A value at or below `1.0` disables the gate — every cluster has at
    /// least one use per argument set by construction — which is how a caller
    /// asks for shape detection alone. A non-finite value (`NaN`, `+∞`)
    /// rejects every proposal rather than admitting one on an unordered
    /// comparison.
    pub min_reuse_ratio: f64,
    /// Require at least one *successful* invocation in the cluster. A shape
    /// that never worked is a different kind of gap (a failing operation), not
    /// a tool to mint from — so the first slice skips it by default.
    pub require_success: bool,
    /// Cap on example command lines per proposal and example values per
    /// parameter, so a hot shape can't produce an unbounded inbox item.
    pub max_examples: usize,
}

impl Default for GapDetectionConfig {
    /// Three occurrences across at least two distinct argument sets, each
    /// argument set carrying its weight three times over, at least one
    /// invocation of which succeeded — enough to rule out coincidence and to
    /// prove the shape is genuinely *reused* with variation. `3` mirrors the
    /// "same incantation reconstructed three times" framing of the driving
    /// issue and [`crate::loop_detect::LoopDetectionConfig`]'s own
    /// rule-of-thumb, and it is the same `3` on both axes for the same reason.
    ///
    /// Taken together the floor is six invocations (three uses each of two
    /// argument sets) — deliberately higher than the three the occurrence
    /// threshold alone admitted, because the evidence a proposal needs is that
    /// the *arguments* recurred, not just the program name.
    fn default() -> Self {
        Self {
            min_occurrences: 3,
            min_distinct_arguments: 2,
            min_reuse_ratio: 3.0,
            require_success: true,
            max_examples: 3,
        }
    }
}

/// Inspect a window of recent shell invocations and propose a reusable tool
/// for every command shape that recurs with variation.
///
/// `invocations` is a recent window of `bash` commands in any order — the
/// caller decides how much history to hand in. The result is deterministic:
/// proposals are sorted by **reuse ratio** first (uses per distinct argument
/// set — the evidence that the shape is an incantation rather than the shell),
/// then by raw occurrences and distinct argument count, both descending, and
/// finally by name, so the same history always yields byte-identical output.
/// Never panics on any input — empty history, unparseable commands, and a
/// zeroed-out `config` all yield an empty `Vec`.
pub fn detect_tool_gaps(
    invocations: &[ShellInvocation],
    config: GapDetectionConfig,
) -> Vec<ProposedTool> {
    if config.min_occurrences < 2 {
        // A shape seen fewer than twice cannot be a pattern; `0`/`1` disable.
        return Vec::new();
    }

    // Cluster by signature. BTreeMap keeps iteration order stable so the final
    // sort is a total order even when two clusters tie on every metric.
    let mut clusters: BTreeMap<String, Cluster> = BTreeMap::new();
    for inv in invocations {
        let Some(analyzed) = analyze(&inv.command) else {
            continue; // empty/whitespace-only command — nothing to mine.
        };
        let entry = clusters
            .entry(analyzed.signature.clone())
            .or_insert_with(|| Cluster::new(analyzed.literals.clone(), analyzed.holes.len()));
        entry.observe(inv, &analyzed, config.max_examples);
    }

    let mut proposals: Vec<ProposedTool> = clusters
        .into_iter()
        .filter_map(|(signature, cluster)| cluster.into_proposal(signature, config))
        .collect();

    // Most reusable first — a shape typed 1,954 times with 1,938 different
    // argument sets is worse evidence than one typed 46 times with 2, so raw
    // occurrences is a tie-break, never the lead. Name closes the order.
    proposals.sort_by(|a, b| {
        reuse_cmp(b, a)
            .then(b.occurrences.cmp(&a.occurrences))
            .then(b.distinct_arguments.cmp(&a.distinct_arguments))
            .then(a.name.cmp(&b.name))
    });
    proposals
}

/// Order two proposals by reuse ratio *without dividing*: with both
/// denominators positive, `a.occ / a.distinct` versus `b.occ / b.distinct` is
/// exactly `a.occ * b.distinct` versus `b.occ * a.distinct`. `u128` makes the
/// cross product unoverflowable for any history that fits in memory, and the
/// comparison stays a total order — which a float ratio, and its `partial_cmp`,
/// would not guarantee.
fn reuse_cmp(a: &ProposedTool, b: &ProposedTool) -> Ordering {
    (a.occurrences as u128 * b.distinct_arguments as u128)
        .cmp(&(b.occurrences as u128 * a.distinct_arguments as u128))
}

/// Does a cluster clear the reuse floor — `occurrences / distinct_arguments >=
/// min_reuse_ratio`? Tested as a multiplication so a zero denominator is
/// impossible and no division rounds. A `NaN` or `+∞` floor makes the
/// comparison false and so rejects everything, which is the safe direction for
/// a knob nobody should be setting to either.
fn meets_reuse_ratio(occurrences: usize, distinct_arguments: usize, floor: f64) -> bool {
    occurrences as f64 >= floor * distinct_arguments as f64
}

/// A signature and the raw material to build a proposal from it: the invariant
/// skeleton words (for naming) and the per-position holes.
struct Analyzed {
    signature: String,
    /// Skeleton words worth naming from (program, subcommands, flag names) —
    /// operators and holes excluded.
    literals: Vec<String>,
    /// One entry per hole, in command order: its kind and concrete value.
    holes: Vec<Hole>,
}

#[derive(Clone)]
struct Hole {
    kind: ParamKind,
    value: String,
}

/// Accumulated evidence for one command shape.
struct Cluster {
    literals: Vec<String>,
    hole_count: usize,
    occurrences: usize,
    successes: usize,
    /// Distinct argument tuples seen (each tuple is the ordered hole values),
    /// mapped to nothing — a set, kept as a map for `BTreeMap` determinism.
    distinct_args: BTreeMap<Vec<String>, ()>,
    /// Per-position distinct example values and their kind, in order.
    params: Vec<ParamAccum>,
    /// Distinct example command lines.
    examples: Vec<String>,
}

struct ParamAccum {
    kind: ParamKind,
    examples: Vec<String>,
}

impl Cluster {
    fn new(literals: Vec<String>, hole_count: usize) -> Self {
        Self {
            literals,
            hole_count,
            occurrences: 0,
            successes: 0,
            distinct_args: BTreeMap::new(),
            params: Vec::with_capacity(hole_count),
            examples: Vec::new(),
        }
    }

    fn observe(&mut self, inv: &ShellInvocation, analyzed: &Analyzed, max_examples: usize) {
        self.occurrences += 1;
        if inv.succeeded {
            self.successes += 1;
        }
        let arg_tuple: Vec<String> = analyzed.holes.iter().map(|h| h.value.clone()).collect();
        self.distinct_args.entry(arg_tuple).or_default();

        for (i, hole) in analyzed.holes.iter().enumerate() {
            if self.params.len() <= i {
                self.params.push(ParamAccum {
                    kind: hole.kind,
                    examples: Vec::new(),
                });
            }
            push_distinct_capped(&mut self.params[i].examples, &hole.value, max_examples);
        }
        push_distinct_capped(&mut self.examples, &inv.command, max_examples);
    }

    fn into_proposal(self, signature: String, config: GapDetectionConfig) -> Option<ProposedTool> {
        let distinct_arguments = self.distinct_args.len();
        if self.occurrences < config.min_occurrences
            || distinct_arguments < config.min_distinct_arguments
            || !meets_reuse_ratio(self.occurrences, distinct_arguments, config.min_reuse_ratio)
            || self.hole_count == 0
            || (config.require_success && self.successes == 0)
        {
            return None;
        }

        let name = synthesize_name(&self.literals);
        let parameters: Vec<ToolParameter> = self
            .params
            .into_iter()
            .enumerate()
            .map(|(i, p)| ToolParameter {
                name: format!("p{}", i + 1),
                kind: p.kind,
                examples: p.examples,
            })
            .collect();
        let command_template = render_template(&signature);
        let description = format!(
            "You ran this shell shape {} times across {} distinct argument sets \
             ({:.1}× reuse). A reusable `{}` tool taking {} parameter(s) would replace \
             the hand-built `{}`. Proposed only — not installed.",
            self.occurrences,
            distinct_arguments,
            self.occurrences as f64 / distinct_arguments as f64,
            name,
            parameters.len(),
            signature,
        );

        Some(ProposedTool {
            name,
            description,
            signature,
            command_template,
            parameters,
            occurrences: self.occurrences,
            distinct_arguments,
            examples: self.examples,
        })
    }
}

/// Push `value` onto `into` if absent, up to `cap` entries — preserves
/// first-seen order and never grows without bound.
fn push_distinct_capped(into: &mut Vec<String>, value: &str, cap: usize) {
    if into.len() >= cap || into.iter().any(|v| v == value) {
        return;
    }
    into.push(value.to_string());
}

/// A lexed shell token, before value/skeleton classification.
struct RawToken {
    text: String,
    quoted: bool,
    operator: bool,
}

/// Split a command line into raw tokens: whitespace separates, single/double
/// quotes group (the inner text is the token, marked `quoted`), and a maximal
/// run of shell-operator characters is one operator token. Not a real shell
/// parser — good enough to recognize command shape without a `regex` dep.
fn tokenize(command: &str) -> Vec<RawToken> {
    let mut tokens = Vec::new();
    let mut chars = command.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '\'' || c == '"' {
            chars.next(); // opening quote
            let mut text = String::new();
            for qc in chars.by_ref() {
                if qc == c {
                    break; // closing quote (unterminated just takes the rest)
                }
                text.push(qc);
            }
            tokens.push(RawToken {
                text,
                quoted: true,
                operator: false,
            });
            continue;
        }
        if is_operator_char(c) {
            let mut text = String::new();
            while let Some(&oc) = chars.peek() {
                if is_operator_char(oc) {
                    text.push(oc);
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(RawToken {
                text,
                quoted: false,
                operator: true,
            });
            continue;
        }
        // A bareword: up to the next whitespace, operator, or quote.
        let mut text = String::new();
        while let Some(&wc) = chars.peek() {
            if wc.is_whitespace() || is_operator_char(wc) || wc == '\'' || wc == '"' {
                break;
            }
            text.push(wc);
            chars.next();
        }
        tokens.push(RawToken {
            text,
            quoted: false,
            operator: false,
        });
    }
    tokens
}

fn is_operator_char(c: char) -> bool {
    matches!(c, '|' | '&' | ';' | '>' | '<')
}

/// An operator that ends one command and starts another, so the next bareword
/// is a fresh command head (a program name to keep literally). Redirections
/// (`>`, `>>`, `<`) do *not* — the token after them is a value (the target).
fn is_command_separator(op: &str) -> bool {
    matches!(op, "|" | "||" | "&&" | ";" | "&" | "|&")
}

/// Reduce a command to its signature, skeleton words, and holes. Returns
/// `None` for an empty/whitespace-only command.
fn analyze(command: &str) -> Option<Analyzed> {
    let raws = tokenize(command);
    if raws.is_empty() {
        return None;
    }

    let mut signature_parts: Vec<String> = Vec::new();
    let mut literals: Vec<String> = Vec::new();
    let mut holes: Vec<Hole> = Vec::new();
    // The first non-operator token is a command head; so is the first after
    // any command-separating operator.
    let mut expect_head = true;

    for raw in &raws {
        if raw.operator {
            signature_parts.push(raw.text.clone());
            expect_head = is_command_separator(&raw.text);
            continue;
        }

        if expect_head {
            // Program / command head — always literal, even if quoted.
            signature_parts.push(raw.text.clone());
            literals.push(raw.text.clone());
            expect_head = false;
            continue;
        }

        classify_argument(raw, &mut signature_parts, &mut literals, &mut holes);
    }

    if signature_parts.is_empty() {
        return None;
    }
    Some(Analyzed {
        signature: signature_parts.join(" "),
        literals,
        holes,
    })
}

/// Classify a non-head argument token into skeleton text and/or a hole,
/// appending to the running signature, skeleton words, and holes.
fn classify_argument(
    raw: &RawToken,
    signature_parts: &mut Vec<String>,
    literals: &mut Vec<String>,
    holes: &mut Vec<Hole>,
) {
    // A quoted argument is a value, whatever it contains.
    if raw.quoted {
        push_hole(ParamKind::Str, &raw.text, None, signature_parts, holes);
        return;
    }

    let word = &raw.text;

    // A flag: `-v`, `--oneline`, `-5` (numeric), or `--depth=5` (with value).
    if word.starts_with('-') {
        if is_numberish(word) {
            // A leading-sign numeric flag value like `-5` in `head -5`.
            push_hole(ParamKind::Number, word, None, signature_parts, holes);
            return;
        }
        if let Some((flag, value)) = split_once_eq(word) {
            let prefix = format!("{flag}=");
            let kind = value_kind(value, false);
            push_hole(kind, value, Some(&prefix), signature_parts, holes);
            return;
        }
        // A plain flag is part of the skeleton.
        signature_parts.push(word.clone());
        literals.push(word.clone());
        return;
    }

    // A `NAME=value` assignment (env var, `KEY=production`): keep the name as
    // skeleton, the value as a hole.
    if let Some((name, value)) = split_once_eq(word)
        && !name.is_empty()
    {
        let prefix = format!("{name}=");
        let kind = value_kind(value, false);
        push_hole(kind, value, Some(&prefix), signature_parts, holes);
        return;
    }

    // A plain positional argument: a hole only if it is structurally value-like
    // (path or number); otherwise a bareword that stays in the skeleton.
    match value_kind_positional(word) {
        Some(kind) => push_hole(kind, word, None, signature_parts, holes),
        None => {
            signature_parts.push(word.clone());
            literals.push(word.clone());
        }
    }
}

/// Append a hole to the signature (as `prefix<placeholder>`) and record its
/// value. `prefix` handles `--flag=` / `NAME=` forms so the flag name stays in
/// the skeleton without a stray space.
fn push_hole(
    kind: ParamKind,
    value: &str,
    prefix: Option<&str>,
    signature_parts: &mut Vec<String>,
    holes: &mut Vec<Hole>,
) {
    let placeholder = kind.placeholder();
    match prefix {
        Some(p) => signature_parts.push(format!("{p}{placeholder}")),
        None => signature_parts.push(placeholder.to_string()),
    }
    holes.push(Hole {
        kind,
        value: value.to_string(),
    });
}

/// Split a word on its first `=`, returning `(before, after)`. `None` when
/// there is no `=`.
fn split_once_eq(word: &str) -> Option<(&str, &str)> {
    word.split_once('=')
}

/// Classify a positional (unquoted, non-flag) value: a hole kind if it is
/// structurally value-like, else `None` (keep it in the skeleton).
fn value_kind_positional(word: &str) -> Option<ParamKind> {
    if is_numberish(word) {
        return Some(ParamKind::Number);
    }
    if looks_like_path(word) {
        return Some(ParamKind::Path);
    }
    None
}

/// Classify a value that is *known* to be a value (the right-hand side of a
/// `=`, where a plain word is still a value, not skeleton). `quoted` forces
/// [`ParamKind::Str`].
fn value_kind(value: &str, quoted: bool) -> ParamKind {
    if quoted {
        return ParamKind::Str;
    }
    if is_numberish(value) {
        return ParamKind::Number;
    }
    if looks_like_path(value) {
        return ParamKind::Path;
    }
    ParamKind::Str
}

/// Is this a number — optionally signed, with at most one decimal point?
/// `-5`, `+3`, `10`, `12.3` are numbers; `-`, `1a`, `1.2.3` are not.
fn is_numberish(word: &str) -> bool {
    let body = word.strip_prefix(['-', '+']).unwrap_or(word);
    if body.is_empty() {
        return false;
    }
    let mut seen_dot = false;
    for c in body.chars() {
        if c == '.' {
            if seen_dot {
                return false;
            }
            seen_dot = true;
        } else if !c.is_ascii_digit() {
            return false;
        }
    }
    // A lone "." (body == ".") is not a number.
    body.chars().any(|c| c.is_ascii_digit())
}

/// Does this look like a filesystem path or URL: contains `/`, starts with
/// `~`, or carries a short trailing file extension (`a.json`, `x.tar.gz`)?
fn looks_like_path(word: &str) -> bool {
    if word.contains('/') || word.starts_with('~') {
        return true;
    }
    has_file_extension(word)
}

/// A trailing `.ext` where `ext` is 1..=8 ASCII-alphanumeric chars and the dot
/// is not the first character (so `.gitignore` and `..` are not "extensions").
fn has_file_extension(word: &str) -> bool {
    let Some(dot) = word.rfind('.') else {
        return false;
    };
    if dot == 0 {
        return false;
    }
    let ext = &word[dot + 1..];
    !ext.is_empty() && ext.len() <= 8 && ext.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Render a signature into a numbered command template: each `<...>` hole
/// becomes `{pN}` in order, so `jq <str> <path>` → `jq {p1} {p2}`.
fn render_template(signature: &str) -> String {
    let mut out = String::with_capacity(signature.len());
    let mut n = 0usize;
    let mut rest = signature;
    while let Some(open) = rest.find('<') {
        if let Some(close_rel) = rest[open..].find('>') {
            let close = open + close_rel;
            out.push_str(&rest[..open]);
            n += 1;
            out.push_str(&format!("{{p{n}}}"));
            rest = &rest[close + 1..];
            continue;
        }
        break;
    }
    out.push_str(rest);
    out
}

/// Build a valid tool name (`^[a-z][a-z0-9_]{1,63}$`) from the skeleton words:
/// strip flag dashes and trailing `=`, keep `[a-z0-9]`, join the first few
/// with `_`, and guarantee the length and leading-letter rules by
/// construction. Mechanical by design — a downstream author step renames it.
fn synthesize_name(literals: &[String]) -> String {
    let mut words: Vec<String> = Vec::new();
    for lit in literals {
        let trimmed = lit.trim_start_matches('-').trim_end_matches('=');
        let cleaned: String = trimmed
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        if !cleaned.is_empty() {
            words.push(cleaned);
        }
        if words.len() >= 4 {
            break; // enough to name; keep it short.
        }
    }

    let mut name = words.join("_");
    if name.is_empty() {
        name = "tool".to_string();
    }
    // Must start with a lowercase letter.
    if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
        name = format!("t_{name}");
    }
    // Must be 2..=64 chars.
    if name.len() < 2 {
        name = format!("{name}_tool");
    }
    if name.len() > 64 {
        name.truncate(64);
        // Truncation can't break the charset (all chars are already valid) but
        // could leave a trailing `_`; that is still a valid name.
    }
    name
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// A hand-rolled matcher for the tool-name contract, so tests do not need
    /// the `regex` crate this module deliberately avoids.
    fn is_valid_tool_name(name: &str) -> bool {
        let len = name.chars().count();
        if !(2..=64).contains(&len) {
            return false;
        }
        let mut chars = name.chars();
        let first = chars.next().unwrap();
        if !first.is_ascii_lowercase() {
            return false;
        }
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    }

    fn ok(command: &str) -> ShellInvocation {
        ShellInvocation::ok(command)
    }

    /// The shipping config with the reuse gate relaxed to "any reuse at all".
    ///
    /// For the tests that pin **normalization** — which tokens become holes,
    /// which commands cluster to one shape — rather than inbox policy. Three
    /// invocations with three argument sets is the clearest way to *state* a
    /// shape, and by construction it carries exactly one use per argument set,
    /// which the shipping [`GapDetectionConfig::min_reuse_ratio`] rejects for a
    /// reason those tests are not about. The ratio itself is pinned by the
    /// tests that keep the default, immediately below.
    fn shapes_only() -> GapDetectionConfig {
        GapDetectionConfig {
            min_reuse_ratio: 1.0,
            ..GapDetectionConfig::default()
        }
    }

    /// Each command repeated `times`, in a stable interleaved order — a history
    /// where argument sets genuinely recur, which is what the default config
    /// asks for.
    fn each_repeated(commands: &[&str], times: usize) -> Vec<ShellInvocation> {
        (0..times)
            .flat_map(|_| commands.iter().map(|c| ok(c)))
            .collect()
    }

    fn failed(command: &str) -> ShellInvocation {
        ShellInvocation {
            command: command.into(),
            succeeded: false,
        }
    }

    #[test]
    fn empty_history_yields_no_proposals() {
        assert!(detect_tool_gaps(&[], GapDetectionConfig::default()).is_empty());
    }

    #[test]
    fn the_motivating_jq_case_is_proposed() {
        // The issue's own example: `extract_json_path`-shaped hand-work — three
        // extractions, each reconstructed by hand three times over. The repeats
        // are the essential part: a *reusable* shape is one whose arguments
        // recur, not merely one whose program name does.
        let history = each_repeated(
            &[
                "jq '.items[0]' a.json",
                "jq '.name' b.json",
                "jq '.error.code' c.json",
            ],
            3,
        );
        let proposals = detect_tool_gaps(&history, GapDetectionConfig::default());
        assert_eq!(proposals.len(), 1, "one shape, one proposal");
        let p = &proposals[0];
        assert_eq!(p.signature, "jq <str> <path>");
        assert_eq!(p.command_template, "jq {p1} {p2}");
        assert_eq!(p.occurrences, 9);
        assert_eq!(p.distinct_arguments, 3);
        assert_eq!(p.reuse_ratio(), 3.0);
        assert_eq!(p.parameters.len(), 2);
        assert_eq!(p.parameters[0].kind, ParamKind::Str);
        assert_eq!(p.parameters[1].kind, ParamKind::Path);
        assert!(is_valid_tool_name(&p.name), "name `{}` invalid", p.name);
        assert_eq!(p.name, "jq");
    }

    #[test]
    fn a_shape_with_no_argument_reuse_is_not_proposed() {
        // `sed -n <str> <path>` — the top proposal on a real 81k-command
        // history, and the whole defect: 1.0× reuse is not a tool, it is `sed`
        // (#2378). Six invocations clear `min_occurrences` twice over and
        // `min_distinct_arguments` three times over, and it is still noise.
        let history: Vec<ShellInvocation> = (0..6)
            .map(|i| ok(&format!("sed -n '{i}p' file{i}.txt")))
            .collect();
        assert!(
            detect_tool_gaps(&history, GapDetectionConfig::default()).is_empty(),
            "a shape with one argument set per use must not reach the inbox"
        );
        // Only the reuse gate rejected it: relax that one knob and the shape is
        // detected exactly as before.
        let proposals = detect_tool_gaps(&history, shapes_only());
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].signature, "sed -n <str> <path>");
        assert_eq!(proposals[0].reuse_ratio(), 1.0);
    }

    #[test]
    fn reuse_outranks_raw_occurrences() {
        // The two shapes from the issue's own measurement, side by side: a
        // high-volume, never-repeating `rg` (12 uses, 12 argument sets) against
        // a retyped incantation (6 uses, 2 argument sets). Occurrences alone
        // would rank the noise first.
        let mut history: Vec<ShellInvocation> = (0..12)
            .map(|i| ok(&format!("rg -n '{i}' src/file{i}.rs")))
            .collect();
        history.extend(each_repeated(
            &["sleep 5 && echo done", "sleep 30 && echo done"],
            3,
        ));

        let ranked = detect_tool_gaps(&history, shapes_only());
        assert_eq!(ranked.len(), 2, "both shapes clear the shape-only bar");
        assert_eq!(
            ranked[0].signature, "sleep <num> && echo done",
            "the 3.0× shape must outrank the 1.0× one despite half the uses"
        );
        assert!(
            ranked[0].occurrences < ranked[1].occurrences,
            "and it must win *while* being the rarer shape, or this proves nothing"
        );

        // Under the shipping default the noisy shape is not merely demoted —
        // it never reaches the inbox at all.
        let shipped = detect_tool_gaps(&history, GapDetectionConfig::default());
        assert_eq!(shipped.len(), 1);
        assert_eq!(shipped[0].signature, "sleep <num> && echo done");
    }

    #[test]
    fn a_non_finite_reuse_floor_rejects_everything() {
        // Nobody should set this, but a NaN comparison is false in both
        // directions and an unordered floor must fail closed, not open.
        let history = each_repeated(&["jq '.a' a.json", "jq '.b' b.json"], 5);
        for floor in [f64::NAN, f64::INFINITY] {
            let config = GapDetectionConfig {
                min_reuse_ratio: floor,
                ..GapDetectionConfig::default()
            };
            assert!(
                detect_tool_gaps(&history, config).is_empty(),
                "a {floor} floor admitted a proposal"
            );
        }
    }

    #[test]
    fn numeric_flag_values_cluster() {
        // `git log --oneline -N`: the count varies, the shape does not.
        let history = vec![
            ok("git log --oneline -5"),
            ok("git log --oneline -10"),
            ok("git log --oneline -20"),
        ];
        let proposals = detect_tool_gaps(&history, shapes_only());
        assert_eq!(proposals.len(), 1);
        let p = &proposals[0];
        assert_eq!(p.signature, "git log --oneline <num>");
        assert_eq!(p.name, "git_log_oneline");
        assert_eq!(p.parameters.len(), 1);
        assert_eq!(p.parameters[0].kind, ParamKind::Number);
    }

    #[test]
    fn flag_with_value_keeps_flag_in_skeleton() {
        let history = vec![
            ok("curl --retry=3 https://a.test/x"),
            ok("curl --retry=5 https://b.test/y"),
            ok("curl --retry=1 https://c.test/z"),
        ];
        let proposals = detect_tool_gaps(&history, shapes_only());
        assert_eq!(proposals.len(), 1);
        let p = &proposals[0];
        assert_eq!(p.signature, "curl --retry=<num> <path>");
        // The `--retry=` flag stays in the skeleton; only its value is a hole.
        assert_eq!(p.command_template, "curl --retry={p1} {p2}");
    }

    #[test]
    fn a_pipeline_shape_is_preserved() {
        let history = vec![
            ok("jq '.a' one.json | head"),
            ok("jq '.b' two.json | head"),
            ok("jq '.c' three.json | head"),
        ];
        let proposals = detect_tool_gaps(&history, shapes_only());
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].signature, "jq <str> <path> | head");
    }

    #[test]
    fn identical_repeats_are_not_proposed() {
        // Byte-identical every time: distinct_arguments == 1. That is loop
        // detection's territory, not the foundry's — never proposed.
        let history = vec![ok("jq '.x' a.json"); 5];
        assert!(detect_tool_gaps(&history, GapDetectionConfig::default()).is_empty());
    }

    #[test]
    fn a_shape_below_the_occurrence_threshold_is_not_proposed() {
        let history = vec![ok("jq '.x' a.json"), ok("jq '.y' b.json")];
        assert!(detect_tool_gaps(&history, GapDetectionConfig::default()).is_empty());
    }

    #[test]
    fn distinct_subcommands_do_not_merge() {
        // `git log` and `git status` are different shapes — plain barewords stay
        // in the skeleton, so nothing here clusters to three of one shape.
        let history = vec![
            ok("git log --oneline -5"),
            ok("git status --short"),
            ok("git log --oneline -9"),
            ok("git status --short"),
        ];
        let proposals = detect_tool_gaps(&history, GapDetectionConfig::default());
        // `git status --short` is byte-identical twice (distinct == 1); the two
        // `git log` differ but occur only twice. Neither qualifies.
        assert!(
            proposals.is_empty(),
            "unexpected proposals: {:?}",
            proposals.iter().map(|p| &p.signature).collect::<Vec<_>>()
        );
    }

    #[test]
    fn always_failing_shape_is_skipped_by_default() {
        let history = vec![
            failed("weirdcmd --flag a.json"),
            failed("weirdcmd --flag b.json"),
            failed("weirdcmd --flag c.json"),
        ];
        assert!(detect_tool_gaps(&history, shapes_only()).is_empty());
        // …but a caller can opt in to failing shapes.
        let permissive = GapDetectionConfig {
            require_success: false,
            ..shapes_only()
        };
        assert_eq!(detect_tool_gaps(&history, permissive).len(), 1);
    }

    #[test]
    fn one_success_in_the_cluster_is_enough() {
        let history = vec![
            failed("jq '.x' a.json"),
            ok("jq '.y' b.json"),
            failed("jq '.z' c.json"),
        ];
        assert_eq!(detect_tool_gaps(&history, shapes_only()).len(), 1);
    }

    #[test]
    fn examples_are_capped() {
        let history: Vec<ShellInvocation> = (0..20)
            .map(|i| ok(&format!("jq '.f{i}' file{i}.json")))
            .collect();
        let proposals = detect_tool_gaps(&history, shapes_only());
        assert_eq!(proposals.len(), 1);
        let p = &proposals[0];
        assert!(
            p.examples.len() <= 3,
            "examples not capped: {}",
            p.examples.len()
        );
        for param in &p.parameters {
            assert!(param.examples.len() <= 3, "param examples not capped");
        }
        // Evidence counts still reflect the full history.
        assert_eq!(p.occurrences, 20);
        assert_eq!(p.distinct_arguments, 20);
    }

    #[test]
    fn output_is_deterministic_and_sorted_by_evidence() {
        // Two shapes at the same 1.0× reuse, so the occurrence tie-break
        // decides: `jq` seen 4×, `sed` seen 3×. Strongest first.
        let history = vec![
            ok("jq '.a' a.json"),
            ok("sed 's/a/b/' one.txt"),
            ok("jq '.b' b.json"),
            ok("sed 's/c/d/' two.txt"),
            ok("jq '.c' c.json"),
            ok("sed 's/e/f/' three.txt"),
            ok("jq '.d' d.json"),
        ];
        let a = detect_tool_gaps(&history, shapes_only());
        let b = detect_tool_gaps(&history, shapes_only());
        assert_eq!(a, b, "detection must be deterministic");
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].signature, "jq <str> <path>");
        assert!(a[0].occurrences >= a[1].occurrences);
    }

    #[test]
    fn zeroed_config_disables_detection() {
        let history = vec![
            ok("jq '.x' a.json"),
            ok("jq '.y' b.json"),
            ok("jq '.z' c.json"),
        ];
        let disabled = GapDetectionConfig {
            min_occurrences: 0,
            ..GapDetectionConfig::default()
        };
        assert!(detect_tool_gaps(&history, disabled).is_empty());
    }

    #[test]
    fn env_assignment_keeps_var_name_in_skeleton() {
        // Only the RUST_LOG value varies; the rest of the line is identical, so
        // all three cluster to one shape. (Plain barewords like `cargo`/`test`
        // stay in the skeleton — that is what keeps distinct shapes apart, so
        // the trailing words must match for these to cluster.)
        let history = vec![
            ok("env RUST_LOG=debug cargo test"),
            ok("env RUST_LOG=trace cargo test"),
            ok("env RUST_LOG=info cargo test"),
        ];
        let proposals = detect_tool_gaps(&history, shapes_only());
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].signature, "env RUST_LOG=<str> cargo test");
        assert_eq!(proposals[0].parameters.len(), 1);
    }

    #[test]
    fn names_are_always_valid() {
        // A pathological program name that is all punctuation still yields a
        // valid synthesized name.
        let history = vec![
            ok("./weird-script.sh a.json"),
            ok("./weird-script.sh b.json"),
            ok("./weird-script.sh c.json"),
        ];
        let proposals = detect_tool_gaps(&history, shapes_only());
        for p in &proposals {
            assert!(is_valid_tool_name(&p.name), "invalid name: {}", p.name);
        }
    }

    #[test]
    fn tokenizer_handles_unterminated_quote() {
        // Must not panic or loop forever on a dangling quote.
        let history = vec![
            ok("grep 'unterminated a.rs"),
            ok("grep 'still open b.rs"),
            ok("grep 'again c.rs"),
        ];
        let _ = detect_tool_gaps(&history, shapes_only());
    }

    #[test]
    fn dotfiles_and_double_dots_are_not_extensions() {
        assert!(!has_file_extension(".gitignore"));
        assert!(!has_file_extension(".."));
        assert!(has_file_extension("a.json"));
        assert!(has_file_extension("archive.tar.gz"));
    }

    #[test]
    fn numberish_edge_cases() {
        assert!(is_numberish("-5"));
        assert!(is_numberish("10"));
        assert!(is_numberish("12.3"));
        assert!(is_numberish("+3"));
        assert!(!is_numberish("-"));
        assert!(!is_numberish("1a"));
        assert!(!is_numberish("1.2.3"));
        assert!(!is_numberish("."));
    }

    #[test]
    fn render_template_numbers_holes_in_order() {
        assert_eq!(render_template("jq <str> <path>"), "jq {p1} {p2}");
        assert_eq!(
            render_template("git log --oneline <num>"),
            "git log --oneline {p1}"
        );
        assert_eq!(render_template("ls"), "ls");
    }

    /// A small, deliberately overlapping alphabet so property runs actually
    /// exercise clustering rather than almost always generating unique shapes.
    fn arb_invocation() -> impl Strategy<Value = ShellInvocation> {
        (0..4usize, 0..4usize, any::<bool>()).prop_map(|(prog, arg, succeeded)| {
            let progs = ["jq", "sed", "grep", "git log"];
            let args = ["'.x' a.json", "'.y' b.json", "-5", "--flag=1 c.json"];
            ShellInvocation {
                command: format!("{} {}", progs[prog], args[arg]),
                succeeded,
            }
        })
    }

    /// Reuse floors worth exercising: the sane band, plus the two non-finite
    /// values a caller should never pass but the detector must survive.
    fn arb_reuse_floor() -> impl Strategy<Value = f64> {
        prop_oneof![
            8 => 0.0f64..6.0,
            1 => Just(f64::NAN),
            1 => Just(f64::INFINITY),
        ]
    }

    proptest! {
        /// `detect_tool_gaps` never panics, for any history and any config,
        /// including the degenerate zero thresholds and a non-finite reuse
        /// floor.
        #[test]
        fn never_panics(
            history in proptest::collection::vec(arb_invocation(), 0..40),
            min_occurrences in 0usize..6,
            min_distinct_arguments in 0usize..6,
            min_reuse_ratio in arb_reuse_floor(),
            require_success in any::<bool>(),
            max_examples in 0usize..6,
        ) {
            let config = GapDetectionConfig {
                min_occurrences,
                min_distinct_arguments,
                min_reuse_ratio,
                require_success,
                max_examples,
            };
            let _ = detect_tool_gaps(&history, config);
        }

        /// Every proposal is well-formed: a valid name, a non-empty parameter
        /// list, and evidence at or above the configured thresholds — and the
        /// list is ordered most-reusable-first, whatever the history.
        #[test]
        fn proposals_respect_their_contract(
            history in proptest::collection::vec(arb_invocation(), 0..60),
            min_occurrences in 2usize..6,
            min_distinct_arguments in 2usize..6,
            min_reuse_ratio in 0.0f64..4.0,
        ) {
            let config = GapDetectionConfig {
                min_occurrences,
                min_distinct_arguments,
                min_reuse_ratio,
                require_success: true,
                max_examples: 3,
            };
            let proposals = detect_tool_gaps(&history, config);
            for p in &proposals {
                prop_assert!(is_valid_tool_name(&p.name), "invalid name {}", p.name);
                prop_assert!(!p.parameters.is_empty(), "zero-parameter proposal");
                prop_assert!(p.occurrences >= min_occurrences);
                prop_assert!(p.distinct_arguments >= min_distinct_arguments);
                prop_assert!(p.reuse_ratio() >= min_reuse_ratio, "ratio floor breached");
                prop_assert!(p.examples.len() <= 3);
                // Parameter count matches the placeholder count in the template.
                let holes = p.command_template.matches("{p").count();
                prop_assert_eq!(holes, p.parameters.len());
            }
            for pair in proposals.windows(2) {
                prop_assert!(
                    pair[0].reuse_ratio() >= pair[1].reuse_ratio(),
                    "ranking is not most-reusable-first: {} then {}",
                    pair[0].signature,
                    pair[1].signature,
                );
            }
        }

        /// Detection is deterministic: the same history yields the same output.
        #[test]
        fn deterministic(history in proptest::collection::vec(arb_invocation(), 0..40)) {
            let a = detect_tool_gaps(&history, GapDetectionConfig::default());
            let b = detect_tool_gaps(&history, GapDetectionConfig::default());
            prop_assert_eq!(a, b);
        }
    }
}
