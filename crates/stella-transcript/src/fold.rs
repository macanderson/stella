//! Three independent fold levels, and the rules that keep them independent.
//!
//! The design constraint that shapes this whole module: **a fold is a property
//! of the reader's session, not of the transcript.** So the state is a set of
//! explicit overrides layered over a computed default, rather than a mutable
//! flag on each node. That is what makes "collapsing a parent preserves child
//! fold state" fall out for free — collapsing a turn writes one entry for the
//! turn and touches nothing below it, so re-expanding restores exactly the
//! steps the reader had open.
//!
//! The alternative — walking the subtree and clearing children — was tried
//! mentally and rejected: it makes the operation lossy, and a reader who opens
//! a turn to re-read the one step they had expanded finds all forty closed.

use std::collections::BTreeMap;

use crate::model::{NodeId, Run, Status};

/// The three levels a reader can collapse to, cycled with `z`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Zoom {
    /// Turn headers only. A 40-step run is four lines.
    Turns,
    /// Turn headers plus one-line step digests. The default, and the level the
    /// "40-step run fits on one screen" acceptance check is measured at.
    #[default]
    Steps,
    /// Everything open, including outputs and every hunk.
    Everything,
}

impl Zoom {
    /// The next preset in the cycle.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Zoom::Turns => Zoom::Steps,
            Zoom::Steps => Zoom::Everything,
            Zoom::Everything => Zoom::Turns,
        }
    }

    /// The label the zoom control renders.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Zoom::Turns => "Turns",
            Zoom::Steps => "Steps",
            Zoom::Everything => "Everything",
        }
    }
}

/// A reader's fold state over one run.
///
/// Cheap to clone and to serialise; holds only the overrides, so an untouched
/// transcript carries an empty map.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FoldState {
    zoom: Zoom,
    overrides: BTreeMap<NodeId, bool>,
}

impl FoldState {
    /// A fresh state at the default zoom.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The current zoom preset.
    #[must_use]
    pub fn zoom(&self) -> Zoom {
        self.zoom
    }

    /// Cycle the zoom preset.
    ///
    /// This clears the reader's per-node overrides, and that is deliberate: a
    /// zoom preset is a "put the whole document at this level" instruction, and
    /// honouring stale overrides underneath it would make the preset
    /// unpredictable — press `z` twice and you would get two different
    /// documents depending on what you had clicked an hour ago.
    pub fn cycle_zoom(&mut self) {
        self.zoom = self.zoom.next();
        self.overrides.clear();
    }

    /// Set the zoom preset explicitly.
    pub fn set_zoom(&mut self, zoom: Zoom) {
        self.zoom = zoom;
        self.overrides.clear();
    }

    /// Whether `node` renders expanded, given the run it belongs to.
    ///
    /// Resolution order: a status that pins the node open wins over everything,
    /// then the reader's explicit override, then the zoom default.
    #[must_use]
    pub fn is_open(&self, run: &Run, node: NodeId) -> bool {
        if pinned_open(run, node) {
            return true;
        }
        if let Some(&explicit) = self.overrides.get(&node) {
            return explicit;
        }
        self.default_open(run, node)
    }

    /// Flip one node.
    ///
    /// A pinned-open node absorbs the toggle rather than closing: the reader is
    /// told, by the node simply not closing, that the failure is not dismissable
    /// from here. Returns whether the node is open afterwards.
    pub fn toggle(&mut self, run: &Run, node: NodeId) -> bool {
        let next = !self.is_open(run, node);
        self.overrides.insert(node, next);
        self.is_open(run, node)
    }

    /// Open one node explicitly.
    pub fn open(&mut self, node: NodeId) {
        self.overrides.insert(node, true);
    }

    /// Close one node explicitly. A pinned node stays open regardless — the
    /// override is recorded but loses to the pin, so re-opening after the run
    /// finishes does not surprise the reader with a stale close.
    pub fn close(&mut self, node: NodeId) {
        self.overrides.insert(node, false);
    }

    /// Expand every output inside one step — the `e` key.
    pub fn expand_outputs_in_step(&mut self, run: &Run, turn: usize, step: usize) {
        self.overrides.insert(NodeId::Output { turn, step }, true);
        let files = run
            .turns
            .get(turn)
            .and_then(|t| t.steps.get(step))
            .and_then(|s| s.call.as_ref())
            .map_or(0, |c| c.files.len());
        for file in 0..files {
            self.overrides
                .insert(NodeId::File { turn, step, file }, true);
        }
    }

    /// What this node does with no override, at the current zoom.
    fn default_open(&self, run: &Run, node: NodeId) -> bool {
        match (self.zoom, node) {
            // A turn is open at every zoom except `Turns` — and even there, the
            // last turn stays open, because a reader arriving at a finished run
            // is nearly always looking at what just happened.
            (Zoom::Turns, NodeId::Turn(t)) => is_last_turn(run, t),
            (_, NodeId::Turn(t)) => is_last_turn(run, t) || run.turns.len() == 1,
            (Zoom::Everything, _) => true,
            (Zoom::Turns, _) => false,
            // At `Steps`, a step digest is visible but its body is not, and
            // prose folds to its first sentence.
            (Zoom::Steps, NodeId::Step { .. }) => false,
            (Zoom::Steps, NodeId::Output { .. } | NodeId::Prose { .. }) => false,
            // A file diff is the *point* of a mutation call, so when its step is
            // open the diff is open with it; individual hunks fold on their own.
            (Zoom::Steps, NodeId::File { .. }) => true,
            (Zoom::Steps, NodeId::Hunk { .. }) => true,
        }
    }
}

/// Whether a node refuses to close, whatever the reader or the zoom says.
fn pinned_open(run: &Run, node: NodeId) -> bool {
    match node {
        NodeId::Turn(t) => run.turns.get(t).is_some_and(|turn| {
            turn.status == Status::Running || turn.steps.iter().any(|s| s.status().pins_open())
        }),
        NodeId::Step { turn, step } | NodeId::Output { turn, step } => run
            .turns
            .get(turn)
            .and_then(|t| t.steps.get(step))
            .is_some_and(|s| s.status().pins_open()),
        NodeId::Prose { .. } | NodeId::File { .. } | NodeId::Hunk { .. } => false,
    }
}

fn is_last_turn(run: &Run, index: usize) -> bool {
    !run.turns.is_empty() && index + 1 == run.turns.len()
}

/// A keyboard command, resolved from a key press.
///
/// Kept as data rather than as a match inside a renderer so both surfaces bind
/// the same vocabulary, and so the bindings are testable without a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// `j` — move the cursor to the next step.
    NextStep,
    /// `k` — move the cursor to the previous step.
    PrevStep,
    /// `→` / `l` — expand the node under the cursor.
    Expand,
    /// `←` / `h` — collapse the node under the cursor.
    Collapse,
    /// `z` — cycle the zoom preset.
    CycleZoom,
    /// `e` — expand every output in the step under the cursor.
    ExpandOutputs,
    /// `c` — copy the invocation under the cursor.
    CopyInvocation,
}

impl Command {
    /// Resolve a key press. `None` for a key this view does not bind.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "j" | "down" => Command::NextStep,
            "k" | "up" => Command::PrevStep,
            "l" | "right" | "enter" => Command::Expand,
            "h" | "left" => Command::Collapse,
            "z" => Command::CycleZoom,
            "e" => Command::ExpandOutputs,
            "c" => Command::CopyInvocation,
            _ => return None,
        })
    }
}

/// A cursor over the run's steps, for `j`/`k`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor {
    /// Turn index.
    pub turn: usize,
    /// Step index within the turn.
    pub step: usize,
}

impl Cursor {
    /// The node the cursor addresses.
    #[must_use]
    pub fn node(self) -> NodeId {
        NodeId::Step {
            turn: self.turn,
            step: self.step,
        }
    }

    /// Move to the next step, crossing into the next turn at a turn's end.
    /// Saturates at the last step rather than wrapping — a wrap in a long
    /// transcript loses the reader's place silently.
    #[must_use]
    pub fn next(self, run: &Run) -> Self {
        let steps = run.turns.get(self.turn).map_or(0, |t| t.steps.len());
        if self.step + 1 < steps {
            return Self {
                step: self.step + 1,
                ..self
            };
        }
        for turn in self.turn + 1..run.turns.len() {
            if !run.turns[turn].steps.is_empty() {
                return Self { turn, step: 0 };
            }
        }
        self
    }

    /// Move to the previous step, crossing back into the previous turn.
    #[must_use]
    pub fn prev(self, run: &Run) -> Self {
        if self.step > 0 {
            return Self {
                step: self.step - 1,
                ..self
            };
        }
        for turn in (0..self.turn).rev() {
            let steps = run.turns[turn].steps.len();
            if steps > 0 {
                return Self {
                    turn,
                    step: steps - 1,
                };
            }
        }
        self
    }
}

/// Apply one command to a fold state and cursor.
///
/// Returns the invocation string when the command was [`Command::CopyInvocation`]
/// — the crate cannot reach a clipboard (invariant #2), so it hands the host the
/// exact text to put there and stays pure.
pub fn apply(
    run: &Run,
    state: &mut FoldState,
    cursor: &mut Cursor,
    command: Command,
) -> Option<String> {
    match command {
        Command::NextStep => *cursor = cursor.next(run),
        Command::PrevStep => *cursor = cursor.prev(run),
        Command::Expand => state.open(cursor.node()),
        Command::Collapse => state.close(cursor.node()),
        Command::CycleZoom => state.cycle_zoom(),
        Command::ExpandOutputs => state.expand_outputs_in_step(run, cursor.turn, cursor.step),
        Command::CopyInvocation => {
            return run
                .turns
                .get(cursor.turn)
                .and_then(|t| t.steps.get(cursor.step))
                .and_then(|s| s.call.as_ref())
                .map(|c| c.header_object.clone());
        }
    }
    None
}
