//! A session's name, derived from the prompt that started it.
//!
//! A list shows this name in place of the session id. The name is
//! the prompt's first clause in sentence case, at most [`MAX_CHARS`]
//! characters, cut on a word boundary. A GitHub pull request or issue link
//! becomes `PR 123` or `issue 123`, so `https://github.com/o/r/pull/123 fix
//! conflicts` is named `Fix conflicts on PR 123`.

/// The longest name, in characters.
const MAX_CHARS: usize = 72;

/// The name of a session whose prompt holds no words.
const FALLBACK: &str = "New session";

/// Words that join a clause to the reference after it. A cut name never ends
/// on one.
const JOINERS: &[&str] = &[
    "on", "in", "for", "to", "at", "of", "with", "from", "into", "about", "through", "by", "via",
];

/// Words a cut name must not end on, besides [`JOINERS`].
const DANGLING: &[&str] = &["a", "an", "the", "and", "or", "but"];

/// Leading words that ask rather than say. The name drops them.
const POLITE: &[&str] = &["please", "pls", "kindly"];

/// The verbs of a leading `can you` question. The name drops them with the `you`.
const ASKING: &[&str] = &["can", "could", "would", "will"];

/// Punctuation stripped from the end of a name.
const CLOSING: &[char] = &['.', ',', ';', ':', '!', '?'];

/// One word of the name, and whether it came from a GitHub link.
struct Word {
    text: String,
    reference: bool,
}

/// Name a session from the prompt that started it.
///
/// The result is never empty, holds no newline, has no leading or trailing
/// whitespace, and is at most [`MAX_CHARS`] characters long.
pub(crate) fn session_name(prompt: &str) -> String {
    let (mut words, leading) = first_clause(prompt);
    drop_filler(&mut words);
    // A reference that opens the prompt, or closes its clause, moves to the
    // end of the name, where the cut below cannot reach it.
    let trailing = match leading {
        Some(reference) => Some((reference, true)),
        None if words.last().is_some_and(|w| w.reference) => words.pop().map(|w| (w.text, false)),
        None => None,
    };
    tidy_tail(&mut words);
    let suffix = match trailing {
        None => String::new(),
        Some((reference, add_on)) => {
            let joiner = words
                .last()
                .filter(|w| !w.reference && is_joiner(&w.text))
                .map(|w| w.text.clone());
            match joiner {
                Some(joiner) => {
                    words.pop();
                    tidy_tail(&mut words);
                    if words.is_empty() {
                        format!(" {reference}")
                    } else {
                        format!(" {joiner} {reference}")
                    }
                }
                None if add_on && !words.is_empty() => format!(" on {reference}"),
                None => format!(" {reference}"),
            }
        }
    };
    let head = cut(&mut words, MAX_CHARS.saturating_sub(suffix.chars().count()));
    let name = format!("{head}{suffix}");
    let name = name.trim();
    if name.is_empty() {
        return FALLBACK.to_string();
    }
    capitalise(name)
}

/// The words of the prompt's first clause, and a GitHub reference that came
/// before any of them.
///
/// A clause ends at a sentence's closing mark, at a standalone dash, or at
/// the end of the first line that holds a word.
fn first_clause(prompt: &str) -> (Vec<Word>, Option<String>) {
    let mut words: Vec<Word> = Vec::new();
    let mut leading = None;
    for line in prompt.lines() {
        if !words.is_empty() {
            break;
        }
        for raw in line.split_whitespace() {
            let token: String = raw.chars().filter(|c| !c.is_control()).collect();
            if token.is_empty() {
                continue;
            }
            if !words.is_empty() && is_dash(&token) {
                return (words, leading);
            }
            let ends = ends_sentence(&token);
            if let Some(reference) = github_reference(&token) {
                if words.is_empty() && leading.is_none() {
                    leading = Some(reference);
                } else {
                    words.push(Word {
                        text: reference,
                        reference: true,
                    });
                }
            } else if !(words.is_empty() && is_marker(&token)) {
                words.push(Word {
                    text: token,
                    reference: false,
                });
            }
            if ends && !words.is_empty() {
                return (words, leading);
            }
        }
    }
    (words, leading)
}

/// `PR 123` or `issue 123` for a GitHub pull request or issue link.
///
/// The link may sit in brackets, quotes, or backticks, and may carry a path,
/// a query, or a fragment after the number.
fn github_reference(token: &str) -> Option<String> {
    let wrapper = |c: char| matches!(c, '<' | '>' | '(' | ')' | '[' | ']' | '`' | '"' | '\'');
    let link = token
        .trim_start_matches(wrapper)
        .trim_end_matches(|c: char| wrapper(c) || CLOSING.contains(&c));
    let link = link
        .strip_prefix("https://")
        .or_else(|| link.strip_prefix("http://"))
        .unwrap_or(link);
    let link = link.strip_prefix("www.").unwrap_or(link);
    let mut parts = link.strip_prefix("github.com/")?.split('/');
    parts.next().filter(|owner| !owner.is_empty())?;
    parts.next().filter(|repo| !repo.is_empty())?;
    let kind = match parts.next()? {
        "pull" | "pulls" => "PR",
        "issues" => "issue",
        _ => return None,
    };
    let number: u64 = parts.next()?.split(['#', '?']).next()?.parse().ok()?;
    Some(format!("{kind} {number}"))
}

/// A leading token that carries no words: a bullet, a heading or quote mark,
/// a numbered-list mark, or a code fence.
fn is_marker(token: &str) -> bool {
    token.starts_with("```")
        || !token.chars().any(char::is_alphanumeric)
        || token
            .strip_suffix(['.', ')'])
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// A standalone dash, which ends a clause.
fn is_dash(token: &str) -> bool {
    matches!(token, "-" | "--" | "\u{2013}" | "\u{2014}")
}

/// Whether `token` closes a sentence.
///
/// A trailing dot does, unless the token reads as an abbreviation or a
/// version such as `e.g.` or `v0.9.`, whose dotted parts are each one or two
/// characters long.
fn ends_sentence(token: &str) -> bool {
    if token.ends_with(['!', '?', ';']) {
        return true;
    }
    let Some(body) = token.strip_suffix('.') else {
        return false;
    };
    !(body.contains('.') && body.split('.').all(|part| part.chars().count() <= 2))
}

/// Drop a leading `please` or `can you`, keeping at least one word.
fn drop_filler(words: &mut Vec<Word>) {
    loop {
        let lower = |i: usize| {
            words
                .get(i)
                .filter(|w| !w.reference)
                .map(|w| w.text.trim_end_matches(',').to_lowercase())
        };
        let filler = match (lower(0), lower(1)) {
            (Some(first), _) if POLITE.contains(&first.as_str()) => 1,
            (Some(first), Some(second)) if ASKING.contains(&first.as_str()) && second == "you" => 2,
            _ => 0,
        };
        if filler == 0 || filler >= words.len() {
            return;
        }
        words.drain(..filler);
    }
}

/// Strip closing punctuation off the last word, dropping a word it empties.
fn tidy_tail(words: &mut Vec<Word>) {
    while let Some(last) = words.last_mut() {
        if last.reference {
            return;
        }
        let kept = last.text.trim_end_matches(CLOSING).len();
        last.text.truncate(kept);
        if !last.text.is_empty() {
            return;
        }
        words.pop();
    }
}

/// Join `words` into at most `budget` characters.
///
/// A cut falls between words and drops a trailing joiner or article. A first
/// word longer than the budget is cut mid-word, because nothing shorter
/// names the session.
fn cut(words: &mut Vec<Word>, budget: usize) -> String {
    let full = join(words);
    if full.chars().count() <= budget {
        return full;
    }
    let mut used = 0;
    let mut keep = 0;
    for word in words.iter() {
        let len = word.text.chars().count() + usize::from(keep > 0);
        if used + len > budget {
            break;
        }
        used += len;
        keep += 1;
    }
    if keep == 0 {
        return words
            .first()
            .map(|w| w.text.chars().take(budget).collect::<String>())
            .unwrap_or_default();
    }
    words.truncate(keep);
    tidy_tail(words);
    while words.len() > 1
        && words
            .last()
            .is_some_and(|w| !w.reference && is_dangling(&w.text))
    {
        words.pop();
        tidy_tail(words);
    }
    join(words)
}

fn join(words: &[Word]) -> String {
    words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_joiner(word: &str) -> bool {
    JOINERS.iter().any(|j| j.eq_ignore_ascii_case(word))
}

fn is_dangling(word: &str) -> bool {
    is_joiner(word) || DANGLING.iter().any(|d| d.eq_ignore_ascii_case(word))
}

/// Upper-case the first letter when its capital is one character.
fn capitalise(name: &str) -> String {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut upper = first.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(capital), None) => std::iter::once(capital).chain(chars).collect(),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_leading_pull_request_link_moves_after_the_verb_clause() {
        assert_eq!(
            session_name("https://github.com/macanderson/oxagen/pull/123 fix conflicts"),
            "Fix conflicts on PR 123"
        );
    }

    #[test]
    fn a_link_alone_names_the_session() {
        assert_eq!(
            session_name("https://github.com/macanderson/oxagen/pull/123"),
            "PR 123"
        );
        assert_eq!(
            session_name("<https://github.com/macanderson/stella/issues/6590>"),
            "Issue 6590"
        );
    }

    #[test]
    fn an_issue_link_inside_the_clause_stays_in_place() {
        assert_eq!(
            session_name(
                "look into https://github.com/macanderson/stella/issues/6590 and fix the flake"
            ),
            "Look into issue 6590 and fix the flake"
        );
    }

    #[test]
    fn a_link_that_closes_the_clause_keeps_its_joiner() {
        assert_eq!(
            session_name("fix the flake in https://github.com/macanderson/stella/issues/45."),
            "Fix the flake in issue 45"
        );
    }

    #[test]
    fn a_long_prompt_is_cut_on_a_word_and_drops_a_dangling_tail() {
        let name = session_name(
            "make the session list show a short name for every run instead of the long uuid \
             it shows today and keep ids",
        );
        assert_eq!(
            name,
            "Make the session list show a short name for every run instead"
        );
        assert!(name.chars().count() <= MAX_CHARS);
    }

    #[test]
    fn a_single_long_token_is_cut_to_the_limit() {
        let name = session_name(&"a".repeat(100));
        assert_eq!(name.chars().count(), MAX_CHARS);
        assert!(name.starts_with('A'));
    }

    #[test]
    fn a_prompt_with_no_words_gets_the_fallback() {
        assert_eq!(session_name(""), FALLBACK);
        assert_eq!(session_name("  \n\t "), FALLBACK);
        assert_eq!(session_name("- \n> "), FALLBACK);
    }

    #[test]
    fn only_the_first_sentence_survives() {
        assert_eq!(
            session_name("fix the build. Then run the tests and push."),
            "Fix the build"
        );
        assert_eq!(
            session_name("Can you please fix the build? It broke on main."),
            "Fix the build"
        );
    }

    #[test]
    fn a_newline_or_a_standalone_dash_ends_the_clause() {
        assert_eq!(
            session_name("fix the parser\nit breaks on unicode input"),
            "Fix the parser"
        );
        assert_eq!(
            session_name(
                "please note how claude names sessions in a human readable way - the summary \
                 we produce today for runs is way to long"
            ),
            "Note how claude names sessions in a human readable way"
        );
    }

    #[test]
    fn an_abbreviation_or_a_version_does_not_end_the_sentence() {
        assert_eq!(
            session_name("use e.g. the cache in v0.9 builds"),
            "Use e.g. the cache in v0.9 builds"
        );
    }

    #[test]
    fn a_list_mark_or_a_fence_before_the_first_word_is_skipped() {
        assert_eq!(session_name("1. fix the build"), "Fix the build");
        assert_eq!(session_name("```\n- fix the build"), "Fix the build");
    }

    proptest! {
        #[test]
        fn a_name_is_short_single_line_and_trimmed(prompt in any::<String>()) {
            let name = session_name(&prompt);
            prop_assert!(!name.is_empty());
            prop_assert!(name.chars().count() <= MAX_CHARS, "{name:?}");
            prop_assert!(!name.contains('\n'));
            prop_assert_eq!(name.trim(), name.as_str());
        }

        #[test]
        fn a_name_with_a_link_stays_within_the_limit(
            kind in "(pull|issues)",
            number in "[0-9]{1,25}",
            text in "[a-z ,.]{0,200}",
        ) {
            let prompt = format!("https://github.com/o/r/{kind}/{number} {text}");
            let name = session_name(&prompt);
            prop_assert!(!name.is_empty());
            prop_assert!(name.chars().count() <= MAX_CHARS, "{name:?}");
        }
    }
}
