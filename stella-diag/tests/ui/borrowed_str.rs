// The subtler half: `&'static str` is loggable because it lives in the binary,
// so the obvious workaround is `.as_str()`. A borrow of a local `String` is not
// `'static`, and the lifetime is what refuses it.
use stella_diag::{diag, Dx};

fn main() {
    let dx = Dx::disabled();
    let answer = String::from("the model said something");
    diag!(&dx, info, "model.answer", text = answer.as_str());
}
