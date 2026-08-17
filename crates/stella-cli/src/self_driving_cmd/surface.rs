//! `stella self-driving surface` — print the host-verb surface this build
//! carries (`doc:pipeline-as-plugins` §10, work item D2).
//!
//! The table itself is [`stella_autonomy::HOST_SURFACE`], which argues in its
//! own module docs why it is declared at all and why it lives in the leaf
//! crate rather than here. This file is the two halves that must live beside
//! the argument parser: the rendering, and the check that the declaration is
//! **true of this binary**.
//!
//! `surface_matches_the_argument_tree` is that check, and it is the reason
//! the declaration is worth trusting. It walks the real
//! [`super::SelfDrivingCmd`] clap tree and compares its leaves against
//! [`stella_autonomy::HOST_SURFACE`] in **both** directions — a verb added to
//! the CLI without a row fails, and a row naming a verb this build does not
//! carry fails. Without the second direction the surface would be a document
//! that only ever grows; without the first it would be a document that
//! silently stops describing the program.

use serde::Serialize;
use stella_autonomy::{HOST_SURFACE, HOST_SURFACE_VERSION, HostVerb};

use crate::query_format::{QueryFormat, Versioned};

/// The JSON payload: the version a host branches on, then the verbs.
///
/// A struct rather than an ad-hoc `json!` so the version leads the object and
/// the rows keep the exact keys [`HostVerb`] declares — a host reading
/// `surface_version` off a hand-built map would be reading a second spelling
/// of the same fact.
#[derive(Serialize)]
struct Surface<'a> {
    surface_version: u32,
    verbs: &'a [HostVerb],
}

/// Render the host surface as human text or the versioned query envelope.
pub(super) fn surface(format: QueryFormat) -> Result<(), String> {
    if format == QueryFormat::Json {
        let payload = Versioned::new(Surface {
            surface_version: HOST_SURFACE_VERSION,
            verbs: HOST_SURFACE,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
        return Ok(());
    }

    println!("self-driving host surface v{HOST_SURFACE_VERSION}");
    println!("  a driver outside this binary calls these verbs; `emits` is what it parses\n");
    let width = HOST_SURFACE
        .iter()
        .map(|verb| verb.path.len())
        .max()
        .unwrap_or(0);
    for verb in HOST_SURFACE {
        println!(
            "  {:<width$}  {:<17}  {}",
            verb.path,
            verb.emits.as_str(),
            verb.summary
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory as _;

    use super::*;

    /// Clap's own name for a subcommand tree's leaves, as a host would type
    /// them: `"cycle begin"`, not `"cycle"`.
    fn leaf_paths(command: &clap::Command, prefix: &str) -> Vec<String> {
        let mut paths = Vec::new();
        for sub in command.get_subcommands() {
            // `help` is clap's own, generated for every node; it is not a
            // verb of this program and a host never calls it.
            if sub.get_name() == "help" {
                continue;
            }
            let path = if prefix.is_empty() {
                sub.get_name().to_string()
            } else {
                format!("{prefix} {}", sub.get_name())
            };
            if sub.has_subcommands() {
                paths.extend(leaf_paths(sub, &path));
            } else {
                paths.push(path);
            }
        }
        paths
    }

    /// The clap tree as the binary actually parses it.
    fn argument_tree_paths() -> Vec<String> {
        // Built through the real top-level parser rather than by augmenting a
        // bare `Command`, so the tree walked here is the one `stella` ships
        // — including anything an attribute on the enclosing `Cli` changes.
        let cli = crate::Cli::command();
        let self_driving = cli
            .get_subcommands()
            .find(|sub| sub.get_name() == "self-driving")
            .cloned()
            .expect("`stella self-driving` is a subcommand of the shipping CLI");
        leaf_paths(&self_driving, "")
    }

    /// **The D2 guard, in both directions.** A verb this build carries with
    /// no declared row is a contract a host cannot discover; a declared row
    /// this build does not carry is a promise the binary breaks. Neither is
    /// detectable by reading one side.
    ///
    /// The comparison itself is [`stella_autonomy::surface_drift`], and is
    /// exercised there against drift this tree does not contain — this test
    /// supplies the one thing that crate cannot see, which is what the
    /// shipping argument parser actually carries.
    #[test]
    fn surface_matches_the_argument_tree() {
        let carried = argument_tree_paths();
        let carried: Vec<&str> = carried.iter().map(String::as_str).collect();
        let drift = stella_autonomy::surface_drift(&carried);

        assert!(
            drift.undeclared.is_empty(),
            "these verbs ship with no row in stella_autonomy::HOST_SURFACE, so a host \
             driving this loop cannot discover them: {:?}",
            drift.undeclared
        );
        assert!(
            drift.absent.is_empty(),
            "HOST_SURFACE declares verbs this build does not carry, so a host that \
             checked the surface would still get `unrecognized subcommand`: {:?}",
            drift.absent
        );
    }

    /// The version is what a host branches on, so it must be present and
    /// first — a payload whose version a consumer has to hunt for is one a
    /// consumer skips checking.
    #[test]
    fn the_json_payload_leads_with_both_versions() {
        let rendered = serde_json::to_string(&Versioned::new(Surface {
            surface_version: HOST_SURFACE_VERSION,
            verbs: &HOST_SURFACE[..1],
        }))
        .expect("serializes");
        assert!(
            rendered.starts_with(r#"{"schema_version":1,"surface_version":1,"verbs":[{"path":"#),
            "{rendered}"
        );
    }
}
