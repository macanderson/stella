// §5.5: `note!` accepts only a string literal, so the justification lives in
// the source, is greppable, and shows up in the diff that introduced it. A
// computed justification would be a justification nobody reviews.
use stella_diag::{note, Redacted};

fn main() {
    let reason = String::from("it seemed fine");
    let _ = Redacted::reviewed("main", note!(reason));
}
