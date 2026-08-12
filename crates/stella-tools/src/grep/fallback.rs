//! The `grep` tool's POSIX backend — what runs when `rg` is not installed,
//! which is the normal case in benchmark and CI containers rather than an
//! exotic one.
//!
//! It exists as its own module because the interesting part is not "shell to
//! `grep`" but **the dialect boundary**. The tool advertises one `pattern`
//! field; ripgrep executes it as a Rust `regex`, and this backend executes it
//! as POSIX ERE. Those two languages agree on most of what an agent writes and
//! disagree silently on the rest, and a silent disagreement in a *search* tool
//! is the worst possible shape: the caller is told the text does not exist
//! (#2982). So this module does two things the previous inline arm did not:
//!
//! 1. Passes `-E`. POSIX `grep` defaults to **BRE**, where `|`, `(`, `)`, `+`,
//!    `?` and `{n,m}` are literal characters — so every alternation the model
//!    wrote matched nothing. Measured over the 38 `grep` calls of a 40-trial
//!    bench arm: 28 of 28 patterns containing an ERE metacharacter answered
//!    `(no matches)`, against 9 of 10 non-empty without one. Zero exceptions.
//! 2. Refuses, with a named error, any pattern using syntax that ERE cannot
//!    execute **faithfully** — `\d`, `\w`, `\b`, lazy `+?`, `(?i)` and
//!    friends. `-E` alone would only move the lie one layer down: `\d` is not
//!    ERE, so a portable `grep -E` reads it as a literal `d` and answers
//!    `(no matches)` again. An honest failure naming the construct is
//!    recoverable in one turn; a false negative is not recoverable at all.
//!
//! The refusal is deliberately dialect-conservative rather than
//! implementation-sniffing: GNU `grep -E` honours a few of these as
//! extensions and BSD/busybox `grep -E` does not, and this backend cannot know
//! which `grep` is on the PATH. Refusing what is not portable keeps the answer
//! the same everywhere, which is the property the caller actually needs.

use std::path::Path;

use tokio::process::Command;

use stella_protocol::tool::ToolOutput;

use super::{
    FALLBACK_SEARCH_TIMEOUT_SECS, MAX_RESULTS, OutputMode, SearchOptions, cap_columns,
    code_map_for, drop_zero_counts, first_error_line, is_pattern_error, no_matches,
};

/// Appended to this backend's empty answer, so a zero-match result from the
/// degraded backend is distinguishable from a zero-match result from `rg`
/// (#2982). It rides only the empty answer because that is where the whole
/// hazard lives — a *non*-empty result is self-evidently a real one, and a
/// per-search banner on every hit would be noise the model pays for.
const BACKEND_NOTE: &str = "\n\n(searched with POSIX `grep -E`: ripgrep is not installed here, \
     so `type` filters are ignored and the pattern was executed as an ERE.)";

/// A regex construct ripgrep's Rust `regex` engine executes and portable
/// POSIX ERE does not.
///
/// Typed rather than a formatted string because the classification is the
/// decision — the message is a rendering of it, and the tests assert on the
/// variant so a reworded diagnostic cannot quietly change what is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EreGap {
    /// A backslash escape with no portable ERE meaning: `\d`, `\w`, `\s`,
    /// `\b`, `\A`, `\p{…}`, `\x41`, and their negations.
    Escape(char),
    /// A lazy quantifier — `*?`, `+?`, `??`, `{n,m}?`. ERE has no laziness,
    /// and reads `+?` as "one-or-more, then optional", which is a *different
    /// match*, not an error.
    LazyQuantifier,
    /// `(?…)` — a non-capturing group, inline flag, or lookaround.
    GroupSyntax,
}

impl EreGap {
    /// How the construct is written, for the diagnostic.
    fn construct(self) -> String {
        match self {
            Self::Escape(c) => format!("`\\{c}`"),
            Self::LazyQuantifier => "a lazy quantifier (`*?`, `+?`, `??`, `{n,m}?`)".to_string(),
            Self::GroupSyntax => "`(?…)` group syntax".to_string(),
        }
    }

    /// The ERE spelling that answers the same question, so the refusal costs
    /// the caller one rewrite rather than a re-plan.
    fn alternative(self) -> &'static str {
        match self {
            Self::Escape('d') => "use `[[:digit:]]`",
            Self::Escape('D') => "use `[^[:digit:]]`",
            Self::Escape('w') => "use `[[:alnum:]_]`",
            Self::Escape('W') => "use `[^[:alnum:]_]`",
            Self::Escape('s') => "use `[[:space:]]`",
            Self::Escape('S') => "use `[^[:space:]]`",
            Self::Escape('b' | 'B') => {
                "word boundaries have no portable ERE spelling — bound the match with an \
                 explicit character class instead"
            }
            Self::Escape('A' | 'z' | 'Z') => "use `^` and `$`",
            Self::Escape('p' | 'P') => {
                "use a POSIX class such as `[[:alpha:]]`, or a literal range"
            }
            Self::Escape('x') => "write the character literally",
            Self::Escape(_) => "there is no portable ERE equivalent — write the text literally",
            Self::LazyQuantifier => {
                "ERE is always greedy — narrow the match with a negated class such as `[^\"]*`"
            }
            Self::GroupSyntax => {
                "use a plain `(…)` group; for case-insensitivity pass `case_insensitive: true`"
            }
        }
    }
}

/// Which escapes Rust's `regex` gives a meaning that portable ERE does not.
///
/// `\b` and `\w` are on the list even though GNU `grep` happens to honour
/// them: BSD `grep` (macOS) and busybox do not, so a pattern using them would
/// answer differently depending on the host — the exact divergence this
/// module exists to close.
fn escape_gap(c: char) -> Option<EreGap> {
    matches!(
        c,
        'd' | 'D' | 'w' | 'W' | 's' | 'S' | 'b' | 'B' | 'A' | 'z' | 'Z' | 'p' | 'P' | 'x'
    )
    .then_some(EreGap::Escape(c))
}

/// The first construct in `pattern` that this backend cannot execute
/// faithfully, or `None` when the whole pattern is portable ERE.
///
/// Scans rather than pattern-matches on substrings, because the answer depends
/// on context that a substring search does not have: `\\d` is an escaped
/// backslash followed by a literal `d` (portable), and `[+?]` is a bracket
/// expression containing two literal characters, not a lazy quantifier.
pub(super) fn ere_gap(pattern: &str) -> Option<EreGap> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    // Inside `[…]`, every metacharacter is literal — except a backslash
    // escape, which Rust's engine still interprets there.
    let mut class_open: Option<usize> = None;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            // A trailing lone backslash is grep's own diagnostic to give, not
            // a dialect gap — stop scanning rather than classify it here.
            let &next = chars.get(i + 1)?;
            if let Some(gap) = escape_gap(next) {
                return Some(gap);
            }
            i += 2;
            continue;
        }
        if let Some(first) = class_open {
            // A `]` in the first position of a class is a literal `]`.
            if c == ']' && i > first {
                class_open = None;
            }
            i += 1;
            continue;
        }
        match c {
            '[' => {
                // `[^]abc]` — the `^` does not count as the first position.
                let first = if chars.get(i + 1) == Some(&'^') {
                    i + 2
                } else {
                    i + 1
                };
                class_open = Some(first);
                i = first;
                continue;
            }
            '(' if chars.get(i + 1) == Some(&'?') => return Some(EreGap::GroupSyntax),
            '*' | '+' | '?' | '}' if chars.get(i + 1) == Some(&'?') => {
                return Some(EreGap::LazyQuantifier);
            }
            _ => i += 1,
        }
    }
    None
}

/// One search, already parsed and root-confined by the tool,
/// about to run against the POSIX backend.
pub(super) struct Fallback<'a> {
    pub(super) pattern: &'a str,
    pub(super) glob_filter: Option<&'a str>,
    pub(super) opts: SearchOptions,
    pub(super) search_dir: &'a Path,
    pub(super) root: &'a Path,
    pub(super) footer: bool,
    pub(super) show_tip: bool,
}

impl Fallback<'_> {
    /// The `grep` invocation for this search. Split out from [`Self::run`] so
    /// the argument vector is inspectable without spawning anything, the same
    /// shape `glob.rs` uses for its `fd`/`find` builders.
    fn command(&self) -> Command {
        let mut grep = Command::new("grep");
        // `-E` is load-bearing, not tidiness: without it POSIX `grep` runs
        // **BRE**, where `|`, `(`, `)`, `+` and `{n,m}` are literal characters,
        // so every alternation the model writes answers `(no matches)` and the
        // agent reads that false negative as a fact (#2982).
        grep.arg("-r").arg("-E").arg("-n").arg("--color=never");
        // Parity with the rg arm's `--glob !.git/`.
        grep.arg("--exclude-dir=.git");
        if let Some(g) = self.glob_filter {
            grep.arg("--include").arg(g);
        }
        // `--type` has no `grep` equivalent — there is no file-type database
        // to consult — so it is one of the two fields this backend cannot
        // honour. The schema says so rather than letting the two backends
        // disagree silently, which is the trap the hidden-file divergence
        // already sprang once. The regex dialect is the other, and
        // [`ere_gap`] refuses rather than merely documenting it, because a
        // dropped `type` filter over-answers where a mis-read escape
        // under-answers.
        if self.opts.case_insensitive {
            grep.arg("-i");
        }
        match self.opts.mode {
            OutputMode::Content => {
                // POSIX-portable spellings: BSD grep (macOS) has the short
                // forms but not the GNU long ones.
                if self.opts.before > 0 {
                    grep.arg("-B").arg(self.opts.before.to_string());
                }
                if self.opts.after > 0 {
                    grep.arg("-A").arg(self.opts.after.to_string());
                }
            }
            OutputMode::FilesWithMatches => {
                grep.arg("-l");
            }
            // `grep -c` counts matching LINES, not matches — it has no
            // `--count-matches`. Documented here rather than papered over: the
            // fallback's count can undercount a line with two hits, and
            // inventing a second counting pass in Rust would make the two
            // backends disagree in a different way.
            OutputMode::Count => {
                grep.arg("-c");
            }
        }
        // `-e <pattern>` for the same leading-`-` safety as the rg arm.
        grep.arg("-e").arg(self.pattern).arg(self.search_dir);
        crate::subprocess_env::scrub_sensitive_env(&mut grep);
        grep.stdout(std::process::Stdio::piped());
        grep.stderr(std::process::Stdio::piped());
        grep
    }

    /// Run the search, or say why it could not be run.
    pub(super) async fn run(&self) -> ToolOutput {
        if let Some(gap) = ere_gap(self.pattern) {
            return ToolOutput::Error {
                message: format!(
                    "grep: ripgrep is not installed, so this search runs POSIX `grep -E`, which \
                     cannot execute {} in pattern `{}`. Rewrite it in POSIX ERE — alternation \
                     `|`, groups, `+`, `?` and `{{n,m}}` all work — and search again ({}).",
                    gap.construct(),
                    crate::exec::truncate_preview(self.pattern, 120),
                    gap.alternative(),
                ),
            };
        }
        // Same cancellation backstop as the rg arm: `run_captured` sets
        // `kill_on_drop`, so a cancelled turn does not leave a recursive walk
        // burning IO, and bounds the wait so a wedged walk cannot hold the
        // turn open.
        match crate::exec::run_captured(self.command(), FALLBACK_SEARCH_TIMEOUT_SECS).await {
            crate::exec::Captured::TimedOut => ToolOutput::Error {
                message: format!(
                    "grep timed out after {FALLBACK_SEARCH_TIMEOUT_SECS}s — narrow the \
                     search with a `path` or `glob` filter"
                ),
            },
            crate::exec::Captured::Done(output) => {
                // `grep` has no `--max-columns`, so the column cap is applied
                // here; then the same byte-then-line order as the rg arm.
                let text = crate::exec::truncate_middle(cap_columns(&String::from_utf8_lossy(
                    &output.stdout,
                )));
                // Recursive `grep -c` emits a row per file WALKED, including
                // `path:0` for every file that did not match; rg's
                // `--count-matches` emits only files that did. Dropping the
                // zeros is what makes the two backends answer the same
                // question — without it a count search in a large tree is
                // mostly noise, and the `MAX_RESULTS` cap would be spent on
                // zeros.
                let text = if self.opts.mode == OutputMode::Count {
                    drop_zero_counts(&text)
                } else {
                    text
                };
                if !text.is_empty() {
                    let lines: Vec<&str> = text.lines().take(MAX_RESULTS).collect();
                    let mut result = lines.join("\n");
                    // Say so when the cap bit, exactly as the rg arm does: a
                    // silently truncated match list reads to the agent as
                    // "there are no other call sites".
                    if lines.len() == MAX_RESULTS {
                        let unit = self.opts.mode.unit(self.opts.has_context());
                        result.push_str(&format!("\n... (showing first {MAX_RESULTS} {unit})"));
                    }
                    if let Some(map) = code_map_for(
                        self.footer,
                        self.show_tip,
                        self.search_dir,
                        self.root,
                        &lines,
                    ) {
                        result.push_str(&map);
                    }
                    return ToolOutput::Ok { content: result };
                }
                let stderr = String::from_utf8_lossy(&output.stderr);
                if output.status.code() == Some(2) && is_pattern_error(&stderr) {
                    return ToolOutput::Error {
                        message: format!("grep error: {}", first_error_line(&stderr)),
                    };
                }
                let mut empty = no_matches(self.footer, self.show_tip, self.root);
                if let ToolOutput::Ok { content } = &mut empty {
                    content.push_str(BACKEND_NOTE);
                }
                empty
            }
            crate::exec::Captured::Unavailable => ToolOutput::Error {
                message: "grep failed: neither `rg` nor `grep` is available".into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Tool;

    /// Run a search through the POSIX backend **specifically**, whatever is on
    /// the PATH. Every test below goes through here rather than
    /// `Grep::execute`, because on a developer machine `rg` is installed and
    /// `Grep::execute` would never reach this arm — a green test there proves
    /// ripgrep works, which was never in question.
    async fn search(dir: &Path, pattern: &str) -> ToolOutput {
        Fallback {
            pattern,
            glob_filter: None,
            opts: SearchOptions::parse(&serde_json::json!({})).expect("defaults parse"),
            search_dir: dir,
            root: dir,
            footer: false,
            show_tip: false,
        }
        .run()
        .await
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("bottle.py"),
            "def _hkey(key):\n    return key\n\nclass HeaderDict(dict):\n    pass\n\nAKIA1234567890123456\nxxxy\n",
        )
        .expect("write");
        dir
    }

    /// The witness for #2982. POSIX `grep` defaults to BRE, where `|` is a
    /// LITERAL character, so before `-E` this exact search — two real symbols
    /// in one file, named in one alternation — answered `(no matches)` and the
    /// agent concluded the code did not exist. Measured at 28 of 28 such
    /// patterns in a 40-trial bench arm.
    #[tokio::test]
    async fn the_posix_backend_matches_an_alternation() {
        let dir = fixture();
        let out = search(dir.path(), "_hkey|class HeaderDict").await;
        let ToolOutput::Ok { content } = out else {
            panic!("expected Ok, got {out:?}");
        };
        assert!(
            content.contains("def _hkey"),
            "the first alternative must match: {content}"
        );
        assert!(
            content.contains("class HeaderDict"),
            "the second alternative must match: {content}"
        );
    }

    /// `+` and `{n}` are literal in BRE for the same reason `|` is, so a fix
    /// that only reached alternation would still lie about these two.
    #[tokio::test]
    async fn the_posix_backend_honours_ere_repetition() {
        let dir = fixture();
        let out = search(dir.path(), "AKIA[A-Z0-9]{16}").await;
        let ToolOutput::Ok { content } = out else {
            panic!("expected Ok for `{{n}}`, got {out:?}");
        };
        assert!(
            content.contains("AKIA1234567890123456"),
            "a counted repetition must match: {content}"
        );

        let out = search(dir.path(), "x+y").await;
        let ToolOutput::Ok { content } = out else {
            panic!("expected Ok for `+`, got {out:?}");
        };
        assert!(
            content.contains("xxxy"),
            "a `+` repetition must match: {content}"
        );
    }

    /// The invariant that actually matters, and the one nothing pinned: which
    /// backend answered must not change the ANSWER. `Grep::execute` takes the
    /// rg arm wherever ripgrep is installed (CI, most developer machines) and
    /// this backend where it is not, so on either host one side of this
    /// comparison is the real ripgrep answer and the other is this module's —
    /// and on a host without rg it degenerates to a self-check rather than to
    /// a false pass, because both sides then run the same code.
    #[tokio::test]
    async fn both_backends_return_the_same_lines_for_an_alternation() {
        let dir = fixture();
        let pattern = "_hkey|class HeaderDict";

        let ToolOutput::Ok { content: mine } = search(dir.path(), pattern).await else {
            panic!("the POSIX backend must answer");
        };
        let ToolOutput::Ok { content: theirs } = super::super::Grep::bare()
            .execute(&serde_json::json!({ "pattern": pattern }), dir.path())
            .await
        else {
            panic!("the default backend must answer");
        };

        // Compare the match text, not the byte stream: the two backends print
        // the same `path:LINE:text` rows, and only this module appends the
        // which-backend-answered note to an EMPTY result.
        let rows = |s: &str| {
            let mut v: Vec<String> = s
                .lines()
                .filter(|l| l.contains(':'))
                .map(|l| l.rsplit('/').next().unwrap_or(l).to_string())
                .collect();
            v.sort();
            v
        };
        assert_eq!(
            rows(&mine),
            rows(&theirs),
            "the backends disagree — mine:\n{mine}\ntheirs:\n{theirs}"
        );
        assert_eq!(rows(&mine).len(), 2, "both alternatives matched: {mine}");
    }

    /// `-E` closes the ERE gap but not the Rust-regex one: portable `grep -E`
    /// reads `\d` as a literal `d`, so honouring the pattern is impossible and
    /// answering `(no matches)` would be the same lie one layer down. The
    /// caller is told what could not be executed and how to spell it instead.
    #[tokio::test]
    async fn a_pattern_the_backend_cannot_execute_is_refused_not_answered() {
        let dir = fixture();
        let out = search(dir.path(), "AKIA\\d{16}").await;
        let ToolOutput::Error { message } = out else {
            panic!("a Rust-only escape must be refused, got {out:?}");
        };
        assert!(message.contains("\\d"), "names the construct: {message}");
        assert!(
            message.contains("[[:digit:]]"),
            "names the ERE spelling: {message}"
        );
        assert!(
            !message.contains("no matches"),
            "a refusal is never a zero-match answer: {message}"
        );
    }

    /// A genuinely empty result says which backend answered, so the caller can
    /// tell a zero-match from `rg` apart from a zero-match from the degraded
    /// path.
    #[tokio::test]
    async fn an_empty_result_names_the_backend_that_produced_it() {
        let dir = fixture();
        let out = search(dir.path(), "zzz_not_here_xyz|also_not_here").await;
        let ToolOutput::Ok { content } = out else {
            panic!("expected Ok, got {out:?}");
        };
        assert!(content.contains("(no matches)"), "{content}");
        assert!(content.contains("grep -E"), "{content}");
    }

    #[test]
    fn rust_only_syntax_is_classified_and_portable_ere_is_not() {
        // Refused: Rust-regex escapes, laziness, `(?…)`.
        assert_eq!(ere_gap("\\d+"), Some(EreGap::Escape('d')));
        assert_eq!(ere_gap("foo\\b"), Some(EreGap::Escape('b')));
        assert_eq!(ere_gap("\\p{Greek}"), Some(EreGap::Escape('p')));
        assert_eq!(ere_gap("\"[^\"]+?\""), Some(EreGap::LazyQuantifier));
        assert_eq!(ere_gap("a{2,3}?"), Some(EreGap::LazyQuantifier));
        assert_eq!(ere_gap("(?i)needle"), Some(EreGap::GroupSyntax));

        // Accepted: everything ERE executes the same way rg does.
        assert_eq!(ere_gap("_hkey|_hval|class HeaderDict"), None);
        assert_eq!(ere_gap("AKIA[A-Z0-9]{16}"), None);
        assert_eq!(ere_gap("fn (main|run)\\(\\)"), None);
        assert_eq!(ere_gap("[[:digit:]]+"), None);
        // A bracket expression makes its contents literal — `[+?]` is two
        // characters, not a lazy quantifier, and `[]?]` starts with a literal
        // `]`.
        assert_eq!(ere_gap("[+?]"), None);
        assert_eq!(ere_gap("[]?]"), None);
        assert_eq!(ere_gap("[^]a?]"), None);
        // An escaped backslash is not the start of an escape.
        assert_eq!(ere_gap("C:\\\\dir"), None);
        // A trailing lone backslash is grep's problem to report, not ours.
        assert_eq!(ere_gap("trailing\\"), None);
    }

    /// The argument vector is the whole fix, so it is pinned directly: a
    /// future edit that drops `-E` fails here as well as in the behavioural
    /// witnesses above.
    #[test]
    fn the_invocation_asks_for_extended_regular_expressions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fallback = Fallback {
            pattern: "a|b",
            glob_filter: None,
            opts: SearchOptions::parse(&serde_json::json!({})).expect("defaults parse"),
            search_dir: dir.path(),
            root: dir.path(),
            footer: false,
            show_tip: false,
        };
        let cmd = fallback.command();
        let args: Vec<_> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.iter().any(|a| a == "-E"), "got {args:?}");
    }
}
