// §5.4: a path is the most-wanted log field in a coding agent and the most
// dangerous. `PathClass` is the way through; the path itself is not.
use std::path::PathBuf;
use stella_diag::{diag, Dx};

fn main() {
    let dx = Dx::disabled();
    let target = PathBuf::from("/home/ada/clients/acme/notes.md");
    diag!(&dx, warn, "tools.write.denied", path = target);
}
