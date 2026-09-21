//! The `gh` subcommands this plane has taken over, and the refusal that
//! names the tool to use instead.
//!
//! # Why a redirect rather than a ban
//!
//! `gh` stays open. An agent that needs `gh release list`, `gh api`, or
//! `gh repo view` still has it, and nothing here touches `git`. What is shut
//! is the small set of subcommands a tool in this module covers. Those are
//! the ones that write text with a footer on it to the forge. A description
//! sent through `gh pr create` carries whatever footer the model chose to
//! type. This plane exists to take that choice away.
//!
//! So the refusal is a redirect. It names the tool, and the action on it, and
//! the call works on the retry. That shape counts for more than the ban does.
//! A bare "not permitted" teaches a model that the tool is gone. What it does
//! next is climb the fence: `gh api` by hand, or a `curl`.
//!
//! # Gated on the slot
//!
//! Every check here waits on the forge being attached. A workspace with no
//! forge set up has no `pull_request` tool to point at. Refusing
//! `gh pr create` there would take something away and give nothing back. The
//! gate is the slot rather than a setting, so the two cannot disagree.
//!
//! # The limits of a text scan
//!
//! This reads the command text. That is the same limit [`crate::bash`]'s own
//! audit states about itself. So `eval "gh pr $verb"` walks straight past it,
//! as does an alias, or a script of some other name that calls `gh` inside.
//! The spellings that keep the word `gh` in the line are read, and
//! `mentions_in_line` says which. It is a fence against the thing that
//! really happens: a model reaching for the command it knows. It holds
//! nothing in. What makes the footer sure is that the tools are the easier
//! path, and that is what the refusal text is for.

use super::ForgeSlots;

/// One taken-over `gh` surface: the subcommand, and where it moved to.
struct Redirect {
    /// The `gh` subcommand, as its two words: `("pr", "create")`.
    verb: (&'static str, &'static str),
    /// The tool that does this now, and the action on it.
    tool: &'static str,
    /// What to call it with, in the words the tool takes.
    action: &'static str,
}

/// Every `gh` subcommand a tool in this module covers.
///
/// `pr view`, `pr list`, `pr diff`, `issue list` and `issue view` are absent.
/// They read, they write no footer, and no tool here stands in for them.
/// `run watch` is absent for its own reason: `watch_ci` does not block, so it
/// cannot stand in for a command whose whole job is to block. Refusing it
/// would leave a real gap.
const REDIRECTS: &[Redirect] = &[
    Redirect {
        verb: ("pr", "create"),
        tool: "pull_request",
        action: "create",
    },
    Redirect {
        verb: ("pr", "edit"),
        tool: "pull_request",
        action: "update",
    },
    Redirect {
        verb: ("pr", "ready"),
        tool: "pull_request",
        action: "update with draft",
    },
    Redirect {
        verb: ("pr", "close"),
        tool: "pull_request",
        action: "close",
    },
    Redirect {
        verb: ("pr", "merge"),
        tool: "pull_request",
        action: "merge with confirm: true",
    },
    Redirect {
        verb: ("pr", "comment"),
        tool: "pull_request",
        action: "comment",
    },
    Redirect {
        verb: ("pr", "checks"),
        tool: "watch_ci",
        action: "the pull request's branch",
    },
    Redirect {
        verb: ("issue", "create"),
        tool: "issue",
        action: "create",
    },
    Redirect {
        verb: ("issue", "edit"),
        tool: "issue",
        action: "update",
    },
    Redirect {
        verb: ("issue", "close"),
        tool: "issue",
        action: "close",
    },
    Redirect {
        verb: ("issue", "comment"),
        tool: "issue",
        action: "comment",
    },
];

/// Refuse a shell command that reaches a forge surface a tool now owns.
///
/// `None` means run it. The gate is the slot: one nobody filled has no tool
/// behind it, so the command is the only way in and stays open.
#[must_use]
pub fn forge_redirect(command: &str, slots: &ForgeSlots) -> Option<String> {
    let pull_requests = slots.has_forge();
    let issues = slots.has_tracker();
    if !pull_requests && !issues {
        return None;
    }
    for redirect in REDIRECTS {
        let attached = match redirect.verb.0 {
            "issue" => issues,
            _ => pull_requests,
        };
        if attached && mentions(command, redirect.verb) {
            return Some(format!(
                "`gh {} {}` is covered by the `{}` tool, which signs what it writes so the record \
                 says what produced it. Call `{}` with action `{}` instead. The rest of `gh` is \
                 open, and so is `git`.",
                redirect.verb.0, redirect.verb.1, redirect.tool, redirect.tool, redirect.action
            ));
        }
    }
    None
}

/// Does this command run `gh <group> <verb>` anywhere in it?
///
/// A newline is a command separator, so each line is scanned on its own.
/// Reading the whole text at once let the last word of one line stand as the
/// word before `gh` on the next, and a two-line script is what a model writes
/// when it pushes a branch and then opens the pull request for it.
fn mentions(command: &str, verb: (&str, &str)) -> bool {
    command.lines().any(|line| mentions_in_line(line, verb))
}

/// [`mentions`] within one line, where the first word is a command word.
///
/// It matches whole words rather than a substring. A path, a branch name or a
/// commit message that holds the words does not trip it. The `gh` token has to
/// be the command word: the start of the line, or straight after a separator.
/// `echo gh pr create` names the command and does not run it, and an agent
/// telling the driver what it plans must not be refused for saying the name
/// out loud.
///
/// Asking that of the literal token `gh` is what the first version did, and
/// six spellings ran straight past it: `command gh`, `env gh`, `sudo gh`,
/// `GH_TOKEN=x gh`, `/usr/bin/gh`, and `gh` opened inside `$(…)` or
/// backticks. Each one is the same command, so each one has to reach the same
/// answer — a redirect that a wrapper word walks around does not redirect
/// anything. [`names_gh`] settles the path spellings, [`space_substitutions`]
/// the substitutions, and [`is_command_position`] the wrappers and the
/// assignments.
fn mentions_in_line(command: &str, verb: (&str, &str)) -> bool {
    let spaced = space_substitutions(command);
    let tokens: Vec<&str> = spaced.split_whitespace().collect();
    for (index, token) in tokens.iter().enumerate() {
        if !names_gh(bare(token)) {
            continue;
        }
        if !is_command_position(&tokens[..index]) {
            continue;
        }
        // Flags may sit between `gh` and its subcommand (`gh --repo o/r pr
        // merge`). So the two words are matched in order among the words that
        // are not flags, rather than at fixed offsets. The scan stops at the
        // next separator, so a second command's words cannot fill out the
        // first one's pair. A flag's value is skipped with it: `o/r` above is
        // not the subcommand, and reading it as one is how `--repo` hides a
        // merge.
        let mut words = Vec::new();
        let mut expecting_value = false;
        for token in tokens[index + 1..]
            .iter()
            .take_while(|token| !matches!(**token, "|" | "&&" | "||" | ";"))
        {
            if token.starts_with('-') {
                // `--repo=o/r` carries its value, so nothing follows it.
                expecting_value = !token.contains('=');
                continue;
            }
            if expecting_value {
                expecting_value = false;
                continue;
            }
            // A quoted subcommand is still the subcommand.
            words.push(bare(token));
            if words.len() == 2 {
                break;
            }
        }
        if words.first() == Some(&verb.0) && words.get(1) == Some(&verb.1) {
            return true;
        }
    }
    false
}

/// Quoting that can be glued to a word without changing what it says.
const QUOTES: &[char] = &['\'', '"'];

/// The words that can precede a command without changing which one runs.
///
/// Each takes a command as its argument and runs it. `command` and `builtin`
/// are the two a shell offers for reaching past a function or an alias, which
/// is the form a model reaches for when it has been told not to call
/// something.
const WRAPPERS: &[&str] = &[
    "command", "builtin", "exec", "env", "nohup", "nice", "time", "sudo", "doas",
];

/// Give each substitution character a word of its own.
///
/// `split_whitespace` hands back `url=$(gh` as a single word, so nothing that
/// reads words could see the `gh` inside it. Spacing `(`, `)` and the backtick
/// out turns the opener into a token, and [`is_command_position`] already
/// treats an opener as the start of a command — which it is, since nothing can
/// precede the first word of a substitution.
///
/// The other separators are left glued on. `;`, `|` and `&` are settled by
/// [`is_separator`]'s trailing-character arm, which has to exist for the
/// spaced spellings anyway.
fn space_substitutions(line: &str) -> String {
    let mut spaced = String::with_capacity(line.len() + 8);
    for character in line.chars() {
        if matches!(character, '(' | ')' | '`') {
            spaced.push(' ');
            spaced.push(character);
            spaced.push(' ');
        } else {
            spaced.push(character);
        }
    }
    spaced
}

/// Peel the quoting off a word.
///
/// Quoting changes how a shell reads a word and not what the word says, so
/// `"gh" "pr" "create"` is the same command. It grants nothing on its own: a
/// quoted `gh` is the command word only where a bare one would be.
fn bare(token: &str) -> &str {
    token.trim_matches(QUOTES)
}

/// Does this word run `gh`?
///
/// A path names the same binary. `/usr/bin/gh` and `./gh` skip `PATH` and run
/// exactly what `gh` would.
fn names_gh(word: &str) -> bool {
    word.rsplit('/').next() == Some("gh")
}

/// Is the word after `prefix` the start of a command?
///
/// It walks backwards over what a shell allows in front of a command and what
/// does not change which command runs: a wrapper from [`WRAPPERS`], a flag
/// belonging to one, and a one-shot assignment such as `GH_TOKEN=x`.
/// Everything else answers the original question, which is whether the word
/// before is a separator or there is no word before at all.
///
/// The walk cannot turn a mention into a run. `echo command gh pr create`
/// steps over `command` and reaches `echo`, which is neither a separator nor a
/// wrapper, so it stays open — the same answer `echo gh pr create` gets.
///
/// A flag has to be claimed by a wrapper to count. `env -i gh` is one command
/// and a bare `--fill gh` is an argument to whatever ran `--fill`, and the
/// walk meets the flag first in both, so it carries the unclaimed flag along
/// and refuses to call the position a command start unless a wrapper turns up
/// to own it.
fn is_command_position(prefix: &[&str]) -> bool {
    let mut prefix = prefix;
    let mut unclaimed_flag = false;
    loop {
        let Some((previous, rest)) = prefix.split_last() else {
            return !unclaimed_flag;
        };
        if is_separator(previous) {
            return !unclaimed_flag;
        }
        if WRAPPERS.contains(previous) {
            unclaimed_flag = false;
        } else if previous.starts_with('-') {
            unclaimed_flag = true;
        } else if !is_assignment(previous) {
            return false;
        }
        prefix = rest;
    }
}

/// Does this token end the command before it?
///
/// The trailing-character arm is what catches a separator written without a
/// space, which is the common spelling: `git push;` and `git push &&`. It
/// accepts a false positive in return — a word that genuinely ends in `;`,
/// `|` or `&` reads as a separator, so `echo hi; gh pr create` and the
/// unlikely `echo 'hi&' gh pr create` reach the same answer. Both directions
/// were considered and this one is the safe one: the cost is a refusal with a
/// tool named in it, and the other cost is a redirect walked around.
fn is_separator(token: &str) -> bool {
    matches!(
        token,
        "|" | "&&" | "||" | "&" | ";" | "(" | "`" | "{" | "!" | "then" | "else" | "do"
    ) || token.ends_with([';', '|', '&'])
}

/// Is this `NAME=value`, the assignment a command may carry in front of it?
fn is_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// Slots with both providers filled, so every redirect is armed.
    fn attached() -> ForgeSlots {
        struct Nothing;

        #[async_trait::async_trait]
        impl stella_protocol::issue::IssueProvider for Nothing {
            fn id(&self) -> &str {
                "nothing"
            }
            async fn list_open(
                &self,
                _limit: usize,
            ) -> Result<Vec<stella_protocol::issue::Issue>, stella_protocol::issue::IssueError>
            {
                Ok(Vec::new())
            }
            async fn file(
                &self,
                _draft: &stella_protocol::issue::IssueDraft,
            ) -> Result<stella_protocol::issue::IssueKey, stella_protocol::issue::IssueError>
            {
                unimplemented!("this fixture only fills the slot")
            }
            async fn close(
                &self,
                _key: &stella_protocol::issue::IssueKey,
                _receipt: &str,
                _state: &str,
            ) -> Result<(), stella_protocol::issue::IssueError> {
                Ok(())
            }
            async fn comment(
                &self,
                _key: &stella_protocol::issue::IssueKey,
                _body: &str,
            ) -> Result<stella_protocol::issue::CommentId, stella_protocol::issue::IssueError>
            {
                unimplemented!("this fixture only fills the slot")
            }
            async fn relabel(
                &self,
                _key: &stella_protocol::issue::IssueKey,
                _add: &[String],
                _remove: &[String],
            ) -> Result<(), stella_protocol::issue::IssueError> {
                Ok(())
            }
            async fn edit(
                &self,
                _key: &stella_protocol::issue::IssueKey,
                _title: Option<&str>,
                _body: Option<&str>,
            ) -> Result<(), stella_protocol::issue::IssueError> {
                Ok(())
            }
            async fn get(
                &self,
                _key: &stella_protocol::issue::IssueKey,
            ) -> Result<stella_protocol::issue::Issue, stella_protocol::issue::IssueError>
            {
                unimplemented!("this fixture only fills the slot")
            }
        }

        impl stella_protocol::pull_request::PullRequestProvider for Nothing {
            fn id(&self) -> &str {
                "nothing"
            }
            fn open(
                &self,
                _draft: &stella_protocol::pull_request::PullRequestDraft,
            ) -> Result<
                stella_protocol::pull_request::PullRequestKey,
                stella_protocol::pull_request::PullRequestError,
            > {
                unimplemented!("this fixture only fills the slot")
            }
            fn get(
                &self,
                _key: &stella_protocol::pull_request::PullRequestKey,
            ) -> Result<
                stella_protocol::pull_request::PullRequest,
                stella_protocol::pull_request::PullRequestError,
            > {
                unimplemented!("this fixture only fills the slot")
            }
            fn list_open(
                &self,
                _search: Option<&str>,
            ) -> Result<
                Vec<stella_protocol::pull_request::PullRequestSummary>,
                stella_protocol::pull_request::PullRequestError,
            > {
                Ok(Vec::new())
            }
            fn checks_for_ref(
                &self,
                _git_ref: &str,
            ) -> Result<
                Vec<stella_protocol::pull_request::Check>,
                stella_protocol::pull_request::PullRequestError,
            > {
                Ok(Vec::new())
            }
            fn required_checks(
                &self,
                _branch: &str,
            ) -> Result<Vec<String>, stella_protocol::pull_request::PullRequestError> {
                Ok(Vec::new())
            }
            fn recent_commits(
                &self,
                _branch: &str,
                _depth: usize,
            ) -> Result<Vec<String>, stella_protocol::pull_request::PullRequestError> {
                Ok(Vec::new())
            }
            fn mark_ready(
                &self,
                _key: &stella_protocol::pull_request::PullRequestKey,
            ) -> Result<(), stella_protocol::pull_request::PullRequestError> {
                Ok(())
            }
            fn add_label(
                &self,
                _key: &stella_protocol::pull_request::PullRequestKey,
                _label: &str,
            ) -> Result<(), stella_protocol::pull_request::PullRequestError> {
                Ok(())
            }
            fn merge(
                &self,
                _key: &stella_protocol::pull_request::PullRequestKey,
            ) -> Result<(), stella_protocol::pull_request::PullRequestError> {
                Ok(())
            }
            fn sync_with_base(
                &self,
                _key: &stella_protocol::pull_request::PullRequestKey,
            ) -> Result<(), stella_protocol::pull_request::PullRequestError> {
                Ok(())
            }
            fn rerun_failed_checks(
                &self,
                _key: &stella_protocol::pull_request::PullRequestKey,
            ) -> Result<(), stella_protocol::pull_request::PullRequestError> {
                Ok(())
            }
        }

        let slots = ForgeSlots::default();
        let nothing = Arc::new(Nothing);
        *slots.issues.write().unwrap() = Some(nothing.clone());
        *slots.pull_requests.write().unwrap() = Some(nothing);
        slots
    }

    /// A covered subcommand is turned back, and the refusal names the tool.
    #[test]
    fn a_covered_subcommand_names_the_tool_that_replaced_it() {
        let slots = attached();
        let refusal = forge_redirect("gh pr create --fill", &slots).expect("redirected");
        assert!(refusal.contains("pull_request"), "{refusal}");
        assert!(refusal.contains("create"), "{refusal}");

        let refusal = forge_redirect("gh issue comment 412 --body hi", &slots).expect("redirected");
        assert!(refusal.contains("`issue` tool"), "{refusal}");
    }

    /// Flags between `gh` and its subcommand do not hide it.
    #[test]
    fn a_repo_flag_does_not_slip_the_subcommand_past() {
        let slots = attached();
        assert!(forge_redirect("gh --repo o/r pr merge 31", &slots).is_some());
    }

    /// Reading is not writing, and the rest of `gh` stays open.
    ///
    /// This is the failure that makes a fence worse than no fence. An agent
    /// refused for `gh pr view` learns the whole command is gone. It then
    /// hand-rolls `gh api` calls, which nothing here checks.
    #[test]
    fn reads_and_everything_else_stay_open() {
        let slots = attached();
        for open in [
            "gh pr view 31",
            "gh pr list",
            "gh pr diff 31",
            "gh issue view 412",
            "gh issue list",
            "gh repo view",
            "gh release list",
            "gh run watch 9",
            "git push origin HEAD",
            "git commit -m 'gh pr create is what I would have run'",
        ] {
            assert_eq!(forge_redirect(open, &slots), None, "{open}");
        }
    }

    /// With nothing attached, nothing is refused.
    ///
    /// A workspace with no forge set up has no tool to point at. A refusal
    /// there would close the only way to do the thing.
    #[test]
    fn an_unattached_forge_refuses_nothing() {
        let slots = ForgeSlots::default();
        assert_eq!(forge_redirect("gh pr create --fill", &slots), None);
    }

    /// The word `gh` inside an argument is not a command.
    #[test]
    fn naming_the_command_is_not_running_it() {
        let slots = attached();
        assert_eq!(
            forge_redirect("echo gh pr create > /dev/null", &slots),
            None
        );
    }

    /// A pipeline's second command is checked too.
    #[test]
    fn a_command_after_a_separator_is_still_checked() {
        let slots = attached();
        assert!(forge_redirect("git push && gh pr create --fill", &slots).is_some());
    }

    /// A newline separates two commands, and the second one is checked.
    ///
    /// Scanning the whole text at once read `HEAD` as the word before `gh`,
    /// found it was not a separator, and let the call through. Two lines is
    /// how a model writes "push the branch, then open the pull request", so
    /// this was the common case rather than an exotic one.
    #[test]
    fn a_command_on_the_next_line_is_still_checked() {
        let slots = attached();
        assert!(
            forge_redirect("git push -u origin HEAD\ngh pr create --fill", &slots).is_some(),
            "a covered verb on the second line must still be refused"
        );
    }

    /// The line scan does not make a named command look like a run one.
    ///
    /// Each line is scanned from its own start, so the guard against
    /// `echo gh pr create` has to hold on every line, not only the first.
    #[test]
    fn naming_the_command_on_a_later_line_is_still_not_running_it() {
        let slots = attached();
        assert_eq!(
            forge_redirect("set -e\necho gh pr create > /dev/null", &slots),
            None
        );
    }

    /// Every spelling of the same command reaches the same answer.
    ///
    /// Each line below runs `gh pr create`. Matching the literal token `gh`
    /// at a command position caught the first one and none of the rest, so a
    /// model that had been refused once could reach the surface by adding a
    /// word. A redirect a wrapper word walks around redirects nothing.
    #[test]
    fn a_wrapped_or_pathed_command_is_still_the_command() {
        let slots = attached();
        for spelling in [
            "gh pr create --fill",
            "command gh pr create --fill",
            "builtin gh pr create --fill",
            "exec gh pr create --fill",
            "env gh pr create --fill",
            "env -i gh pr create --fill",
            "nohup gh pr create --fill",
            "sudo gh pr create --fill",
            "GH_TOKEN=x gh pr create --fill",
            "GH_TOKEN=x GH_HOST=github.com gh pr create --fill",
            "env GH_TOKEN=x gh pr create --fill",
            "/usr/bin/gh pr create --fill",
            "./gh pr create --fill",
            "command /usr/local/bin/gh pr create --fill",
            "url=$(gh pr create --fill)",
            "url=`gh pr create --fill`",
            "echo hi && command gh pr create --fill",
        ] {
            assert!(
                forge_redirect(spelling, &slots).is_some(),
                "must be refused: {spelling}"
            );
        }
    }

    /// A wrapper word does not turn a mention into a run.
    ///
    /// The backwards walk steps over `command`, and what it steps onto has to
    /// answer the original question. `echo` is not a separator, so every line
    /// here stays open — the same answer `echo gh pr create` already got.
    #[test]
    fn a_wrapper_inside_an_argument_is_still_not_a_command() {
        let slots = attached();
        for open in [
            "echo command gh pr create",
            "echo env gh pr create",
            "echo /usr/bin/gh pr create",
            "git commit -m \"gh pr create\"",
            "echo GH_TOKEN=x gh pr create",
        ] {
            assert_eq!(forge_redirect(open, &slots), None, "{open}");
        }
    }
}
