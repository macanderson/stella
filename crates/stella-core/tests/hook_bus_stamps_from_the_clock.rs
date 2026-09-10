//! Witness: a hook event's `timestamp` is the injected clock's reading, not
//! the process's wall clock.
//!
//! A bus that stamps from `SystemTime::now()` cannot pass this: no test can
//! say what such a stamp will be. Two readings decades apart show the stamp
//! follows the clock the bus was handed.

use std::sync::{Arc, Mutex};

use stella_core::bus::{HookBus, names};
use stella_core::ports::FixedClock;

fn stamp_at(unix_ms: u64) -> String {
    let bus = HookBus::new("witness", FixedClock(unix_ms));
    let seen = Arc::new(Mutex::new(None));
    let sink = seen.clone();
    let _observer = bus.on("*", move |event| {
        *sink.lock().expect("mutex poisoned") = Some(event.timestamp.clone());
        Ok(())
    });
    bus.emit_named(names::FILE_READ, serde_json::json!({"path": "a"}));
    seen.lock()
        .expect("mutex poisoned")
        .clone()
        .expect("the observer saw the event")
}

#[test]
fn the_stamp_is_the_clocks_reading() {
    assert_eq!(stamp_at(0), "1970-01-01T00:00:00.000Z");
    assert_eq!(stamp_at(1_700_000_000_123), "2023-11-14T22:13:20.123Z");
}
