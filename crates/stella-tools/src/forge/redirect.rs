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
//! as does any alias or wrapper script. It is a fence against the thing that
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
    let pull_requests = slots.pull_requests.read().ok()?.is_some();
    let issues = slots.issues.read().ok()?.is_some();
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
/// It matches whole words rather than a substring. A path, a branch name or a
/// commit message that holds the words does not trip it. The `gh` token has to
/// be the command word: the start of the text, or straight after a separator.
/// `echo gh pr create` names the command and does not run it, and an agent
/// telling the driver what it plans must not be refused for saying the name
/// out loud.
fn mentions(command: &str, verb: (&str, &str)) -> bool {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    for (index, token) in tokens.iter().enumerate() {
        if *token != "gh" {
            continue;
        }
        let is_command_word = index == 0
            || matches!(
                tokens[index - 1],
                "|" | "&&" | "||" | ";" | "(" | "{" | "!" | "then" | "else" | "do"
            )
            || tokens[index - 1].ends_with(';')
            || tokens[index - 1].ends_with('|')
            || tokens[index - 1].ends_with('&');
        if !is_command_word {
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
            words.push(*token);
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
}
