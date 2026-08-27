// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The panel protocol, end to end, against the plugin that ships in this
//! repository.
//!
//! Every other panel test uses a fixture. This one starts
//! `plugins/stella-hello/panel.py` as a real process, hands it a real lease,
//! decodes the frame it actually writes, and blits that frame through the real
//! host renderer into a terminal buffer — so a green run here is evidence the
//! whole chain works, not evidence that two halves agree about a fixture.
//!
//! Run it and read the panel:
//!
//! ```text
//! cargo test -p stella-cli --test hello_panel_demo -- --nocapture
//! ```

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use stella_plugin::{PanelLease, PanelRect, PanelSurface, PluginManifest, Runtime};

/// The manifest and the process the demo drives, resolved from the repository
/// rather than written inline — a demo that invented its own manifest would
/// prove nothing about the one that ships.
fn hello_plugin() -> (PluginManifest, Runtime) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/stella-hello")
        .canonicalize()
        .expect("the hello plugin ships in this repository");
    let manifest = PluginManifest::from_toml_str(
        &std::fs::read_to_string(dir.join("plugin.toml")).expect("its manifest is readable"),
    )
    .expect("its manifest parses");
    let panel = manifest.panel.clone().expect("it declares a panel");
    let mut process = panel.process.clone().expect("it declares a process");
    // `${plugin_dir}` interpolation is the host's job (`[panel.process]`'s own
    // doc comment), so the demo does it the way a host would.
    process.argv = process
        .argv
        .iter()
        .map(|arg| arg.replace("${plugin_dir}", &dir.display().to_string()))
        .collect();
    (manifest, process)
}

fn python_is_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The demo. A real process draws a real frame into a real buffer, and the
/// rendered panel is printed for a human to look at.
#[tokio::test]
async fn the_hello_plugin_draws_its_panel_in_the_deck() {
    if !python_is_available() {
        eprintln!("skipped: python3 is not on PATH, and the hello plugin is written in it");
        return;
    }
    let (manifest, process) = hello_plugin();

    // A deck-sized frame, with the panel occupying the middle of it.
    let mut terminal = Terminal::new(TestBackend::new(64, 12)).expect("a test terminal");
    let area = Rect::new(2, 1, 60, 10);

    // The host draws its own chrome first and leases what is left inside it.
    let mut lease_rect = Rect::default();
    terminal
        .draw(|f| {
            lease_rect = stella_tui::plugin_panel::chrome(area, &manifest.name, f.buffer_mut());
        })
        .expect("chrome draws");

    // Ask the plugin for one frame against exactly that rectangle.
    let lease = PanelLease::new(
        &manifest.name,
        PanelSurface::Command,
        7,
        PanelRect {
            cols: lease_rect.width,
            rows: lease_rect.height,
        },
        33,
    );
    let tick = stella_runtime::panel_host::ask(&process, lease.clone())
        .await
        .expect("the hello plugin answers a frame");
    let frame = tick.frame.expect("it drew one");

    // It answered the lease it was given, and its frame fits.
    assert_eq!(frame.tick, lease.tick, "the frame echoes the host's tick");
    // `admits` rather than `fits`: geometry alone would draw a frame answering
    // a different surface or a tick the host has moved past.
    lease.admits(&frame).expect("the frame answers this lease");

    // Blit it through the real host renderer.
    terminal
        .draw(|f| {
            let _ = stella_tui::plugin_panel::chrome(area, &manifest.name, f.buffer_mut());
            stella_tui::plugin_panel::blit(&frame, lease_rect, f.buffer_mut());
        })
        .expect("the frame blits");

    let painted = render_to_string(terminal.backend());
    println!("\n{painted}\n");

    // The plugin's own words are on screen…
    assert!(painted.contains("hello from a plugin"), "{painted}");
    assert!(
        painted.contains(&format!("{}×{}", lease_rect.width, lease_rect.height)),
        "the panel states the rectangle it was actually leased: {painted}"
    );
    // …inside the host's chrome, which the plugin did not draw and cannot suppress.
    assert!(painted.contains("◳ panel · hello"), "{painted}");
    assert!(painted.contains('╭') && painted.contains('╯'), "{painted}");
}

fn render_to_string(backend: &TestBackend) -> String {
    let buffer = backend.buffer();
    let area = *buffer.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
