//! Reading a shell command as *text* — never running one.
//!
//! Three questions live here, and they are one subject: how a command splits
//! into words and separators, what the resulting shape says about whether the
//! command does any work at all, and whether it writes to the filesystem.
//!
//! It sits in `stella-core` rather than beside the shell tool because **two
//! planes ask the same question of the same string**. The `bash` tool asks it
//! per call, to append an advisory to a result
//! (`stella_tools::bash::sleep_advisory`); the engine asks it per turn, over
//! the calls already sitting in the transcript, to decide whether the turn is
//! stalling ([`crate::driver`]'s stall rung) and whether a shell command
//! invalidated a file the already-read digest still names
//! ([`crate::compaction::read_digest`]). `stella-tools` depends on
//! `stella-core`, so one implementation can serve both — and the alternative
//! was a second copy of the operator list, which is the failure this splitter
//! was extracted to end in the first place: four near-copies had accumulated
//! inside `bash.rs` and had already diverged (#2301).
//!
//! Pure functions over borrowed text (invariant 2): no process, no clock, no
//! filesystem. In particular the stall classifier is a *static text-shape*
//! check and never a measured elapsed time, so the same transcript always
//! classifies the same way — a timing here would make loop detection
//! nondeterministic.

/// Words the tokenizer emits as command separators, and the one predicate
/// every consumer of [`shell_words`] asks about them.
///
/// Four near-copies of this list had accumulated in `stella-tools`' `bash.rs`
/// — the `cd` skip list, the grep-scan boundary, `segment_args` and
/// [`bare_sleep_seconds`] — and they had already diverged: the grep boundary
/// omitted `&`, so `ls & grep -rn "struct X" .` scanned past the background
/// operator (#2301).
///
/// A newline is an operator here for the same reason `;` is: it ends one
/// command and begins the next. Redirection tokens (`2>&1`, `>>`, `<`) are
/// deliberately **not** operator words — they neither end a command nor start
/// one, and `stella-tools`' `redirect_target` reads them as ordinary words.
///
/// Neither are the substitution words [`shell_words`] emits (`(`, `)`, `$(`
/// and the backtick). They are word boundaries, not separators: `(`
/// *introduces* a command, and a consumer that treated it as one would read
/// `echo (cd /outside` as a directory change the shell never performs.
pub fn is_operator_word(word: &str) -> bool {
    matches!(word, ";" | "&" | "&&" | "|" | "||" | "\n")
}

/// A quote-aware word split — enough to pull a `cd` target or a `grep`
/// pattern out of the common command shapes, returning each word already
/// unquoted. NOT a shell parser: it respects `'…'` and `"…"` (so a pattern or
/// path with spaces stays one word) and preserves backslash escapes like
/// `\|` (so an alternation survives to whatever inspects the pattern);
/// unquoted operators (`&&`, `||`, `|`, `;`, `&`, and a bare newline) come
/// back as their own words to bound a scan — including when attached to a
/// word, so `cd /app; ls` yields the target `/app`, not the unresolvable
/// `/app;` a paid bench trial saw warned about as an escape from its own
/// session root.
///
/// The subshell parens split for the same reason and are emitted the same
/// way, `$(` as one opener; unlike the operators they are not
/// [`is_operator_word`]s, because `(` starts a command rather than ending
/// one. An unquoted backtick is the other spelling of that opener and splits
/// with them (#4409).
pub fn shell_words(command: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut has_word = false;
    let (mut in_single, mut in_double) = (false, false);
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
                has_word = true;
            }
            '"' if !in_single => {
                in_double = !in_double;
                has_word = true;
            }
            c @ (';' | '&' | '|') if !in_single && !in_double => {
                if has_word {
                    words.push(std::mem::take(&mut cur));
                    has_word = false;
                }
                let mut op = String::from(c);
                // `&&` / `||` are one operator; `;;` never appears in the
                // shapes this scans, so `;` stays single.
                if c != ';' && chars.peek() == Some(&c) {
                    chars.next();
                    op.push(c);
                }
                words.push(op);
            }
            // A paren bounds a command rather than belonging to the word
            // beside it: `(` opens a subshell, `)` closes one and also
            // terminates a `case` pattern. Glued to the adjacent word they hid
            // a real directory change from every consumer that reads command
            // position — `(cd /outside; ls)` yielded `["(cd", …]`, so no word
            // ever equalled `cd` (#3619), while the spaced `( cd /outside )`
            // had been read correctly all along.
            //
            // Each is emitted alone: neither pairs the way `&&` does, so `((`
            // is two words. The one exception is `$(`, which is a single
            // command-substitution opener rather than a `$` word beside a
            // subshell — split in two, the `$` sits between the opener and the
            // command and hides it again.
            '(' | ')' if !in_single && !in_double => {
                let substitution = c == '(' && cur.ends_with('$');
                if substitution {
                    cur.pop();
                    has_word = !cur.is_empty();
                }
                if has_word {
                    words.push(std::mem::take(&mut cur));
                    has_word = false;
                }
                words.push(if substitution {
                    String::from("$(")
                } else {
                    String::from(c)
                });
            }
            // A backtick is the older spelling of `$(`, and the body between
            // a pair of them is a command like any other. Left glued to the
            // word beside it, the whole substitution was a single word —
            // `` `cd /outside` `` split as `["`cd", "/outside`"]`, so no word
            // ever equalled `cd` and every consumer that reads command
            // position was blind to it (#4409), the same defect the parens
            // had before #3619.
            //
            // Both ends emit the same word, because the two are the same
            // character and telling them apart is a parity question belonging
            // to whoever is tracking command position, not to a splitter.
            // Quoted, it is text: a backtick inside `'…'` or `"…"` stays in
            // its word, which keeps this on the module's "better a missed
            // note than a wrong one" side — a double-quoted substitution does
            // run, and is deliberately not seen.
            '`' if !in_single && !in_double => {
                if has_word {
                    words.push(std::mem::take(&mut cur));
                    has_word = false;
                }
                words.push(String::from(c));
            }
            '\\' if !in_single => {
                // Keep the escape literal (covers `\|`, `\"`, …); we don't
                // interpret it, just preserve it for the symbol test.
                cur.push('\\');
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
                has_word = true;
            }
            // A newline separates two commands as surely as `;` does, so it
            // is an operator word rather than plain whitespace. Without it a
            // command-position rule cannot see the second line of
            // `ls\ncd /outside` as a command at all.
            '\n' if !in_single && !in_double => {
                if has_word {
                    words.push(std::mem::take(&mut cur));
                    has_word = false;
                }
                words.push(String::from('\n'));
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if has_word {
                    words.push(std::mem::take(&mut cur));
                    has_word = false;
                }
            }
            c => {
                cur.push(c);
                has_word = true;
            }
        }
    }
    if has_word {
        words.push(cur);
    }
    words
}

/// One `sleep` argument read as whole seconds, or `None` when it is not a
/// duration at all.
///
/// GNU `sleep` takes an optional unit suffix — `s`, `m`, `h`, `d` — and
/// `sleep 10m` asks for exactly the 600 seconds `sleep 600` does. Reading only
/// the bare number made every suffixed form invisible to
/// [`bare_sleep_seconds`], and invisible in the worst direction: the `?` below
/// is a whole-command answer, so `sleep 10m; echo done` did not merely
/// contribute zero, it classified as *not a sleep at all*.
///
/// **Saturating, never wrapping, in both directions.** The argument is
/// model-authored text, so `sleep 99999999999999999999` is runtime data and
/// must not be an arithmetic overflow panic (invariant 5). Rust's float→int
/// cast already saturates, which puts an absurd request on `u64::MAX` and a
/// negative or `NaN` one on `0` — and `sleep` itself rejects both of those,
/// so contributing nothing is the honest reading.
fn sleep_arg_seconds(arg: &str) -> Option<u64> {
    // Byte-wise, so the slice below always lands on a char boundary: the
    // suffixes are ASCII, and a multi-byte final char takes the default arm.
    let (digits, per_unit_secs) = match arg.as_bytes().last()? {
        b's' => (&arg[..arg.len() - 1], 1.0),
        b'm' => (&arg[..arg.len() - 1], 60.0),
        b'h' => (&arg[..arg.len() - 1], 3600.0),
        b'd' => (&arg[..arg.len() - 1], 86400.0),
        _ => (arg, 1.0),
    };
    let secs = digits.parse::<f64>().ok()?;
    Some((secs * per_unit_secs).round() as u64)
}

/// The accumulated seconds a *bare* sleep command blocks for, or `None` if
/// any segment does real work beyond sleeping and a harmless no-op.
///
/// Only a bare sleep is worth naming: `sleep 2 && curl` retry backoffs are
/// ordinary and must stay unflagged, so this requires **every** segment of
/// the command to be `sleep N` or an inert no-op (`echo`, `printf`, `true`)
/// — anything else (a real command sharing the line) disqualifies the whole
/// command. That matches the pathological shape #2022 observed
/// (`sleep 300; echo done`) rather than a compound one that happens to
/// contain a sleep, and it is deliberately biased toward saying nothing: the
/// expensive direction here is the false positive, because clamping or
/// scolding a legitimate retry loop breaks real tasks.
///
/// The accumulation across segments saturates for the reason
/// `sleep_arg_seconds` does: two absurd sleeps on one line
/// (`sleep 99999999999999999999; sleep 99999999999999999999`) each saturate to
/// `u64::MAX`, and a plain `+` on that pair is an overflow panic on model
/// text in library code.
pub fn bare_sleep_seconds(command: &str) -> Option<u64> {
    let words = shell_words(command);
    let segments = segments(&words);

    let mut total_secs = 0u64;
    let mut saw_sleep = false;
    for segment in &segments {
        match segment {
            [] => {}
            [cmd, arg] if cmd == "sleep" => {
                total_secs = total_secs.saturating_add(sleep_arg_seconds(arg)?);
                saw_sleep = true;
            }
            [cmd, ..] if matches!(cmd.as_str(), "echo" | "printf" | "true") => {}
            _ => return None,
        }
    }
    saw_sleep.then_some(total_secs)
}

/// Split a tokenized command at its [`is_operator_word`] separators, so each
/// slice is one command with its arguments. Shared by the two classifiers
/// below, which both reason per command rather than per line.
fn segments(words: &[String]) -> Vec<&[String]> {
    let mut out: Vec<&[String]> = Vec::new();
    let mut start = 0;
    for (i, w) in words.iter().enumerate() {
        if is_operator_word(w) {
            out.push(&words[start..i]);
            start = i + 1;
        }
    }
    out.push(&words[start..]);
    out
}

/// Commands whose whole job is to write, move or remove a file. Every entry
/// mutates on its ordinary invocation, with no flag needed to make it do so —
/// which is what lets the classifier below key on the command word alone.
const WRITING_COMMANDS: &[&str] = &[
    "cp", "dd", "install", "ln", "mv", "patch", "rm", "rsync", "tee", "touch", "truncate",
];

/// Formatters: they rewrite the files they are pointed at, so a digested path
/// appearing as an argument is a path whose bytes have moved. A reviewed list
/// rather than a pattern, because "it looks like a formatter" is not something
/// a command word can be asked.
const FORMATTING_COMMANDS: &[&str] = &[
    "autopep8",
    "black",
    "clang-format",
    "dprint",
    "gofmt",
    "prettier",
    "rustfmt",
    "yapf",
];

/// `git` subcommands that write into the working tree. `log`, `status`,
/// `diff`, `show` and the rest are absent on purpose: this list is what
/// separates a `git` call that changes a file from the many that read one.
const WRITING_GIT_SUBCOMMANDS: &[&str] = &[
    "am",
    "apply",
    "checkout",
    "cherry-pick",
    "clean",
    "merge",
    "mv",
    "pull",
    "rebase",
    "reset",
    "restore",
    "revert",
    "rm",
    "stash",
    "switch",
];

/// What one shell command's text says about the filesystem.
///
/// Two answers rather than one, because a path-matched caller can act on the
/// first and is structurally blind to the second (#4444).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ShellWrites {
    /// The words of every segment that matches a writing shape — the candidate
    /// names of what it wrote — or empty when no segment writes. See
    /// [`shell_writes`] for what a match does and does not claim.
    pub named: Vec<String>,
    /// Some writing segment named no target at all, so [`Self::named`] cannot
    /// contain what it rewrote: `cargo fmt` over the workspace, `git stash`,
    /// `git reset --hard`, `git checkout .`.
    ///
    /// A caller matching [`Self::named`] against the paths it holds concludes
    /// "nothing of mine moved" for every one of those, which is the unsafe
    /// direction — the write is real and its target is the whole tree.
    pub sweeping: bool,
}

/// Read `command` as text and report what it says about the filesystem.
///
/// **This is a text shape, not a resolution.** Nothing here decides which
/// file was written; it decides that *a* file was, and hands back the words a
/// caller can compare against the paths it cares about. Parsing arbitrary
/// shell to extract written paths is not soundly decidable, so a caller using
/// this to invalidate cached knowledge must treat a match as "this may have
/// changed" and take the over-invalidating side of every doubt (#3827).
///
/// Three shapes make a segment a write, each one a whole segment:
///
/// - an output redirection — a word beginning with `>`, which covers `>`,
///   `>>` and both glued to their target. A leading file descriptor (`2>`)
///   deliberately does not qualify: `cmd 2>&1` tokenizes to `… 2> & 1` here,
///   and reading that as a write would make every command that captures
///   stderr look like a mutation;
/// - a command word in this module's reviewed `WRITING_COMMANDS` or
///   `FORMATTING_COMMANDS` list, or
///   `sed`/`perl` carrying an in-place flag (`-i`, `-i.bak`);
/// - `git` with a subcommand in `WRITING_GIT_SUBCOMMANDS`, or `cargo fmt`.
///
/// A writing segment that names no candidate target sets
/// [`ShellWrites::sweeping`] instead of merely contributing its own words,
/// because those words are the command and its flags and a path-matched caller
/// gets nothing from them.
///
/// What it still misses, because no reading of the command text can see it: a
/// step that rewrites files as a side effect of doing something else — `make`,
/// `cargo build` over a `build.rs` that writes into the tree, `./scripts/regen.sh`.
/// Those match no writing shape at all, so they set neither answer.
#[must_use]
pub fn shell_writes(command: &str) -> ShellWrites {
    let words = shell_words(command);
    let writing: Vec<&[String]> = segments(&words)
        .into_iter()
        .filter(|segment| segment_writes(segment))
        .collect();
    ShellWrites {
        sweeping: writing
            .iter()
            .any(|segment| !segment_names_a_target(segment)),
        named: writing.into_iter().flat_map(<[String]>::to_vec).collect(),
    }
}

/// `git` subcommands that rewrite whatever a ref or a patch happens to
/// contain, so no word of the command can be naming what moved. The rest of
/// [`WRITING_GIT_SUBCOMMANDS`] — `checkout`, `mv`, `restore`, `rm`, `switch` —
/// accept a pathspec and are read through [`segment_names_a_target`]'s general
/// rule, which is why `git merge main` is here and `git mv a.rs b.rs` is not:
/// `main` is a word that looks like a target and is not one.
const REF_WISE_GIT_SUBCOMMANDS: &[&str] = &[
    "am",
    "apply",
    "cherry-pick",
    "clean",
    "merge",
    "pull",
    "rebase",
    "reset",
    "revert",
    "stash",
];

/// Whether a writing segment spells a target a path-matched caller could
/// recognise.
///
/// The `git`/`cargo` verb is skipped before the scan, or `git stash` would
/// name `stash` as the file it wrote.
fn segment_names_a_target(segment: &[String]) -> bool {
    let mut rest = segment
        .iter()
        .skip_while(|word| is_assignment_word(word))
        .map(String::as_str);
    let Some(command) = rest.next().map(basename) else {
        return false;
    };
    let mut args = rest;
    let subcommand = if matches!(command, "git" | "cargo") {
        args.next()
    } else {
        None
    };
    if command == "git" && subcommand.is_some_and(|sub| REF_WISE_GIT_SUBCOMMANDS.contains(&sub)) {
        return false;
    }
    args.any(word_could_name_a_file)
}

/// Whether one argument could be the file a writing segment rewrote.
///
/// Deliberately narrow, and narrow in the direction that costs a false
/// *sweep* rather than a false confinement: a word carrying neither `/` nor
/// `.` is a ref, a flag or a subcommand far more often than a path, and
/// reading `main` in `git checkout main` as a target is what would let a
/// branch switch pass for a write confined to one file. A bare directory
/// (`.`, `..`, a trailing `/`) is excluded for the same reason — it names
/// everything under it, which is precisely what a path match cannot see.
///
/// A leading redirection is stripped, so `> out.rs` and `>out.rs` read alike.
fn word_could_name_a_file(word: &str) -> bool {
    let word = word.trim_start_matches('>');
    !word.is_empty()
        && !word.starts_with('-')
        && !is_assignment_word(word)
        && word != "."
        && word != ".."
        && !word.ends_with('/')
        && (word.contains('/') || word.contains('.'))
}

/// Whether one command-with-arguments matches a writing shape. The command
/// word is read through its basename, so `/usr/bin/sed` and `sed` classify
/// alike, and leading `VAR=value` assignments are skipped the way a shell
/// skips them.
fn segment_writes(segment: &[String]) -> bool {
    if segment.iter().any(|word| word.starts_with('>')) {
        return true;
    }
    let mut rest = segment
        .iter()
        .skip_while(|word| is_assignment_word(word))
        .map(String::as_str);
    let Some(command) = rest.next().map(basename) else {
        return false;
    };
    let args: Vec<&str> = rest.collect();
    match command {
        "sed" | "perl" => args.iter().any(|arg| arg.starts_with("-i")),
        "git" => args
            .first()
            .is_some_and(|sub| WRITING_GIT_SUBCOMMANDS.contains(sub)),
        "cargo" => args.first() == Some(&"fmt"),
        other => WRITING_COMMANDS.contains(&other) || FORMATTING_COMMANDS.contains(&other),
    }
}

/// Whether a word is a leading `NAME=value` environment assignment rather
/// than the command. Deliberately strict about the name: `sed -i 's/a=b/c/'`
/// must not have its pattern mistaken for one.
fn is_assignment_word(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The last `/`-separated component of a word, which is the word itself when
/// it carries no separator. Pure text: it neither resolves nor normalizes a
/// path, and `..` or a trailing slash come back as they were written.
#[must_use]
pub fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

#[cfg(test)]
mod tests {
    use super::{bare_sleep_seconds, basename, is_operator_word, shell_words, shell_writes};

    #[test]
    fn shell_words_splits_operators_attached_to_a_word_in_core() {
        assert_eq!(shell_words("cd /app; ls"), ["cd", "/app", ";", "ls"]);
        assert_eq!(shell_words("a&&b"), ["a", "&&", "b"]);
        assert_eq!(shell_words("echo 'a;b'"), ["echo", "a;b"]);
    }

    /// The paren words, which bound a command without separating two: the
    /// glued spelling has to tokenize the way the spaced one already did, or
    /// the `cd` inside a subshell is never a word at all (#3619).
    #[test]
    fn subshell_parens_are_words_of_their_own() {
        assert_eq!(
            shell_words("(cd /outside; ls)"),
            ["(", "cd", "/outside", ";", "ls", ")"]
        );
        assert_eq!(shell_words("( cd /x )"), ["(", "cd", "/x", ")"]);
        // `$(` is one opener; a `$` word beside a subshell would sit between
        // the opener and the command it introduces.
        assert_eq!(shell_words("$(cd /x)"), ["$(", "cd", "/x", ")"]);
        assert_eq!(shell_words("out=$(pwd)"), ["out=", "$(", "pwd", ")"]);
        // Quoted and escaped parens stay inside the word.
        assert_eq!(shell_words("echo '(cd /x)'"), ["echo", "(cd /x)"]);
        assert_eq!(
            shell_words(r"find . \( -name x \)"),
            ["find", ".", r"\(", "-name", "x", r"\)"]
        );
    }

    /// The other command-substitution spelling, which stayed glued to its
    /// word long after the parens stopped: `` `cd /outside` `` tokenized as
    /// `["`cd", "/outside`"]`, so nothing downstream could see the `cd` at
    /// all (#4409).
    #[test]
    fn a_backtick_substitution_splits_like_a_subshell() {
        assert_eq!(
            shell_words("`cd /outside`"),
            ["`", "cd", "/outside", "`"],
            "an unquoted backtick opens and closes a substitution"
        );
        assert_eq!(
            shell_words("out=`cd /x && pwd`"),
            ["out=", "`", "cd", "/x", "&&", "pwd", "`"]
        );
        assert_eq!(shell_words("echo `date`"), ["echo", "`", "date", "`"]);
        // Quoted or escaped, it is text and stays inside the word.
        assert_eq!(shell_words("echo '`cd /x`'"), ["echo", "`cd /x`"]);
        assert_eq!(shell_words("echo \"`date`\""), ["echo", "`date`"]);
        assert_eq!(shell_words(r"echo \`date\`"), ["echo", r"\`date\`"]);
    }

    #[test]
    fn every_separator_the_splitter_emits_is_an_operator_word_in_core() {
        for word in [";", "&", "&&", "|", "||", "\n"] {
            assert!(is_operator_word(word), "{word} should be an operator word");
        }
        for word in [">", ">>", "2>&1", "<", "<<<"] {
            assert!(
                !is_operator_word(word),
                "{word} should not be an operator word"
            );
        }
    }

    /// The shapes #2022 measured, and the accumulation across one command.
    #[test]
    fn a_bare_sleep_is_detected_and_summed() {
        assert_eq!(bare_sleep_seconds("sleep 300; echo done"), Some(300));
        assert_eq!(bare_sleep_seconds("sleep 120"), Some(120));
        assert_eq!(bare_sleep_seconds("sleep 30 && sleep 30"), Some(60));
        assert_eq!(bare_sleep_seconds("sleep 2.5"), Some(3));
    }

    /// Two absurd sleeps on ONE line, which is the pair the per-segment
    /// accumulation adds. Both saturate to `u64::MAX` on the float→int cast,
    /// so a plain `+` here panics `attempt to add with overflow` in every
    /// overflow-checked build — on model-authored text, in library code
    /// (invariant 5). The sibling assertion in `driver::loop_escalation`
    /// cannot see this: its two sleeps are two separate calls, so it only
    /// exercises the fold that already saturated.
    #[test]
    fn two_absurd_sleeps_in_one_command_saturate_rather_than_overflowing() {
        assert_eq!(
            bare_sleep_seconds("sleep 99999999999999999999"),
            Some(u64::MAX)
        );
        assert_eq!(
            bare_sleep_seconds("sleep 99999999999999999999; sleep 99999999999999999999"),
            Some(u64::MAX)
        );
    }

    /// GNU `sleep`'s suffix forms are the same wait spelled differently, and
    /// the `?` made the unsuffixed reading fail *closed*: `sleep 10m` used to
    /// answer `None` — not "a sleep worth 0s", but "not a sleep at all" —
    /// so the pathological shape wearing a suffix bypassed the rung entirely.
    #[test]
    fn a_suffixed_sleep_is_read_in_seconds() {
        assert_eq!(bare_sleep_seconds("sleep 300s"), Some(300));
        assert_eq!(bare_sleep_seconds("sleep 5m"), Some(300));
        assert_eq!(bare_sleep_seconds("sleep 10m; echo done"), Some(600));
        assert_eq!(bare_sleep_seconds("sleep 1h"), Some(3600));
        assert_eq!(bare_sleep_seconds("sleep 1d"), Some(86400));
        assert_eq!(bare_sleep_seconds("sleep 2m && sleep 30"), Some(150));
        // A suffix with no number is not a duration, and a duration this
        // cannot read still disqualifies the whole command rather than
        // silently counting as zero.
        assert_eq!(bare_sleep_seconds("sleep m"), None);
        assert_eq!(bare_sleep_seconds("sleep later"), None);
    }

    /// The shapes #3827 names, each one classified by its command word or its
    /// redirection rather than by anything about the path it carries.
    #[test]
    fn a_writing_command_hands_back_its_own_words() {
        for command in [
            "sed -i 's/a/b/' src/alpha.rs",
            "sed -i.bak 's/a/b/' src/alpha.rs",
            "cat > src/alpha.rs",
            "echo x >> src/alpha.rs",
            "mv src/alpha.rs src/beta.rs",
            "cp src/beta.rs src/alpha.rs",
            "rm src/alpha.rs",
            "git checkout src/alpha.rs",
            "git stash",
            "rustfmt src/alpha.rs",
            "cargo fmt",
            "/usr/bin/sed -i 's/a/b/' src/alpha.rs",
            "RUST_LOG=debug rustfmt src/alpha.rs",
        ] {
            assert!(
                !shell_writes(command).named.is_empty(),
                "{command} writes to the filesystem"
            );
        }
        assert_eq!(
            shell_writes("cargo test && mv src/alpha.rs src/beta.rs").named,
            ["mv", "src/alpha.rs", "src/beta.rs"],
            "only the writing segment's words come back"
        );
    }

    /// **Witness (#4444).** A write that names no target is reported as one,
    /// so a path-matched caller stops reading "no word of mine appears" as
    /// "nothing of mine moved".
    #[test]
    fn a_write_that_names_no_target_is_reported_as_sweeping() {
        for command in [
            "cargo fmt",
            "cargo fmt --all",
            "rustfmt",
            "git checkout .",
            "git stash",
            "git reset --hard",
            "git reset --hard HEAD~1",
            "git clean -fd",
            "git pull",
            "git merge main",
            "git rebase origin/main",
            "git apply /tmp/fix.patch",
            "git checkout main",
            "cargo test && git checkout .",
        ] {
            assert!(
                shell_writes(command).sweeping,
                "{command} rewrites files it never spelled"
            );
        }
    }

    /// The counter-test: a write confined to the paths it spells must stay
    /// confined, or the sweep swallows the whole rule it was added beside.
    #[test]
    fn a_write_that_spells_its_target_is_not_sweeping() {
        for command in [
            "sed -i 's/a/b/' src/alpha.rs",
            "cargo fmt -- src/alpha.rs",
            "rustfmt src/alpha.rs",
            "mv src/alpha.rs src/beta.rs",
            "cp src/beta.rs src/alpha.rs",
            "rm src/alpha.rs",
            "git checkout src/alpha.rs",
            "git checkout -- src/alpha.rs",
            "git restore src/alpha.rs",
            "cat > src/alpha.rs",
            "echo x >>src/alpha.rs",
            "cargo test",
            "git status",
        ] {
            assert!(
                !shell_writes(command).sweeping,
                "{command} names the file it wrote"
            );
        }
    }

    /// What the narrow reading of a target word deliberately over-reads, so
    /// the scope of the decision is recorded rather than implied: a written
    /// path carrying neither `/` nor `.` is not recognised as a target, and
    /// the segment reads as sweeping. Over-invalidating is the safe
    /// direction — `git checkout main` is a branch switch, and a rule loose
    /// enough to call `main` a file would call that write confined.
    #[test]
    fn a_dotless_target_word_reads_as_a_sweep() {
        for command in ["touch Makefile", "rm Makefile", "sed -i 's/a/b/' Makefile"] {
            let writes = shell_writes(command);
            assert!(
                writes.sweeping,
                "{command}: a dotless target is not recognised as one"
            );
            assert!(
                writes.named.iter().any(|word| word == "Makefile"),
                "{command}: it is still handed to the path match"
            );
        }
    }

    /// The direction that costs something: a read-only command classified as
    /// a write drops knowledge the caller had every right to keep. Capturing
    /// stderr is the shape to get right, because `2>&1` tokenizes across the
    /// `&` and its leading `2>` must not read as a redirection to a file.
    #[test]
    fn a_read_only_command_writes_nothing() {
        for command in [
            "cargo test",
            "cargo test 2>&1",
            "cargo build --workspace",
            "git status",
            "git diff src/alpha.rs",
            "git log --oneline -5",
            "rg -n 'fn main' src/alpha.rs",
            "sed -n '1,50p' src/alpha.rs",
            "ls -la src",
            "cat src/alpha.rs",
        ] {
            assert!(
                shell_writes(command).named.is_empty(),
                "{command} must not read as a write"
            );
        }
    }

    #[test]
    fn a_word_is_named_by_its_last_component() {
        assert_eq!(basename("crates/stella-core/src/alpha.rs"), "alpha.rs");
        assert_eq!(basename("alpha.rs"), "alpha.rs");
        assert_eq!(basename("/abs/alpha.rs"), "alpha.rs");
    }

    /// The expensive direction: anything doing real work beside the sleep
    /// answers `None`, whatever the sleep is worth.
    #[test]
    fn a_sleep_beside_real_work_is_never_bare() {
        assert_eq!(
            bare_sleep_seconds("sleep 2 && curl -s http://localhost:8080"),
            None
        );
        assert_eq!(bare_sleep_seconds("sleep 5; tail -f build.log"), None);
        assert_eq!(bare_sleep_seconds("echo waiting; sleep 5; ls"), None);
        assert_eq!(bare_sleep_seconds("cargo test"), None);
    }
}
