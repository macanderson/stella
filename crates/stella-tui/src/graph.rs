//! Plain, backend-agnostic types for the code-graph inspector.
//!
//! `stella-tui` does **not** depend on `stella-graph`. The caller (the CLI,
//! which already owns a `CodeGraph`) queries `CodeGraph::neighbors(file)` and
//! converts the result into a [`GraphSnapshot`] it hands the deck. This keeps
//! the TUI decoupled — it renders data given to it, never reaching into a
//! backend — and lets the scenario driver synthesize a snapshot for demos.
//!
//! The snapshot is one of the two labeled **out-of-band read-models**: it is
//! not folded from `AgentEvent`s (a graph's structure isn't in the per-session
//! event stream). What this session did to the files in it is folded, so
//! [`GraphSnapshot::stamp_session_touches`] writes that half in from the deck's
//! ledger rather than the caller measuring it twice.

use stella_protocol::FileChangeKind;

/// A queried neighborhood of the code graph, ready to draw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphSnapshot {
    /// The symbol/file the neighborhood is centered on (human label).
    pub focus: String,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// Every indexed code file (root-relative, sorted) — the Graph tab's file
    /// picker lists these so any file can be re-rooted, not just the busiest
    /// one the caller seeds `focus` with. Rides along on the snapshot because
    /// `stella-tui` cannot reach the graph store itself (it renders data given
    /// to it); the caller fills it from `stella_graph::CodeGraph::all_files`.
    pub files: Vec<String>,
    /// Wall clock the caller spent answering this query, in milliseconds.
    ///
    /// `None` for a snapshot nobody timed — a synthesized demo, a scenario
    /// fixture — and the query bar then draws nothing rather than a zero,
    /// because "0ms" and "not measured" are different statements (#4335).
    ///
    /// Measured by the caller because only the caller knows what the query
    /// cost: the deck cannot see the index at all. The driver's
    /// `agent::graph_snapshot_focus` times its whole round-trip — opening
    /// `codegraph.db`, reading the neighborhood and the file list, closing it
    /// again — which is the number a reader is asking about when they wonder
    /// whether the tab is slow.
    pub query_ms: Option<u64>,
    /// The free-form query this neighborhood answers, when it answers one.
    ///
    /// `None` for a snapshot rooted on a *file* — the picker's re-root, the
    /// busiest-file seed, an `/init` rebuild — and the query bar then reads
    /// `file:<focus>` as it always has. `Some(text)` is what the user typed
    /// into the `q` box, echoed back by the producer rather than remembered
    /// deck-side, so the bar can never show a query that some later snapshot
    /// did not answer (#4335).
    pub query: Option<String>,
}

/// One node — a symbol or file. Cited by human label, never a raw UUID (L-C4).
#[derive(Clone, Debug, PartialEq)]
pub struct GraphNode {
    /// Human, inspectable label (the primary on-screen identifier).
    pub label: String,
    /// e.g. `"function"`, `"struct"`, `"trait"`, `"file"`, `"module"`.
    pub kind: String,
    /// Optional source location for the detail panel (`"src/x.rs:42"`).
    pub location: Option<String>,
    /// What this session did to the file this node lives in, when the session
    /// is one the deck folded. `None` on every snapshot as a *producer* builds
    /// it: a producer reads an index, and an index does not know what a
    /// conversation has been doing to the tree.
    ///
    /// Filled in by [`GraphSnapshot::stamp_session_touches`] from the deck's
    /// own file ledger, which is where a turn ordinal exists at all — see that
    /// method for why the stamp is a projection recomputed per frame rather
    /// than a value the snapshot arrives carrying.
    pub touch: Option<SessionTouch>,
}

/// What this session did to one file, and when: the `● hot` mark's tag in the
/// node list (SPEC 9.1) and the `edited turn 14` suffix on a node card's edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionTouch {
    /// The 1-based turn ordinal that last touched the file
    /// (`SessionModel::turns_completed` plus one, as
    /// [`crate::model::FileState::touched_turn`] records it).
    ///
    /// `None` for a path the ledger kept across a `/clear`: the touch happened,
    /// and the turn numbering it was counted in is gone. The node still marks
    /// hot; it carries no `turn N`, because there is no turn N to name.
    pub turn: Option<u32>,
    /// The most recent operation on the file, which [`Self::verb`] renders.
    pub kind: FileChangeKind,
}

impl SessionTouch {
    /// The past-tense verb the tag reads with: `created`, `edited`, `deleted`,
    /// `read`.
    ///
    /// `Modified` renders as `edited` rather than `modified` because SPEC 9.1
    /// writes the tag `edited turn 14`; the other three keep the protocol's own
    /// word, so nothing has to be looked up to read the tag.
    #[must_use]
    pub fn verb(&self) -> &'static str {
        match self.kind {
            FileChangeKind::Created => "created",
            FileChangeKind::Modified => "edited",
            FileChangeKind::Deleted => "deleted",
            FileChangeKind::Read => "read",
        }
    }

    /// The tag as a card renders it (`edited turn 14`), or `None` when the
    /// touch cannot name a turn.
    #[must_use]
    pub fn tag(&self) -> Option<String> {
        let turn = self.turn?;
        Some(format!("{} turn {turn}", self.verb()))
    }
}

/// One row of the session's file ledger, as the Graph tab reads it: which path,
/// what the session last did to it, and in which turn.
///
/// The deck's ledger is a `Vec<FileState>` keyed by path; this is the slice of
/// it [`GraphSnapshot::stamp_session_touches`] needs, so `stella-tui`'s graph
/// types stay free of the transcript model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileTouch {
    /// Root-relative path, as the `FileChange` event spelled it.
    pub path: String,
    pub touch: SessionTouch,
}

/// A directed edge between two [`GraphSnapshot::nodes`] by index.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphEdge {
    pub from: usize,
    pub to: usize,
    /// e.g. `"imports"`, `"calls"`, `"defines"`, `"references"`.
    pub kind: String,
}

impl GraphNode {
    /// Whether this node lives in `path` — the node *is* that file, its label
    /// is that file's basename, or its [`location`](Self::location) points
    /// inside it.
    ///
    /// One matcher for the `● hot` mark and for the session tag, so a node can
    /// never be hot without a tag or tagged without being hot.
    ///
    /// Both tests are anchored at a separator. A bare `starts_with` on the
    /// location matched `src/lib.rs.bak:3` against `src/lib.rs`, and a bare
    /// `ends_with` on the label matched `my_lib.rs` against `lib.rs` — the
    /// ledger's paths and the index's labels are both dense in near-misses like
    /// that. Anchoring also keeps this allocation-free, which matters because
    /// it runs over every node against every ledger row on every frame.
    #[must_use]
    pub fn lives_in(&self, path: &str) -> bool {
        let basename_of_path = path
            .strip_suffix(self.label.as_str())
            .is_some_and(|before| before.ends_with('/'));
        let inside_path = self.location.as_deref().is_some_and(|loc| {
            loc.strip_prefix(path)
                // `path:line` from the index, or the bare path when the
                // producer had no line to attach.
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(':'))
        });
        self.label == path || basename_of_path || inside_path
    }
}

/// The file ledger, indexed for [`GraphSnapshot::stamp_session_touches`].
///
/// Two of [`GraphNode::lives_in`]'s three arms are equality tests on a string
/// the ledger already holds, so they become lookups. Only the third — a node
/// whose `location` sits *inside* a ledger path — still needs to try
/// candidates, and it tries at most as many as the location has `:` in it
/// rather than scanning the ledger.
///
/// **Built per call, not cached.** The projection is recomputed every frame on
/// purpose (see `stamp_session_touches`), and a cached index would be the
/// invalidation rule ADR 0019 chose the per-frame projection to avoid. Building
/// it is one pass over the ledger; the scan it replaces was one pass *per node*.
///
/// # Why it stores indices rather than touches
///
/// `find` returned the FIRST matching row, and several rows can match one node
/// — a path and its basename twin, or two spellings of one file. Preserving
/// which row wins means comparing ledger positions across the three arms and
/// taking the lowest, so a rewrite that stamped "any match" would be a
/// behaviour change wearing a performance change's clothes.
struct LedgerIndex<'a> {
    /// Ledger position of the first row whose path is exactly this string.
    by_path: std::collections::HashMap<&'a str, usize>,
    /// Ledger position of the first row whose path *ends* with `/<basename>`.
    by_basename: std::collections::HashMap<&'a str, usize>,
}

impl<'a> LedgerIndex<'a> {
    fn of(ledger: &'a [FileTouch]) -> Self {
        let mut by_path = std::collections::HashMap::with_capacity(ledger.len());
        let mut by_basename = std::collections::HashMap::with_capacity(ledger.len());
        for (i, row) in ledger.iter().enumerate() {
            // `or_insert` and not `insert`: first wins, which is what `find`
            // did.
            by_path.entry(row.path.as_str()).or_insert(i);
            // `lives_in`'s middle arm is `path.strip_suffix(label)` ending in
            // `/`, which is exactly "the label IS this path's basename". A
            // path with no `/` has no basename match, and `rsplit_once`
            // returning `None` is that case.
            if let Some((_, base)) = row.path.rsplit_once('/') {
                by_basename.entry(base).or_insert(i);
            }
        }
        Self {
            by_path,
            by_basename,
        }
    }

    /// The first ledger row `node` lives in, by the same rules and the same
    /// tie-break `ledger.iter().find(|row| node.lives_in(&row.path))` used.
    fn first_match<'l>(&self, node: &GraphNode, ledger: &'l [FileTouch]) -> Option<&'l FileTouch> {
        let mut best: Option<usize> = None;
        let mut consider = |i: Option<usize>| {
            if let Some(i) = i {
                best = Some(best.map_or(i, |b| b.min(i)));
            }
        };

        // `self.label == path`
        consider(self.by_path.get(node.label.as_str()).copied());
        // `path.strip_suffix(label)` ending in `/` — the label IS the basename.
        consider(self.by_basename.get(node.label.as_str()).copied());
        // `location` is `path` or `path:line`. The candidates are the location
        // itself and every prefix ending just before a `:`, which is what
        // `strip_prefix(path)` accepted.
        if let Some(loc) = node.location.as_deref() {
            consider(self.by_path.get(loc).copied());
            for (i, _) in loc.match_indices(':') {
                consider(self.by_path.get(&loc[..i]).copied());
            }
        }

        best.and_then(|i| ledger.get(i))
    }
}

impl GraphSnapshot {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Write each node's [`touch`](GraphNode::touch) from `ledger`, the file
    /// ledger of the session on screen.
    ///
    /// The stamp is a **projection, recomputed every frame**, not a value the
    /// snapshot arrives carrying. A neighborhood is read once and then sits on
    /// screen for the rest of a session; a tag baked in at read time would
    /// still name turn 3 after turn 9 edited the same file, while the `● hot`
    /// mark beside it — derived live since it existed — had already moved on.
    /// Two readings of one ledger disagreeing on the same row is the defect
    /// this method exists to make impossible, so it reads the ledger where the
    /// mark does: at the moment of drawing.
    ///
    /// A node no ledger row matches keeps whatever it had, so a producer that
    /// *did* measure a session (a scenario fixture, a replayed neighborhood)
    /// is not blanked by a deck with an empty ledger. Where both have an
    /// answer the ledger wins: it is this session's own record of this
    /// session.
    pub fn stamp_session_touches(&mut self, ledger: &[FileTouch]) {
        let index = LedgerIndex::of(ledger);
        for node in &mut self.nodes {
            if let Some(row) = index.first_match(node, ledger) {
                node.touch = Some(row.touch);
            }
        }
    }

    /// Degree (edge count touching) of a node index — handy for sizing.
    pub fn degree(&self, node: usize) -> usize {
        self.edges
            .iter()
            .filter(|e| e.from == node || e.to == node)
            .count()
    }

    /// The [`files`](Self::files) that match a picker query — a case-insensitive
    /// substring test, preserving the sorted file order. An empty (or
    /// whitespace-only) query matches every file, so the picker opens on the
    /// full list. Both the picker's key handler (for selection bounds) and its
    /// renderer route through this one function so the highlighted row and the
    /// selected path can never disagree.
    pub fn matching_files(&self, query: &str) -> Vec<&str> {
        let needle = query.trim().to_lowercase();
        self.files
            .iter()
            .map(String::as_str)
            .filter(|f| needle.is_empty() || f.to_lowercase().contains(&needle))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    /// **The witness for the rewrite (#5221).** The index answers exactly what
    /// the linear scan answered, tie-break included.
    ///
    /// `stamp_session_touches` used to be
    /// `ledger.iter().find(|row| node.lives_in(&row.path))`, and `find` returns
    /// the FIRST match. Several rows can match one node — a path and its
    /// basename twin, two spellings of one file — so an index that stamped
    /// "any match" would be a behaviour change wearing a performance change's
    /// clothes, and the goldens would not catch it because they carry no such
    /// collision.
    ///
    /// Checked against the old implementation directly rather than against
    /// pinned expectations: the requirement is *equivalence*, and the fixture
    /// below contains every collision the rewrite could get wrong.
    #[test]
    fn the_ledger_index_agrees_with_the_scan_it_replaced() {
        let ledger: Vec<FileTouch> = [
            // A basename twin: both end in `/lib.rs`, and the FIRST must win.
            "crates/a/src/lib.rs",
            "crates/b/src/lib.rs",
            // The exact path of a node's label, later than its basename twin.
            "lib.rs",
            // A near-miss the rules must keep rejecting.
            "crates/a/src/my_lib.rs",
            // The location arm's target, and a decoy sharing its prefix.
            "crates/a/src/engine.rs",
            "crates/a/src/engine.rs.bak",
        ]
        .iter()
        .enumerate()
        .map(|(i, path)| FileTouch {
            path: (*path).to_string(),
            touch: SessionTouch {
                // A distinct turn per row, so "which row won" is visible in
                // the stamp rather than inferred.
                turn: Some(i as u32),
                kind: stella_protocol::FileChangeKind::Modified,
            },
        })
        .collect();

        let nodes = vec![
            // Matches by basename — two rows qualify, the first wins.
            GraphNode {
                label: "lib.rs".into(),
                kind: "file".into(),
                location: None,
                touch: None,
            },
            // Matches by location, with a `:line` suffix.
            GraphNode {
                label: "run_turn".into(),
                kind: "function".into(),
                location: Some("crates/a/src/engine.rs:412".into()),
                touch: None,
            },
            // The near miss: a node called `lib.rs` must not light up
            // `my_lib.rs`, and this node's own label matches nothing else.
            GraphNode {
                label: "my_lib.rs".into(),
                kind: "file".into(),
                location: None,
                touch: None,
            },
            // Nothing matches at all.
            GraphNode {
                label: "absent.rs".into(),
                kind: "file".into(),
                location: Some("crates/z/src/absent.rs:1".into()),
                touch: None,
            },
        ];

        // The implementation this replaced, kept here as the oracle.
        let scanned: Vec<Option<SessionTouch>> = nodes
            .iter()
            .map(|node| {
                ledger
                    .iter()
                    .find(|row| node.lives_in(&row.path))
                    .map(|row| row.touch)
            })
            .collect();

        let mut snapshot = GraphSnapshot {
            nodes: nodes.clone(),
            ..GraphSnapshot::default()
        };
        snapshot.stamp_session_touches(&ledger);
        let indexed: Vec<Option<SessionTouch>> =
            snapshot.nodes.iter().map(|node| node.touch).collect();

        assert_eq!(
            indexed, scanned,
            "the index and the scan must stamp the same row for every node"
        );
        // And the fixture is doing its job — a run where nothing matched would
        // pass the comparison above while proving nothing.
        assert!(
            scanned.iter().filter(|t| t.is_some()).count() >= 2,
            "the fixture must exercise real matches: {scanned:?}"
        );
    }

    /// **The measurement #5221 asks for**, at the documented bounds.
    ///
    /// `#[ignore]` because it is a timing, not an assertion: a wall-clock
    /// threshold in CI is a flake generator, and the number is only meaningful
    /// on a machine nobody is also compiling on. Run it by hand:
    ///
    /// ```text
    /// cargo test -p stella-tui --lib stamping_at_the_documented_bounds -- --ignored --nocapture
    /// ```
    ///
    /// The bounds are the real ones, not hypothetical: nodes are capped at
    /// `agent::graph_view`'s `QUERY_MAX_FRAMES` (256) and the ledger at
    /// `model::file_state`'s `MAX_TRACKED_FILES` (2048), so this is the worst
    /// frame the deck can be asked to draw — and it is the worst *kind* of
    /// worst, because no node matches any row. A match short-circuits `find`;
    /// a miss walks the whole ledger. Every one of the 524k `lives_in` calls
    /// here is a full scan.
    #[test]
    #[ignore = "a timing, not an assertion — see the doc comment"]
    fn stamping_at_the_documented_bounds_is_cheap_enough_to_do_every_frame() {
        const NODES: usize = 256;
        const LEDGER: usize = 2048;

        let mut snapshot = GraphSnapshot {
            nodes: (0..NODES)
                .map(|i| GraphNode {
                    label: format!("symbol_{i}"),
                    kind: "function".into(),
                    location: Some(format!("crates/some-crate/src/module_{i}.rs:{i}")),
                    touch: None,
                })
                .collect(),
            ..GraphSnapshot::default()
        };
        // Disjoint from every node's path, so nothing short-circuits.
        let ledger: Vec<FileTouch> = (0..LEDGER)
            .map(|i| FileTouch {
                path: format!("crates/other-crate/src/unrelated_{i}.rs"),
                touch: SessionTouch {
                    turn: Some(1),
                    kind: stella_protocol::FileChangeKind::Modified,
                },
            })
            .collect();

        // A few frames, so a single scheduling hiccup does not become the
        // reported number.
        const FRAMES: u32 = 20;
        let start = std::time::Instant::now();
        for _ in 0..FRAMES {
            snapshot.stamp_session_touches(&ledger);
        }
        let per_frame = start.elapsed() / FRAMES;

        println!(
            "stamp_session_touches: {NODES} nodes x {LEDGER} ledger rows, \
             {per_frame:?} per frame"
        );
        // No threshold asserted. The number goes on #5221; a reader who wants
        // a budget should add one with a `MEASURED:` marker and its own
        // argument for the ceiling.
        //
        // What dominates it now is BUILDING the index — two maps over 2048
        // rows, once per frame — not the lookups. That is the trade ADR 0019
        // asks for: a per-frame projection with no invalidation rule. If this
        // ever needs to be faster, the next move is the generation counter the
        // issue describes, and it needs its own argument for reintroducing
        // invalidation.
    }

    use super::*;

    fn snapshot_with_files(files: &[&str]) -> GraphSnapshot {
        GraphSnapshot {
            focus: "root".into(),
            nodes: Vec::new(),
            edges: Vec::new(),
            files: files.iter().map(|s| s.to_string()).collect(),
            query_ms: None,
            query: None,
        }
    }

    #[test]
    fn an_empty_query_matches_every_file_in_order() {
        let snap = snapshot_with_files(&["src/a.rs", "src/b.rs", "src/c.rs"]);
        assert_eq!(
            snap.matching_files(""),
            vec!["src/a.rs", "src/b.rs", "src/c.rs"]
        );
        // Whitespace-only is treated as empty, not as a literal space search.
        assert_eq!(
            snap.matching_files("   "),
            vec!["src/a.rs", "src/b.rs", "src/c.rs"]
        );
    }

    #[test]
    fn a_query_narrows_case_insensitively_by_substring() {
        let snap = snapshot_with_files(&["src/Auth.rs", "src/db/pool.rs", "README.md"]);
        // Case-insensitive.
        assert_eq!(snap.matching_files("auth"), vec!["src/Auth.rs"]);
        // Matches anywhere in the path, not just the basename.
        assert_eq!(snap.matching_files("db/"), vec!["src/db/pool.rs"]);
        // No match yields an empty list (the picker then shows its empty hint).
        assert!(snap.matching_files("zzz").is_empty());
    }

    fn node(label: &str, kind: &str, location: Option<&str>) -> GraphNode {
        GraphNode {
            label: label.into(),
            kind: kind.into(),
            location: location.map(str::to_string),
            touch: None,
        }
    }

    #[test]
    fn a_touch_reads_as_the_spec_tag_and_says_nothing_without_a_turn() {
        let edited = SessionTouch {
            turn: Some(14),
            kind: FileChangeKind::Modified,
        };
        assert_eq!(edited.verb(), "edited");
        assert_eq!(edited.tag().as_deref(), Some("edited turn 14"));

        // The other three keep the protocol's own word.
        for (kind, verb) in [
            (FileChangeKind::Created, "created"),
            (FileChangeKind::Deleted, "deleted"),
            (FileChangeKind::Read, "read"),
        ] {
            let touch = SessionTouch {
                turn: Some(2),
                kind,
            };
            assert_eq!(touch.verb(), verb);
            assert_eq!(touch.tag(), Some(format!("{verb} turn 2")));
        }

        // A path the ledger kept across a `/clear` was touched by a turn
        // numbering that no longer exists, so it names no turn.
        assert_eq!(
            SessionTouch {
                turn: None,
                kind: FileChangeKind::Modified,
            }
            .tag(),
            None
        );
    }

    #[test]
    fn a_node_matches_the_file_it_is_the_file_named_by_or_defined_in() {
        let path = "crates/stella-protocol/src/lib.rs";
        // The node *is* the file, by full path and by basename.
        assert!(node(path, "file", None).lives_in(path));
        assert!(node("lib.rs", "file", None).lives_in(path));
        // A symbol defined in it, via its location — with a line and without.
        assert!(node("Attachment", "struct", Some(&format!("{path}:48"))).lives_in(path));
        assert!(node("Attachment", "struct", Some(path)).lives_in(path));
        // A same-named file in another crate is a different node.
        assert!(!node("crates/stella-tui/src/lib.rs", "file", None).lives_in(path));
        // Near-misses on either side of the separator do not match: the ledger
        // and the index are both dense in these.
        assert!(!node("my_lib.rs", "file", None).lives_in(path));
        assert!(!node("Backup", "struct", Some(&format!("{path}.bak:3"))).lives_in(path));
    }

    #[test]
    fn the_ledger_stamps_the_nodes_it_names_and_leaves_the_rest_alone() {
        let fixture = SessionTouch {
            turn: Some(9),
            kind: FileChangeKind::Created,
        };
        let mut snap = GraphSnapshot {
            focus: "src/a.rs".into(),
            nodes: vec![
                node("src/a.rs", "file", None),
                node("helper", "function", Some("src/b.rs:12")),
                // A node a producer already measured a session for.
                GraphNode {
                    touch: Some(fixture),
                    ..node("src/c.rs", "file", None)
                },
            ],
            edges: Vec::new(),
            files: Vec::new(),
            query_ms: None,
            query: None,
        };
        let ledger = vec![FileTouch {
            path: "src/b.rs".into(),
            touch: SessionTouch {
                turn: Some(3),
                kind: FileChangeKind::Modified,
            },
        }];
        snap.stamp_session_touches(&ledger);

        assert_eq!(snap.nodes[0].touch, None, "the ledger never named src/a.rs");
        assert_eq!(
            snap.nodes[1].touch.map(|t| t.verb()),
            Some("edited"),
            "the symbol defined in src/b.rs wears the row's touch"
        );
        assert_eq!(
            snap.nodes[2].touch,
            Some(fixture),
            "a producer's own measurement survives a ledger that says nothing"
        );

        // Re-stamping from a ledger that has moved on replaces the tag rather
        // than keeping the older reading beside a `● hot` mark that has not.
        let ledger = vec![FileTouch {
            path: "src/b.rs".into(),
            touch: SessionTouch {
                turn: Some(11),
                kind: FileChangeKind::Deleted,
            },
        }];
        snap.stamp_session_touches(&ledger);
        assert_eq!(
            snap.nodes[1].touch.and_then(|t| t.tag()).as_deref(),
            Some("deleted turn 11")
        );
    }
}
