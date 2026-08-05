//! Print the launch mark's assemble timeline as plain text frames — a dev
//! tool for eyeballing the splash at any size without driving a pty.
//!
//! ```sh
//! cargo run -p stella-tui --example splash_frames [WIDTH HEIGHT]
//! ```

use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn main() {
    let mut args = std::env::args().skip(1);
    let w: u16 = args.next().and_then(|a| a.parse().ok()).unwrap_or(80);
    let h: u16 = args.next().and_then(|a| a.parse().ok()).unwrap_or(24);

    for (label, ms) in [
        ("trails slide in", 130_u64),
        ("star pops", 330),
        ("wordmark types on", 700),
        ("assembled · cursor blinking", 1_100),
    ] {
        let state = stella_tui::splash::state_backdated_for_preview(ms);
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                stella_tui::splash::render(&state, Some("glm-4.6"), area, f.buffer_mut());
            })
            .unwrap();
        println!(
            "── t={ms}ms · {label} {}",
            "─".repeat(40_usize.saturating_sub(label.len()))
        );
        let buf = terminal.backend().buffer();
        let area = *buf.area();
        for y in 0..area.height {
            let row: String = (0..area.width)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect();
            println!("{}", row.trim_end());
        }
        println!();
    }
}
