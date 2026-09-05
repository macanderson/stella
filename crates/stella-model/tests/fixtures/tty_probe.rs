//! Fixture binary for `tests/credential_prompt_dev_tty.rs`.
//!
//! Prints `TerminalPrompt.can_prompt(true)`, then exits. It runs as its
//! own process so the test harness can give it a real controlling
//! terminal, one `cargo test`'s own process does not have.

use stella_model::credential::{CredentialPrompt, TerminalPrompt};

fn main() {
    println!("{}", TerminalPrompt.can_prompt(true));
}
