// The line from docs/design/diagnostics/diagnostics.md §5.2, verbatim:
//
//     diag!(warn, "tools.write.denied", path = user_path);   // ← does not compile
//
// `tracing` would log this happily with `%user_path`. Here it is a type error.
use stella_diag::{diag, Dx};

fn main() {
    let dx = Dx::disabled();
    let user_path = String::from("/home/ada/.ssh/id_ed25519");
    diag!(&dx, warn, "tools.write.denied", path = user_path);
}
