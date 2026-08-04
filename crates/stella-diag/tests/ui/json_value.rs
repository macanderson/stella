// A field value is not `serde_json::Value`. It is the one type that would make
// every other rule in §5.2 decorative, since anything serializes into it.
use stella_diag::{diag, Dx};

fn main() {
    let dx = Dx::disabled();
    let payload = serde_json::json!({ "prompt": "the user's actual prompt" });
    diag!(&dx, debug, "model.request", body = payload);
}
