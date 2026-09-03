//! Check that a panicking panel is contained in a build made with the release
//! panic strategy.
//!
//! `cargo test` always unwinds, so no test can see the strategy the shipped
//! binary is built with. This example can: run it with `--release` and it
//! enters the real panel boundary. If the profile unwinds, the boundary paints
//! its error card and this exits 0. If the profile aborts, the panic kills the
//! process here and the exit code says so.
//!
//! ```sh
//! cargo run -p stella-tui --example panic_guard_probe --release
//! ```

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn main() {
    // A quiet hook so the one line this prints is the whole story. Under an
    // aborting profile the hook still runs and the process still dies.
    std::panic::set_hook(Box::new(|_| {}));

    let area = Rect::new(0, 0, 60, 10);
    let mut frame = Buffer::empty(area);
    stella_tui::panel_guard::guarded_band(&mut frame, area, "probe", |_| {
        panic!("panic_guard_probe: on purpose")
    });

    let _ = std::panic::take_hook();

    let painted: String = (0..area.height)
        .flat_map(|y| (0..area.width).map(move |x| (x, y)))
        .map(|p| frame[p].symbol().to_string())
        .collect();

    if !painted.contains("panicked") {
        eprintln!("panic_guard_probe: FAIL — the boundary caught the panic but painted no card");
        std::process::exit(1);
    }
    println!("panic_guard_probe: OK — a panel panic was contained under this profile");
}
