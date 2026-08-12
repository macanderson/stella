//! An OS write-boundary around the graded tree, for shells that must not
//! reach it (#2931).
//!
//! [`super::confine`] audits the command *text* before the spawn, and its own
//! header is candid that a shell can compute a path at runtime and step around
//! any such check. That ceiling was reached in the field rather than in theory:
//! on `video-processing__pBadsUh` a worker spelled the graded tree as
//! `chr(47) + 'app'`, read and wrote through it five times, and the run's whole
//! output — 126 steps, 53.8 minutes — was then discarded by the post-hoc
//! sealed-bytes detector. A guard a `chr()` call defeats is not a guard, and
//! detect-then-discard is not a substitute for enforcement.
//!
//! # What this module is
//!
//! The boundary the text audit cannot be: the kernel refuses the write, so no
//! spelling of the path matters. Two facilities are driven, one per platform:
//!
//! * **macOS** — Seatbelt via `sandbox-exec -p`, denying `file-write*` on each
//!   graded subpath. The profile is inherited by every descendant of the
//!   shell, which is what makes it a boundary rather than a check on the first
//!   process.
//! * **Linux** — a private mount namespace (`unshare --user --map-root-user
//!   --mount`) in which each graded tree is bind-mounted back over itself
//!   read-only. `MS_RDONLY` is enforced against root, which the container
//!   case needs. The user-namespace pair is load-bearing, not decoration:
//!   changing a pre-existing mount's propagation type requires `CAP_SYS_ADMIN`
//!   in the user namespace that *owns* that mount, and an unprivileged CI
//!   runner process has no such capability in the initial namespace it was
//!   born into — `--user --map-root-user` gives it root, and therefore
//!   `CAP_SYS_ADMIN`, inside a namespace of its own before `--mount` runs
//!   (#3010).
//!
//! Neither is available everywhere, and a facility that is *present* is not
//! the same as one that *works* — an unprivileged container, a missing
//! `util-linux`, a hardened kernel. So availability is never assumed from the
//! platform: [`posture`] runs a **functional probe** once per process that
//! constructs a throwaway tree, attempts the exact escape through the wrapper,
//! and reports [`Mechanism`] only after observing the write fail and the
//! exemption below succeed. A recipe that is wrong on some host degrades that
//! host to [`Advisory`] instead of silently containing nothing.
//!
//! # Writes, not reads — and `.git` is exempt
//!
//! The ban is on **writes** only, matching the decision #2928 settled for the
//! text audit: a read cannot corrupt the tree the run is judged against, and
//! several workers' first instinct is `ls /app`. Both mechanisms above leave
//! reads alone.
//!
//! `<graded>/.git` is exempt from the ban, and the exemption is load-bearing
//! rather than a concession. A candidate is a `git worktree` of the graded
//! tree: it shares that object store, so a `git commit` the model runs inside
//! its own workspace writes objects under the graded tree's `.git`. Banning
//! that would break ordinary work to protect nothing — what adoption delivers
//! and what a grader reads is the *file tree*, and that is what is sealed
//! here. The ref namespace inside `.git` keeps exactly the protection it has
//! today: [`super::confine`]'s text audit of `refs/worktree/stella/` and the
//! object-destroying git subcommands. This module neither widens nor weakens
//! that.
//!
//! # What this does not claim
//!
//! `STELLA_BASH_SANDBOX` was removed in #1300 because it wrapped one tool and
//! called the result "sandbox: on" — a session-wide claim it did not have.
//! This module is deliberately narrower in what it says: it is a **write ban
//! on named directories**, armed only where a host declared a distinct graded
//! tree, and it is wired to the `bash` tool alone. `run_script`,
//! `start_process`, custom manifest tools and hook actions still spawn around
//! it — a **declared gap**, tracked in #2997, not a silence. The claim made
//! here is true of the surface it covers and is not extended to the session.
//!
//! `crates/stella-cli/src/candidate_ws/escape.rs` remains the post-hoc
//! backstop, and matters most exactly where the posture is [`Advisory`].

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The OS facility enforcing the ban.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mechanism {
    /// macOS Seatbelt, driven through `sandbox-exec -p <profile>`.
    Seatbelt,
    /// A Linux private mount namespace with a read-only bind over each
    /// graded tree.
    MountNamespace,
}

/// Why no OS boundary is in force, when none is.
///
/// Named rather than collapsed to a bool because the two have different
/// answers: one is a platform this module has no recipe for, the other is a
/// host where the recipe did not hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advisory {
    /// This platform ships no facility this module knows how to drive.
    UnsupportedPlatform,
    /// The facility is present but the functional probe did not observe it
    /// refuse the write — an unprivileged container, a missing tool, a
    /// kernel that declined.
    ProbeFailed,
}

/// Whether the graded-tree write ban is enforced by the OS on this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Posture {
    /// The kernel refuses the write, whatever the command text says.
    Enforced(Mechanism),
    /// Only [`super::confine`]'s text audit and the post-hoc sealed-bytes
    /// detector stand between the shell and the graded tree.
    Advisory(Advisory),
}

impl Posture {
    /// Whether an OS boundary is actually in force.
    pub fn is_enforced(&self) -> bool {
        matches!(self, Self::Enforced(_))
    }
}

impl std::fmt::Display for Posture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Enforced(Mechanism::Seatbelt) => {
                f.write_str("enforced by the macOS sandbox (sandbox-exec)")
            }
            Self::Enforced(Mechanism::MountNamespace) => {
                f.write_str("enforced by a private mount namespace (read-only bind)")
            }
            Self::Advisory(Advisory::UnsupportedPlatform) => {
                f.write_str("advisory: this platform has no write-boundary facility Stella drives")
            }
            Self::Advisory(Advisory::ProbeFailed) => f.write_str(
                "advisory: this host's write-boundary facility did not hold under probe",
            ),
        }
    }
}

/// This process's containment posture, probed once and cached.
///
/// The probe spawns one short-lived child. It is deliberately not run at
/// construction: [`super::ShellConfinement`] is plain data built in hundreds
/// of tests, and a constructor that shells out is a constructor nobody can
/// call cheaply.
pub fn posture() -> Posture {
    static POSTURE: OnceLock<Posture> = OnceLock::new();
    *POSTURE.get_or_init(probe)
}

/// Rewrite `(program, args)` so the child runs behind the OS write ban on
/// `graded_roots`, or `None` when nothing should change — no graded tree
/// declared, no usable root, or an [`Advisory`] posture.
///
/// Returning `None` rather than a silently-unwrapped argv keeps the caller's
/// two cases distinguishable: a wrap that happened and one that could not.
pub fn wrap(
    graded_roots: &[PathBuf],
    program: &str,
    args: &[String],
) -> Option<(String, Vec<String>)> {
    let Posture::Enforced(mechanism) = posture() else {
        return None;
    };
    let protected = usable_roots(graded_roots);
    if protected.is_empty() {
        return None;
    }
    let exempt: Vec<PathBuf> = protected
        .iter()
        .map(|root| root.join(".git"))
        .filter(|git| git.exists())
        .collect();
    wrap_argv(mechanism, &protected, &exempt, program, args)
}

/// The graded roots this module can express as a protected subpath.
///
/// A root with no component of its own (`/`, a relative path) is dropped for
/// the reason [`super::confine`] declines to arm on one: protecting
/// everything is the blanket refusal neither module exists to be. A non-UTF-8
/// path is dropped because neither an SBPL profile nor a generated shell
/// script can carry it — and dropping it is the honest direction, since the
/// caller then sees no wrap rather than a wrap that protects less than it says.
fn usable_roots(graded_roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for root in graded_roots {
        let usable = root.is_absolute()
            && root
                .components()
                .any(|c| matches!(c, std::path::Component::Normal(_)))
            && root.to_str().is_some();
        if usable && !out.contains(root) {
            out.push(root.clone());
        }
    }
    out
}

/// Build the wrapped argv. Pure — no probing, no filesystem — so every
/// mechanism's recipe is unit-testable on any platform.
fn wrap_argv(
    mechanism: Mechanism,
    protected: &[PathBuf],
    exempt: &[PathBuf],
    program: &str,
    args: &[String],
) -> Option<(String, Vec<String>)> {
    match mechanism {
        Mechanism::Seatbelt => {
            let profile = seatbelt_profile(protected, exempt)?;
            let mut argv = vec!["-p".to_string(), profile, program.to_string()];
            argv.extend(args.iter().cloned());
            Some((SANDBOX_EXEC.to_string(), argv))
        }
        Mechanism::MountNamespace => {
            let script = mount_script(protected, exempt)?;
            let argv = vec![
                "--user".to_string(),
                "--map-root-user".to_string(),
                "--mount".to_string(),
                "--propagation".to_string(),
                "private".to_string(),
                "--".to_string(),
                "/bin/sh".to_string(),
                "-c".to_string(),
                script,
                // $0 for the inner shell; the exec'd argv starts after it.
                "stella-contain".to_string(),
                program.to_string(),
            ]
            .into_iter()
            .chain(args.iter().cloned())
            .collect();
            Some((UNSHARE.to_string(), argv))
        }
    }
}

/// Absolute so a `PATH` the model controls cannot substitute the sandbox
/// launcher for something that launches nothing.
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Resolved off `PATH` rather than pinned: `unshare` lives in `/usr/bin` on
/// Debian and `/bin` on some images, and the functional probe — not a path
/// guess — is what decides whether it is trusted.
const UNSHARE: &str = "unshare";

/// A Seatbelt profile that allows everything except writes under the graded
/// trees, with each tree's `.git` allowed back.
///
/// `(allow default)` first and the denials after, because SBPL takes the
/// **last** matching rule: the ordering is what lets the `.git` allow override
/// the subpath deny that contains it.
fn seatbelt_profile(protected: &[PathBuf], exempt: &[PathBuf]) -> Option<String> {
    let mut profile = String::from("(version 1)\n(allow default)\n");
    for root in protected {
        profile.push_str(&format!(
            "(deny file-write* (subpath \"{}\"))\n",
            sbpl_escape(root.to_str()?)
        ));
    }
    for git in exempt {
        profile.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            sbpl_escape(git.to_str()?)
        ));
    }
    Some(profile)
}

/// Escape a path for an SBPL string literal.
fn sbpl_escape(path: &str) -> String {
    path.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The shell program run inside the fresh mount namespace: bind each graded
/// tree over itself, remount that bind read-only, then bind each `.git` back
/// rw before exec'ing the real command.
///
/// The order is the whole recipe. A bind mount does not inherit `MS_RDONLY`
/// from the mount it is taken through — only from the superblock — which is
/// why the read-only-ness needs its own `remount` step and why the `.git`
/// bind, taken *after* that remount, comes back writable. Any link failing
/// aborts with a distinctive status rather than exec'ing uncontained: a
/// namespace that half-applied must not look like one that applied.
fn mount_script(protected: &[PathBuf], exempt: &[PathBuf]) -> Option<String> {
    let mut script = String::new();
    for root in protected {
        let q = sh_quote(root.to_str()?);
        script.push_str(&format!(
            "mount --bind {q} {q} || exit 127\nmount -o remount,bind,ro {q} || exit 127\n"
        ));
    }
    for git in exempt {
        let q = sh_quote(git.to_str()?);
        script.push_str(&format!("mount --bind {q} {q} || exit 127\n"));
    }
    script.push_str("shift\nexec \"$@\"\n");
    Some(script)
}

/// Single-quote a word for `/bin/sh`, closing and reopening the quote around
/// any embedded quote. Total over every byte a UTF-8 path can hold.
fn sh_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// Decide the posture by *doing the escape* rather than by asking the platform
/// what it supports.
///
/// The probe builds a throwaway tree that stands in for a graded one, runs the
/// two operations whose outcomes define the contract — a write to a graded
/// file, which must fail, and a write under `.git`, which must succeed — and
/// accepts the mechanism only if it observed both. Anything else, including
/// the wrapper failing to launch at all, is [`Advisory`].
fn probe() -> Posture {
    let Some(mechanism) = platform_mechanism() else {
        return Posture::Advisory(Advisory::UnsupportedPlatform);
    };
    match probe_mechanism(mechanism) {
        Ok(true) => Posture::Enforced(mechanism),
        _ => Posture::Advisory(Advisory::ProbeFailed),
    }
}

fn platform_mechanism() -> Option<Mechanism> {
    if cfg!(target_os = "macos") {
        Some(Mechanism::Seatbelt)
    } else if cfg!(target_os = "linux") {
        Some(Mechanism::MountNamespace)
    } else {
        None
    }
}

/// `Ok(true)` only when the probe observed the ban hold *and* the `.git`
/// exemption work. Every I/O failure is `Err` and lands on [`Advisory`].
fn probe_mechanism(mechanism: Mechanism) -> std::io::Result<bool> {
    let dir = std::env::temp_dir().join(format!(
        "stella_contain_probe_{}_{}",
        std::process::id(),
        mechanism_tag(mechanism)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let graded = dir.join("graded");
    std::fs::create_dir_all(graded.join(".git"))?;
    let sentinel = graded.join("sentinel");
    std::fs::write(&sentinel, b"original")?;
    // Canonicalized because both mechanisms name a real path to the kernel,
    // and the platform temp dir is a symlink on macOS.
    let graded = graded.canonicalize()?;
    let outcome = run_probe(mechanism, &graded);
    let held = matches!(std::fs::read(graded.join("sentinel")), Ok(b) if b == b"original")
        && graded.join(".git").join("exempt").exists()
        && outcome.unwrap_or(false);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(held)
}

fn mechanism_tag(mechanism: Mechanism) -> &'static str {
    match mechanism {
        Mechanism::Seatbelt => "seatbelt",
        Mechanism::MountNamespace => "mountns",
    }
}

/// Run the escape attempt under the wrapper. `Ok(false)` when the wrapper
/// itself could not launch — which must not be read as containment.
fn run_probe(mechanism: Mechanism, graded: &Path) -> std::io::Result<bool> {
    let script = format!(
        "printf pwned > {sentinel} 2>/dev/null; printf ok > {exempt} 2>/dev/null; exit 0",
        sentinel = sh_quote(&graded.join("sentinel").to_string_lossy()),
        exempt = sh_quote(&graded.join(".git").join("exempt").to_string_lossy()),
    );
    let protected = vec![graded.to_path_buf()];
    let exempt = vec![graded.join(".git")];
    let Some((program, args)) = wrap_argv(
        mechanism,
        &protected,
        &exempt,
        "/bin/sh",
        &["-c".to_string(), script],
    ) else {
        return Ok(false);
    };
    let status = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    Ok(status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    /// The ordering that makes the exemption work at all: SBPL takes the last
    /// matching rule, so a `.git` allow written *before* the subpath deny that
    /// contains it would be overridden and every candidate `git commit` would
    /// fail.
    #[test]
    fn the_seatbelt_profile_denies_the_tree_then_allows_its_git_back() {
        let profile = seatbelt_profile(&roots(&["/app"]), &roots(&["/app/.git"])).expect("profile");
        let deny = profile
            .find("(deny file-write* (subpath \"/app\"))")
            .expect(&profile);
        let allow = profile
            .find("(allow file-write* (subpath \"/app/.git\"))")
            .expect(&profile);
        assert!(deny < allow, "{profile}");
        assert!(
            profile.starts_with("(version 1)\n(allow default)\n"),
            "{profile}"
        );
    }

    /// The read-only-ness needs its own remount, and the `.git` bind has to
    /// come after it — see [`mount_script`]'s doc for why the order is the
    /// recipe rather than a style choice.
    #[test]
    fn the_mount_script_remounts_read_only_before_binding_git_back() {
        let script = mount_script(&roots(&["/app"]), &roots(&["/app/.git"])).expect("script");
        let ro = script.find("remount,bind,ro '/app'").expect(&script);
        let git = script.find("mount --bind '/app/.git'").expect(&script);
        assert!(ro < git, "{script}");
        assert!(script.ends_with("shift\nexec \"$@\"\n"), "{script}");
        // A half-applied namespace must not exec the command uncontained.
        assert!(script.contains("|| exit 127"), "{script}");
    }

    /// Quoting is the difference between a protected path and an injection:
    /// both generators have to survive the characters a real path may hold.
    #[test]
    fn paths_are_escaped_for_the_language_they_land_in() {
        assert_eq!(sbpl_escape(r#"/a"b\c"#), r#"/a\"b\\c"#);
        assert_eq!(sh_quote("/a'b"), r"'/a'\''b'");
        let script = mount_script(&roots(&["/a'b"]), &[]).expect("script");
        assert!(script.contains(r"'/a'\''b'"), "{script}");
    }

    /// A root that would protect everything, a duplicate, and a relative path
    /// are all dropped — the same posture `confine::ShellConfinement` takes,
    /// and for the same reason.
    #[test]
    fn a_root_that_would_protect_everything_is_not_usable() {
        assert!(usable_roots(&roots(&["/"])).is_empty());
        assert!(usable_roots(&roots(&["relative"])).is_empty());
        assert_eq!(usable_roots(&roots(&["/app", "/app"])), roots(&["/app"]));
        assert_eq!(
            usable_roots(&roots(&["/app", "/srv/g"])),
            roots(&["/app", "/srv/g"])
        );
    }

    /// The wrapper preserves the command it wraps, in order — a containment
    /// that rewrote the argv would be a behaviour change, not a boundary.
    #[test]
    fn the_wrapped_argv_still_runs_the_original_command_last() {
        let args = vec!["-c".to_string(), "echo hi".to_string()];
        for mechanism in [Mechanism::Seatbelt, Mechanism::MountNamespace] {
            let (program, argv) =
                wrap_argv(mechanism, &roots(&["/app"]), &[], "bash", &args).expect("wrap");
            assert!(!program.is_empty());
            let tail = &argv[argv.len() - 3..];
            assert_eq!(tail, ["bash", "-c", "echo hi"], "{mechanism:?}: {argv:?}");
        }
    }

    /// With no graded tree declared there is nothing to contain, and the
    /// spawn must be left exactly as it was.
    #[test]
    fn nothing_is_wrapped_without_a_graded_tree() {
        assert!(wrap(&[], "bash", &["-c".to_string()]).is_none());
        assert!(wrap(&roots(&["/"]), "bash", &["-c".to_string()]).is_none());
    }
}
