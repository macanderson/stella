//! Dev harness: serve the observatory over any workspace without building
//! the full CLI. Useful when iterating on `assets/index.html` (the page is
//! embedded at compile time, so rebuild between edits).
//!
//! ```sh
//! cargo run -p stella-observatory --example serve -- /path/to/workspace 7787
//! ```

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let root = args
        .next()
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .expect("workspace root");
    let port: u16 = args.next().and_then(|p| p.parse().ok()).unwrap_or(7787);
    stella_observatory::serve(root, port, |addr| {
        println!("observatory: http://{addr}");
    })
    .await
    .expect("serve");
}
