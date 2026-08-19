//! `triage` and `close` — how an issue is classified, and how it ends.
//!
//! One subject, so one module: both are the loop exercising authority over the
//! backlog it draws from, and both are bounded by the same rule — it owns the
//! backlog's *hygiene* and none of what an issue *means*.
//!
//! Split out of `self_driving_cmd.rs` rather than added to it. That file is
//! over the 1500-line limit and `doc:backlog-self-driving` §9 records it as
//! closed to growth, with new logic landing in siblings here (AGENTS.md
//! § *God files* — plan around them, never into them).

use stella_autonomy::{Citation, Closure, ClosureRefusal};

use crate::query_format::QueryFormat;

use super::{backlog, config, convention, state};

/// `stella self-driving triage` — bring an issue up to the standard.
///
/// Mechanical repairs are applied; judgement is reported. The split is the
/// point: the loop has authority over the backlog's *hygiene* and none over
/// what an issue *means*.
pub(super) fn triage_issue(key: &str, dry_run: bool, format: QueryFormat) -> Result<(), String> {
    let root = state::repo_root();
    let bound = convention::load(&root);
    let provider = crate::issue_provider::GhIssueProvider;
    let issue = backlog::resolve(&provider, key)?;

    let labels: Vec<&str> = issue.labels.iter().map(|l| l.name.as_str()).collect();
    let plan = stella_autonomy::repair(&bound.convention, &labels);

    if format == QueryFormat::Json {
        println!(
            "{}",
            serde_json::to_string(&plan).map_err(|e| e.to_string())?
        );
    } else if plan.is_empty() {
        println!("#{key} already matches this workspace's convention");
    } else {
        if !plan.remove.is_empty() {
            println!("#{key} remove: {}", plan.remove.join(", "));
        }
        for choice in &plan.choose {
            println!(
                "#{key} needs a `{}` label — one of: {} ({:?})",
                choice.axis,
                choice.candidates.join(", "),
                choice.reason
            );
        }
    }

    if dry_run || plan.remove.is_empty() {
        return Ok(());
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime for the issue provider: {error}"))?;
    runtime
        .block_on(backlog::relabel(&provider, &issue.key, &[], &plan.remove))
        .map_err(|error| error.to_string())?;

    println!("#{key} relabelled");
    Ok(())
}

/// What a `close` invocation asked for, before it becomes a
/// `stella_autonomy::Closure`.
///
/// A struct rather than nine positional parameters: clippy is right that nine
/// is too many, and the names are what make a call site readable.
pub(super) struct CloseRequest<'a> {
    pub(super) key: &'a str,
    pub(super) completed: bool,
    pub(super) not_planned: Option<&'a str>,
    pub(super) duplicate_of: Option<&'a str>,
    pub(super) partial: Option<&'a str>,
    pub(super) pr: Option<&'a str>,
    pub(super) commit: Option<&'a str>,
    pub(super) per: Option<&'a str>,
    pub(super) remaining: &'a [String],
}

/// `stella self-driving close` — end an issue, with a receipt that cites.
///
/// The refusal happens **before** the tracker is reached, so a closure missing
/// its citation cannot half-happen: `check_closure` is consulted first, and
/// only then does anything write.
pub(super) fn close_issue(req: CloseRequest<'_>) -> Result<(), String> {
    let key = req.key;

    // The change citation — a pull request, or a commit for the all-hands path.
    let change = match (req.pr, req.commit) {
        (Some(pr), _) => Some(Citation::PullRequest { key: pr.to_owned() }),
        (_, Some(sha)) => Some(Citation::Commit {
            sha: sha.to_owned(),
        }),
        _ => None,
    };

    // The authority citation. A `doc:` prefix names a document; anything else
    // is read as a context-record lineage id, which is what those look like.
    let authority = req.per.map(|raw| {
        if raw.starts_with("doc:") {
            Citation::Document {
                doc_id: raw.to_owned(),
            }
        } else {
            Citation::ContextRecord {
                lineage_id: raw.to_owned(),
            }
        }
    });

    let missing_change = || {
        format!(
            "#{key} cannot close as done without citing what carried it. Pass `--pr <key>`, \
             or `--commit <sha>` if it landed directly."
        )
    };

    let closure = if let Some(reason) = req.not_planned {
        Closure::NotPlanned {
            reason: reason.to_owned(),
            per: authority.ok_or_else(|| {
                format!(
                    "#{key} cannot close as not planned without citing what settles it. Pass \
                     `--per doc:<id>` or `--per <context-record-lineage-id>` — a reason with no \
                     authority behind it is an opinion, and the next person to hit this has no \
                     way to find out who decided."
                )
            })?,
        }
    } else if let Some(of) = req.duplicate_of {
        Closure::Duplicate { of: of.to_owned() }
    } else if let Some(done) = req.partial {
        Closure::Partial {
            done: done.to_owned(),
            by: change.ok_or_else(missing_change)?,
            remaining: req.remaining.to_vec(),
        }
    } else if req.completed {
        Closure::Fixed {
            by: change.ok_or_else(missing_change)?,
        }
    } else {
        return Err(
            "say how it is closing: --completed, --not-planned, --duplicate-of, or --partial"
                .into(),
        );
    };

    stella_autonomy::check_closure(&closure).map_err(|refusal| match refusal {
        ClosureRefusal::PartialWithNoRemainder => format!(
            "#{key} is only partly done and nothing is tracking the rest. File the remaining \
             work as its own issues first, then close with `--remaining <key>` for each. A \
             backlog that shrinks by more than the work finished is worse than one that does \
             not shrink."
        ),
        ClosureRefusal::MissingRationale { field } => format!(
            "#{key} cannot close with an empty `{field}` — a closed issue whose comment says \
             nothing is worse than an open one, because it looks handled."
        ),
        ClosureRefusal::WrongCitationKind { wanted } => format!(
            "#{key} needs {wanted} as its citation. A change and a decision are different \
             claims and are evidenced in different currencies."
        ),
    })?;

    let root = state::repo_root();
    let cfg = config::load(&root);
    let provider = crate::issue_provider::GhIssueProvider;
    let issue_key = stella_protocol::issue::IssueKey::from(key);
    let receipt = stella_autonomy::receipt(&closure);

    // Stella's canonical resolution, spelled the way THIS tracker spells it.
    let resolution = cfg
        .vocabulary
        .resolution(stella_autonomy::resolution_of(&closure))
        .to_owned();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime for the issue provider: {error}"))?;

    // A partial closure comments first and closes second: if the close fails,
    // the link to the remainder is already on the issue, which is the half a
    // human needs. The reverse order would leave a closed issue with no trail.
    // The close then goes out *bare* — the receipt is already on the issue, so
    // re-attaching it as the close's `--comment` would post the identical
    // signed receipt twice.
    if matches!(closure, Closure::Partial { .. }) {
        runtime
            .block_on(backlog::comment(
                &provider,
                &issue_key,
                &receipt,
                &cfg.attribution.issue_comment,
            ))
            .map_err(|error| error.to_string())?;

        runtime
            .block_on(backlog::close_bare(&provider, &issue_key, &resolution))
            .map_err(|error| error.to_string())?;
    } else {
        runtime
            .block_on(backlog::close_with_receipt(
                &provider,
                &issue_key,
                &receipt,
                &cfg.attribution.issue_comment,
                &resolution,
            ))
            .map_err(|error| error.to_string())?;
    }

    println!("closed #{key} as {resolution}");
    Ok(())
}
