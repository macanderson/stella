// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Where a word is *data* rather than a command: the `cd`-escape detector,
//! the one `bash` advisory that needs more than words.
//!
//! It lives beside [`super`] rather than inside it because `bash.rs` sits
//! against the 1500-line ceiling, and because this concern — which parts of a
//! command text are never executed — is one subject that the tool's
//! spawn/timeout/policy body is not.
//!
//! The splitter itself moved out of this crate entirely (#2022): the engine's
//! stall rung reads the same command strings out of the transcript that the
//! `bash` advisories read out of one call, so [`shell_words`] and
//! [`is_operator_word`] now live in [`stella_core::shell_text`] and are
//! re-exported here for the scans below. Two copies of that operator list is
//! the exact drift this module was created to end.
//!
//! # What "data" means here, and why only `cd` pays for it
//!
//! A shell command carries text that is never executed: a heredoc body being
//! written to a file, a `#` comment, an echoed argument. Reading a `cd` out of
//! one of those and telling the agent its work is invisible to verification is
//! the false warning #2301 was filed for — the same defect class #2282 killed
//! for operator attachment, with the same cost per occurrence.
//!
//! [`cd_escape_target`] therefore strips those regions and requires the `cd`
//! to sit in **command position** before it will name a target. The audit in
//! [`super::shell_write_audit`] deliberately does neither: it is a fence
//! around what the shell may be about to overwrite, so a path appearing
//! anywhere in the text is exactly what it wants to see.

use std::path::Path;

/// The command-text splitter and its operator predicate, re-exported from
/// [`stella_core::shell_text`] where they now live so the engine's stall rung
/// and these advisories read one implementation (#2022).
pub(super) use stella_core::shell_text::{is_operator_word, shell_words};

/// One pending heredoc: the delimiter that ends its body, and whether `<<-`
/// asked for leading tabs to be ignored on that terminator line.
struct Heredoc {
    delimiter: String,
    strip_tabs: bool,
}

/// The command with its **non-executed text removed** — heredoc bodies and
/// `#` comments — so a scan cannot mistake data for a command.
///
/// Newlines outside the dropped regions are preserved, because command
/// position is read from them. Quoting is tracked well enough to know whether
/// a `<<` or a `#` is live: `echo "# not a comment"` and `echo '<<EOF'` keep
/// their text. Everything else is copied through byte for byte — this removes
/// regions, it does not rewrite words.
///
/// Deliberately partial, on the "better a missed note than a wrong one"
/// posture this whole module holds: a delimiter produced by expansion
/// (`<<$END`) is taken literally, and an unterminated heredoc swallows the
/// rest of the command rather than guessing where it ended. Both directions
/// lose a warning; neither invents one.
fn strip_data_regions(command: &str) -> String {
    let chars: Vec<char> = command.chars().collect();
    let mut out = String::with_capacity(command.len());
    let mut pending: Vec<Heredoc> = Vec::new();
    let (mut in_single, mut in_double) = (false, false);
    let mut i = 0usize;

    while i < chars.len() {
        let c = chars[i];
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
                out.push(c);
                i += 1;
            }
            '"' if !in_single => {
                in_double = !in_double;
                out.push(c);
                i += 1;
            }
            '\\' if !in_single => {
                out.push(c);
                i += 1;
                if let Some(&next) = chars.get(i) {
                    out.push(next);
                    i += 1;
                }
            }
            // `#` opens a comment only at the start of a word: `echo foo#bar`
            // passes a literal `#` and `curl host/#frag` names a fragment.
            '#' if !in_single && !in_double && starts_a_word(&chars, i) => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            // `<<` opens a heredoc; `<<<` is a here-string, whose word is
            // ordinary text on the same line and needs no skipping.
            '<' if !in_single
                && !in_double
                && chars.get(i + 1) == Some(&'<')
                && chars.get(i + 2) != Some(&'<')
                && i.checked_sub(1).map(|prev| chars[prev]) != Some('<') =>
            {
                i += 2;
                let strip_tabs = chars.get(i) == Some(&'-');
                if strip_tabs {
                    i += 1;
                }
                while matches!(chars.get(i), Some(' ' | '\t')) {
                    i += 1;
                }
                let (delimiter, after) = read_delimiter(&chars, i);
                i = after;
                if !delimiter.is_empty() {
                    pending.push(Heredoc {
                        delimiter,
                        strip_tabs,
                    });
                }
                // The operator and its delimiter are gone; keep the word
                // boundary they occupied.
                out.push(' ');
            }
            '\n' if !in_single && !in_double && !pending.is_empty() => {
                out.push('\n');
                i += 1;
                for heredoc in std::mem::take(&mut pending) {
                    i = skip_heredoc_body(&chars, i, &heredoc);
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Is the character at `at` the first of a word — start of input, or preceded
/// by whitespace or an operator character?
fn starts_a_word(chars: &[char], at: usize) -> bool {
    match at.checked_sub(1).map(|prev| chars[prev]) {
        None => true,
        Some(prev) => prev.is_whitespace() || matches!(prev, ';' | '&' | '|' | '('),
    }
}

/// The heredoc delimiter beginning at `from`, unquoted, and the index just
/// past it. `<<'EOF'`, `<<"EOF"` and `<<\EOF` all name `EOF` — the quoting
/// only decides whether the body expands, which is not a question this asks.
fn read_delimiter(chars: &[char], from: usize) -> (String, usize) {
    let mut delimiter = String::new();
    let (mut in_single, mut in_double) = (false, false);
    let mut i = from;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '\\' if !in_single => {
                i += 1;
                match chars.get(i) {
                    Some(&next) => delimiter.push(next),
                    None => break,
                }
            }
            c if !in_single && !in_double && (c.is_whitespace() || is_delimiter_end(c)) => break,
            c => delimiter.push(c),
        }
        i += 1;
    }
    (delimiter, i)
}

/// Characters that end a heredoc delimiter word without being part of it.
fn is_delimiter_end(c: char) -> bool {
    matches!(c, ';' | '&' | '|' | '<' | '>')
}

/// Skip from `from` to just past the line that terminates `heredoc`'s body,
/// or to the end of input if that line never comes.
fn skip_heredoc_body(chars: &[char], from: usize, heredoc: &Heredoc) -> usize {
    let mut i = from;
    while i < chars.len() {
        let line_start = i;
        while i < chars.len() && chars[i] != '\n' {
            i += 1;
        }
        let line: String = chars[line_start..i].iter().collect();
        if i < chars.len() {
            i += 1; // consume the newline that ends this body line
        }
        let candidate = if heredoc.strip_tabs {
            line.trim_start_matches('\t')
        } else {
            line.as_str()
        };
        // The terminator is the delimiter alone on its line; `\r` is tolerated
        // so a CRLF-authored command still closes.
        if candidate.trim_end_matches('\r') == heredoc.delimiter {
            return i;
        }
    }
    i
}

/// If the command `cd`s somewhere outside the workspace `root`, return that
/// target — every other tool is confined to `root`, so an edit under a drifted
/// tree is invisible to the file tools, the turn's diff, and verification (and
/// ungraphed besides, the cross-tree gap the telemetry showed). Targets we
/// can't resolve (`$VAR`, `~`, `-`, a flag) are skipped: better a missed note
/// than a wrong one.
///
/// Two rules keep a `cd` that is *data* from being read as a directory change
/// (#2301): the non-executed regions come off first
/// ([`strip_data_regions`]), and the word must sit in **command position** —
/// first in the text, or right after an operator word. `echo cd /outside`
/// therefore says nothing, while `ls\ncd /outside` still warns, because the
/// newline is an operator word.
pub(super) fn cd_escape_target(command: &str, root: &Path) -> Option<String> {
    let words = shell_words(&strip_data_regions(command));
    for (index, word) in words.iter().enumerate() {
        if word != "cd" || (index > 0 && !is_operator_word(&words[index - 1])) {
            continue;
        }
        let Some(target) = words.get(index + 1).map(String::as_str) else {
            continue;
        };
        // `-` (cd to previous dir) is caught by the `-` prefix below. An
        // operator word means the `cd` had no target at all (`cd && make`):
        // that goes to `$HOME`, which we skip for the same reason as `~`.
        if target.is_empty()
            || target.starts_with('$')
            || target.starts_with('~')
            || target.starts_with('-')
            || is_operator_word(target)
        {
            continue;
        }
        if crate::resolve_within_root(root, target).is_none() {
            return Some(target.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cd_escape_target_flags_out_of_root_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        // In-root cd — no drift.
        assert_eq!(cd_escape_target("cd sub && ls", dir.path()), None);
        assert_eq!(cd_escape_target("cd . && cargo build", dir.path()), None);
        // Out-of-root — the target is returned.
        assert_eq!(
            cd_escape_target("cd / && grep -rn x .", dir.path()).as_deref(),
            Some("/")
        );
        assert!(cd_escape_target("cd ../.. && ls", dir.path()).is_some());
        // Unresolvable / no cd — skipped.
        assert_eq!(cd_escape_target("cd $HOME && ls", dir.path()), None);
        assert_eq!(cd_escape_target("cd ~/foo && ls", dir.path()), None);
        assert_eq!(cd_escape_target("grep -rn x .", dir.path()), None);
    }

    #[test]
    fn shell_words_splits_operators_attached_to_a_word() {
        assert_eq!(shell_words("cd /app; ls"), ["cd", "/app", ";", "ls"]);
        assert_eq!(shell_words("a&&b"), ["a", "&&", "b"]);
        assert_eq!(
            shell_words("a || b|c & d"),
            ["a", "||", "b", "|", "c", "&", "d"]
        );
        // Quoted and escaped operators stay inside the word.
        assert_eq!(shell_words("echo 'a;b'"), ["echo", "a;b"]);
        assert_eq!(shell_words(r"grep x\|y ."), ["grep", r"x\|y", "."]);
    }

    /// The false positive a paid bench trial hit: `cd /app; …` with session
    /// root `/app` drew the "work here is invisible to the diff and to
    /// verification" warning, because the target parsed as the unresolvable
    /// `/app;`.
    #[test]
    fn an_attached_separator_does_not_fake_a_cd_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        // In-root cd with the separator attached — no drift, no warning.
        assert_eq!(cd_escape_target("cd sub; ls", &root), None);
        assert_eq!(
            cd_escape_target(&format!("cd {}; ls", root.display()), &root),
            None
        );
        // A real escape still warns, and names the clean target.
        assert_eq!(cd_escape_target("cd /; ls", &root).as_deref(), Some("/"));
        // A bare `cd` before an operator goes to `$HOME` — unresolvable
        // here, so it is skipped rather than misnamed.
        assert_eq!(cd_escape_target("cd && ls", &root), None);
    }

    /// #2301 witness: writing a script with a heredoc is a very common agent
    /// shape, and the `cd` in its body is text being written to a file — not
    /// a directory change this session ever makes.
    #[test]
    fn a_heredoc_body_cd_is_not_an_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert_eq!(
            cd_escape_target(
                "cat > setup.sh <<'EOF'\ncd /workspace\nmake\nEOF\nbash setup.sh",
                &root
            ),
            None
        );
        // The unquoted-delimiter and tab-stripping spellings behave the same.
        assert_eq!(
            cd_escape_target("cat <<-EOF > s.sh\n\tcd /workspace\n\tEOF\nls", &root),
            None
        );
        // A here-string is not a heredoc: nothing after it is a body, so the
        // real `cd` on the next line is still seen.
        assert_eq!(
            cd_escape_target("grep x <<< \"$body\"\ncd /", &root).as_deref(),
            Some("/")
        );
    }

    /// #2301 witness: an echoed `cd` is an argument to `echo`, not a command.
    #[test]
    fn an_echoed_cd_is_not_an_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert_eq!(cd_escape_target("echo cd /outside", &root), None);
        assert_eq!(cd_escape_target("echo \"cd /outside\"", &root), None);
        assert_eq!(cd_escape_target(r"printf 'cd /tmp\n'", &root), None);
    }

    /// #2301 witness: a `#` comment is never executed, so neither is the `cd`
    /// somebody left in one.
    #[test]
    fn a_commented_cd_is_not_an_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert_eq!(cd_escape_target("make build # cd /old-path", &root), None);
        assert_eq!(cd_escape_target("# cd /old-path\nmake build", &root), None);
        // A `#` inside a word is not a comment, and a quoted one is text.
        assert_eq!(cd_escape_target("echo \"# cd /old-path\"", &root), None);
    }

    /// The over-suppression guard for the two rules above: requiring command
    /// position without making a newline an operator word would stop warning
    /// on the second line of a multi-line command, which is a real escape.
    #[test]
    fn a_real_cd_on_a_later_line_still_warns() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert_eq!(cd_escape_target("ls\ncd /", &root).as_deref(), Some("/"));
        assert_eq!(
            cd_escape_target("make build # build first\ncd /", &root).as_deref(),
            Some("/")
        );
        assert_eq!(
            cd_escape_target("cat > s.sh <<'EOF'\necho hi\nEOF\ncd /", &root).as_deref(),
            Some("/")
        );
    }

    /// The four inline copies this predicate replaced had already diverged:
    /// the grep-scan boundary omitted `&`.
    #[test]
    fn is_operator_word_covers_every_separator_the_splitter_emits() {
        for word in [";", "&", "&&", "|", "||", "\n"] {
            assert!(is_operator_word(word), "{word} should be an operator word");
        }
        // Redirections are not separators — they belong to the command.
        for word in [">", ">>", "2>&1", "<", "<<<"] {
            assert!(
                !is_operator_word(word),
                "{word} should not be an operator word"
            );
        }
    }
}
