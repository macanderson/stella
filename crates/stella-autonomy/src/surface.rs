//! The **host surface**: the verbs an outside program drives this loop
//! through, declared as data (`doc:pipeline-as-plugins` §10, work item D2).
//!
//! # Why a declaration rather than a `--help` probe
//!
//! §10 settles that self-driving is a **host**, not a wrapper: it drives
//! Stella from outside, the way `scripts/self-driving.sh` already does,
//! delegating each decision to a `stella self-driving …` verb (#1548). That
//! makes the verb set a **contract with a caller that is not this
//! repository** — and until this module existed, nothing in the tree said so.
//! The contract was the union of whatever the shell driver happened to call,
//! discoverable only by reading a `case` statement, and the driver's own
//! compatibility check was:
//!
//! ```text
//! stella self-driving --help >/dev/null 2>&1
//! ```
//!
//! which answers "does this build have the subcommand at all" and nothing
//! about *which* verbs it carries or what they emit. A third-party host
//! written against a verb a later build renamed learns that from clap's
//! `unrecognized subcommand` in the middle of a cycle, which reads as a bug
//! in the host rather than a version skew. So the surface is declared here,
//! carries a [`HOST_SURFACE_VERSION`] a caller can branch on, and is printed
//! by `stella self-driving surface` for a host that wants to check before it
//! starts rather than after it fails.
//!
//! # Why the declaration lives in this crate
//!
//! Same reason the folds do (see the crate docs): a leaf both readers can
//! link. `stella-cli` renders the surface and is checked against it — the
//! parity test in `stella-cli`'s `self_driving_cmd::surface` walks the real
//! clap tree and fails in **both** directions, so a verb cannot ship
//! undeclared and a declared verb cannot be silently removed — and anything
//! else that has to reason about the host surface (an installer checking
//! compatibility, the Observatory naming the verb that wrote a record) reads
//! the same table instead of a second copy. `stella-cli` could hold it, but
//! then the CLI's own argument tree would be both the claim and its own
//! evidence, which is not a check.
//!
//! # What this module deliberately does not do
//!
//! It does not execute anything, and it holds no flags. A verb's *flags* are
//! clap's business and change far more often than the verb set — pinning
//! every flag here would make this table a mirror of the argument parser and
//! it would drift the first time someone added an option. What a host binds
//! to is the **verb path** and the **shape of what comes back on stdout**,
//! which is what [`HostVerb`] carries.

use serde::Serialize;

/// The version of the host surface this build declares.
///
/// A host reads it once, at start-up, and refuses a build it was not written
/// against instead of discovering the mismatch mid-cycle.
///
/// **Bump rules**, the same shape as the CLI's query envelope and for the
/// same reason: increment only when a host written against the previous
/// version could break — a verb removed or renamed, or a verb's [`Emits`]
/// changed. Never bump for a purely additive verb; a host must ignore verbs
/// it does not recognize, exactly as it must ignore JSON keys it does not
/// recognize.
pub const HOST_SURFACE_VERSION: u32 = 1;

/// The shape a verb writes to stdout — the half of the contract a host parses.
///
/// Closed rather than free text, so a host can branch on it and so a new
/// output shape is a deliberate addition to this enum rather than a sentence
/// somebody has to notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Emits {
    /// `KEY=value` lines the caller `eval`s, one per line, values already
    /// quoted where they can contain a space.
    ///
    /// The reason this shape exists at all: a driver that re-derived the
    /// numbers from prose would be the second implementation this crate was
    /// created to prevent.
    ShellAssignments,
    /// Human text by default; the CLI's versioned query envelope
    /// (`{"schema_version":N,"rows":[…]}`) under `--format json`.
    QueryEnvelope,
    /// A bare JSON document on stdout with no envelope around it. Distinct
    /// from [`Emits::QueryEnvelope`] on purpose — a host that assumed an
    /// envelope here would read `.rows` off an object that has none.
    Json,
    /// Human-readable text. A host may show it to a person or grep it; it is
    /// not a parsing contract.
    Text,
    /// The answer **is** the exit status, and stdout is the explanation for a
    /// human. `watch` is the case: exit 0 is WAKE and exit 1 is SLEEP, which
    /// is why a driver spells it `watch || continue` rather than treating a
    /// non-zero status as a failure.
    ExitStatus,
}

impl Emits {
    /// The kebab-case wire spelling, so a renderer never re-spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Emits::ShellAssignments => "shell-assignments",
            Emits::QueryEnvelope => "query-envelope",
            Emits::Json => "json",
            Emits::Text => "text",
            Emits::ExitStatus => "exit-status",
        }
    }
}

/// One verb on the host surface.
///
/// `Serialize` but deliberately **not** `Deserialize`: every field borrows
/// `'static`, so a derived reader could only ever produce a table this binary
/// already holds. A host outside the process parses the JSON in its own
/// language, and a Rust caller reads [`HOST_SURFACE`] — the same bytes,
/// without the parse that could fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HostVerb {
    /// The verb path after `stella self-driving`, segments separated by a
    /// single space — `"plan"`, `"cycle begin"`, `"run stamp-cycle-pid"`.
    ///
    /// One string rather than a list because it is what a host writes on a
    /// command line; [`HostVerb::segments`] gives the parsed form back to the
    /// one caller that needs it.
    pub path: &'static str,
    /// What the verb writes to stdout.
    pub emits: Emits,
    /// One line a host can show a person, in the terms the loop uses.
    pub summary: &'static str,
}

impl HostVerb {
    /// The path's segments, as a subcommand walk consumes them.
    pub fn segments(&self) -> impl Iterator<Item = &'static str> {
        self.path.split(' ')
    }
}

/// Every verb on the host surface, in the order the loop's own cycle uses
/// them: size the cycle, read state, run it, decide whether to run another,
/// then the run-lifecycle and reporting verbs a driver wraps around all of
/// that.
///
/// Declaration order is the rendering order and is not sorted: a host reading
/// `stella self-driving surface` should see the cycle in the sequence it
/// happens, which is information a sort would destroy.
pub const HOST_SURFACE: &[HostVerb] = &[
    HostVerb {
        path: "surface",
        emits: Emits::QueryEnvelope,
        summary: "this table: the verbs this build carries and what each emits",
    },
    HostVerb {
        path: "plan",
        emits: Emits::ShellAssignments,
        summary: "size this cycle to the machine it is running on",
    },
    HostVerb {
        path: "state",
        emits: Emits::QueryEnvelope,
        summary: "the loop's durable state: cycle counter, dry streak, recent ledger records",
    },
    HostVerb {
        path: "cycle begin",
        emits: Emits::ShellAssignments,
        summary: "allocate the next cycle and emit its context",
    },
    HostVerb {
        path: "cycle end",
        emits: Emits::Json,
        summary: "append the cycle's ledger record, recalibrate, advance a dry lens",
    },
    HostVerb {
        path: "watch",
        emits: Emits::ExitStatus,
        summary: "low-duty sentinel once every lens is dry: exit 0 WAKE, exit 1 SLEEP",
    },
    HostVerb {
        path: "metrics",
        emits: Emits::Text,
        summary: "the ledger folded into the loop's evidence about itself",
    },
    HostVerb {
        path: "aperture",
        emits: Emits::Text,
        summary: "read, advance, reset or list the audit lens ladder",
    },
    HostVerb {
        path: "seen",
        emits: Emits::Text,
        summary: "the finding dedup set: digest, filter, record, count",
    },
    HostVerb {
        path: "calibrate",
        emits: Emits::Text,
        summary: "move the AIMD ceilings from evidence, or show them",
    },
    HostVerb {
        path: "queue",
        emits: Emits::QueryEnvelope,
        summary: "the ranked defect queue this cycle draws its batch from",
    },
    HostVerb {
        path: "file",
        emits: Emits::Text,
        summary: "file a finding as an issue — refused unless it matches this workspace's convention",
    },
    HostVerb {
        path: "run start",
        emits: Emits::ShellAssignments,
        summary: "begin a run — the multi-cycle unit a person starts and stops",
    },
    HostVerb {
        path: "run end",
        emits: Emits::Text,
        summary: "close the live run with a status and a reason",
    },
    HostVerb {
        path: "run cancel",
        emits: Emits::Text,
        summary: "close the live run as cancelled — a person stopped it",
    },
    HostVerb {
        path: "run list",
        emits: Emits::Text,
        summary: "every run, folded: status, cycles, yield, live phase",
    },
    HostVerb {
        path: "run stamp-cycle-pid",
        emits: Emits::Text,
        summary: "record (or clear) the pid executing the in-flight cycle",
    },
    HostVerb {
        path: "runs",
        emits: Emits::Text,
        summary: "shorthand for `run list`",
    },
    HostVerb {
        path: "phase",
        emits: Emits::Text,
        summary: "stamp the live run's phase, so a status light becomes a workflow",
    },
];

/// The verb with this exact path, if this build carries it.
///
/// The lookup a host does before it binds: "is `cycle begin` here, and does
/// it still emit what I parse?" A linear scan over a table this size is
/// faster than any map that would have to be built to serve it.
#[must_use]
pub fn host_verb(path: &str) -> Option<&'static HostVerb> {
    HOST_SURFACE.iter().find(|verb| verb.path == path)
}

/// The two ways a program can disagree with the surface it declares.
///
/// Both directions, because each hides a different failure and neither is
/// visible from one side of the comparison. Named separately rather than
/// summed into a bool so a caller's message can say *which* verbs, which is
/// the whole difference between a failing test somebody fixes and a failing
/// test somebody re-reads three times.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SurfaceDrift<'a> {
    /// Verb paths the program carries that [`HOST_SURFACE`] does not declare
    /// — a contract a host has no way to discover, so it will never be
    /// called by anything but this repository's own driver.
    pub undeclared: Vec<&'a str>,
    /// Declared paths the program does not carry — a promise the binary
    /// breaks, and the worse of the two: a host that dutifully checked the
    /// surface first still gets `unrecognized subcommand`.
    pub absent: Vec<&'static str>,
}

impl SurfaceDrift<'_> {
    /// Whether the program and its declaration agree exactly.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.undeclared.is_empty() && self.absent.is_empty()
    }
}

/// Compare the verb paths a program actually carries against
/// [`HOST_SURFACE`].
///
/// Pure and order-insensitive: `carried` is whatever a caller enumerated from
/// its own argument parser, in whatever order that parser walks. The one
/// caller that matters is `stella-cli`'s `self_driving_cmd::surface` test,
/// which walks the real clap tree — the comparison lives here rather than
/// there so it can be exercised against **drift that does not exist in this
/// tree**, which is the only way to know the guard bites at all. A guard
/// whose only evidence is that it passes on a tree where it should pass has
/// not been tested.
#[must_use]
pub fn surface_drift<'a>(carried: &[&'a str]) -> SurfaceDrift<'a> {
    SurfaceDrift {
        undeclared: carried
            .iter()
            .copied()
            .filter(|path| host_verb(path).is_none())
            .collect(),
        absent: HOST_SURFACE
            .iter()
            .map(|verb| verb.path)
            .filter(|declared| !carried.contains(declared))
            .collect(),
    }
}
