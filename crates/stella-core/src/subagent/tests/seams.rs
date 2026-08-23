use super::*;

// ---- seams: gate, steering, attribution -------------------------------

/// A steering source that records whether its queue was ever drained.
struct SpySteering {
    drained: AtomicUsize,
    stop: bool,
}

impl TurnSteering for SpySteering {
    fn drain_steering(&self) -> Vec<String> {
        self.drained.fetch_add(1, Ordering::SeqCst);
        vec!["a message the user wrote to the PARENT".to_string()]
    }
    fn soft_stop_requested(&self) -> bool {
        self.stop
    }
}

#[tokio::test]
async fn a_child_honors_the_soft_stop_but_never_eats_the_parents_steering() {
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![Ok(text_result("hi", 0.01))]);
    let tools = MixedTools::default();
    let steering = SpySteering {
        drained: AtomicUsize::new(0),
        stop: true,
    };
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep)
        .with_steering(&steering);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("stoppable", "work"),
            &mut budget,
            &tx,
        )
        .await;

    assert_eq!(
        steering.drained.load(Ordering::SeqCst),
        0,
        "drain_steering is destructive by contract — a child that called it \
         would silently swallow a message addressed to the parent"
    );
    assert!(
        matches!(&outcome, SubAgentOutcome::Incomplete { reason, .. } if reason.contains(crate::driver::SOFT_STOP_REASON)),
        "the latched soft stop must still stop the child, got {outcome:?}"
    );
    assert_eq!(
        child_provider.calls.load(Ordering::SeqCst),
        0,
        "a stop latched before the first step stops it there"
    );
}

#[test]
fn child_steering_forwards_the_stop_and_swallows_the_drain() {
    let parent = SpySteering {
        drained: AtomicUsize::new(0),
        stop: true,
    };
    let child = ChildSteering::new(&parent);
    assert!(child.drain_steering().is_empty());
    assert_eq!(parent.drained.load(Ordering::SeqCst), 0);
    assert!(child.soft_stop_requested());
}

/// A gate that counts how many times it was polled — proving the child polls
/// the parent's gate rather than running unpausable.
struct CountingGate(AtomicUsize);

#[async_trait]
impl TurnGate for CountingGate {
    async fn wait_if_paused(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn a_child_polls_the_parents_pause_gate() {
    // `assess` dropped the gate when it hand-rolled a verifier engine, so a
    // paused session kept spending inside the verifier. Inheritance here is
    // what closes that.
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("read_file", "c1", 0.01)),
        Ok(text_result("done", 0.01)),
    ]);
    let tools = MixedTools::default();
    let gate = CountingGate(AtomicUsize::new(0));
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep)
        .with_gate(&gate);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("pausable", "work"),
            &mut budget,
            &tx,
        )
        .await;

    assert_eq!(
        gate.0.load(Ordering::SeqCst),
        2,
        "once per child step — a child that ignored the gate would keep \
         spending through a pause"
    );
}

/// [`TurnControls`] is the owned form a session-scoped host holds, so a
/// sub-agent dispatcher can give a child the seams of the turn that asked for
/// it. This pins the two properties that make it safe to call blindly at
/// dispatch time.
#[tokio::test]
async fn owned_turn_controls_stop_a_child_without_clobbering_an_attached_gate() {
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![Ok(text_result("hi", 0.01))]);
    let tools = MixedTools::default();

    // A gate attached the per-turn way, and controls carrying only steering:
    // the driver's gate must survive, because a host that reads controls
    // holding one seam would otherwise silently unpause the child.
    let gate = CountingGate(AtomicUsize::new(0));
    let steering = Arc::new(SpySteering {
        drained: AtomicUsize::new(0),
        stop: true,
    });
    let controls = TurnControls::none().with_steering(steering.clone());
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep)
        .with_gate(&gate)
        .with_turn_controls(&controls);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("controlled", "work"),
            &mut budget,
            &tx,
        )
        .await;

    assert!(
        matches!(&outcome, SubAgentOutcome::Incomplete { reason, .. }
            if reason.contains(crate::driver::SOFT_STOP_REASON)),
        "the published stop must reach the child, got {outcome:?}"
    );
    assert_eq!(
        gate.0.load(Ordering::SeqCst),
        1,
        "the separately-attached gate must still be polled — controls fill \
         absent seams, they do not replace the set"
    );
    assert_eq!(
        steering.drained.load(Ordering::SeqCst),
        0,
        "still `ChildSteering` underneath: the stop crosses, the parent's \
         queued messages never do"
    );
}

/// Every driver used to publish exactly one seam — worker lanes a gate, the
/// deck's lead a steering tap — so the both-at-once case had no caller and no
/// test. The deck's lead lane is that caller now (#1219): it steers with `>`
/// and pauses with `p`, and hands the pair to its sub-agent dispatcher in one
/// [`TurnControls`]. A `with_gate` that displaced the steering it was given
/// would turn the lead's Esc into a no-op the moment pause was wired up —
/// silently, since both are `Option` and neither absence is an error.
#[test]
fn turn_controls_carrying_both_seams_give_a_child_both() {
    let provider = ScriptedProvider::new(vec![]);
    let tools = MixedTools::default();
    let both = TurnControls::none()
        .with_gate(Arc::new(CountingGate(AtomicUsize::new(0))))
        .with_steering(Arc::new(SpySteering {
            drained: AtomicUsize::new(0),
            stop: false,
        }));
    assert!(!both.is_empty());

    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &NoSleep)
        .with_turn_controls(&both);

    assert!(engine.gate.is_some(), "the pause must survive the steering");
    assert!(
        engine.steering.is_some(),
        "and the steering must survive the pause — the two seams are \
         independent, not a slot the last writer wins"
    );
}

#[test]
fn empty_turn_controls_leave_an_engine_exactly_as_it_was() {
    let provider = ScriptedProvider::new(vec![]);
    let tools = MixedTools::default();
    let gate = CountingGate(AtomicUsize::new(0));
    let nothing = TurnControls::none();
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &NoSleep)
        .with_gate(&gate)
        .with_turn_controls(&nothing);

    assert!(
        engine.gate.is_some(),
        "a driver that publishes nothing must not cost a turn its own gate"
    );
    assert!(engine.steering.is_none());
    assert!(TurnControls::none().is_empty());
}

#[test]
fn attribution_leaves_its_own_scope_including_on_an_unwind() {
    let bus = HookBus::new("session-1");
    let parent = bus.push_agent("parent".into());

    {
        let _child = AgentAttribution::enter(Some(&bus), "child");
        // Nested one deeper, then unwound by panic — the guard must still run
        // on the way out, and must leave "child" attributed rather than
        // clearing to None.
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _grandchild = AgentAttribution::enter(Some(&bus), "grandchild");
            panic!("a tool blew up");
        }));
        assert!(unwound.is_err());
        assert_eq!(
            bus.current_agent(),
            Some("child".to_string()),
            "the grandchild's guard ran during the unwind and left the child attributed"
        );
    }

    assert_eq!(
        bus.current_agent(),
        Some("parent".to_string()),
        "the child's exit leaves its parent attributed"
    );
    bus.drop_agent(&parent);
    assert_eq!(bus.current_agent(), None);
}

#[test]
fn attribution_with_no_bus_is_a_no_op() {
    let guard = AgentAttribution::enter(None, "orphan");
    drop(guard);
}

// ---- receipts ---------------------------------------------------------

#[tokio::test]
async fn the_child_claims_its_own_receipt_turn_slot() {
    // Receipts key on (execution_id, turn_instance, step, call_seq) and every
    // turn restarts step at 0, so a child sharing the parent's slot would
    // overwrite the parent's manifests in the store.
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![Ok(text_result("done", 0.01))]);
    let tools = MixedTools::default();
    let config = EngineConfig {
        turn_instance: 4,
        lifecycle_enabled: true,
        ..EngineConfig::default()
    };
    let parent = Engine::with_sleeper(&parent_provider, &tools, config, &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let spec = SubAgentSpec {
        turn_instance: 5,
        ..SubAgentSpec::read_only("receipted", "work")
    };
    parent
        .run_sub_agent(SubAgentHost::new(&child_provider), &spec, &mut budget, &tx)
        .await;

    let slots: Vec<u32> = drain(&mut rx)
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::StepManifest { turn_instance, .. } => Some(turn_instance),
            _ => None,
        })
        .collect();
    assert!(
        !slots.is_empty() && slots.iter().all(|slot| *slot == 5),
        "the child's manifests must land in ITS slot, got {slots:?}"
    );
}
