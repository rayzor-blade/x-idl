//! An optional argument the caller left out is left off the call, not passed
//! as `undefined`: an overloaded method, as WebGPU's `setBindGroup` is, may
//! refuse the explicit `undefined`, which a page showed after it had run a
//! while. The generated JavaScript runs under Node against a stand-in that
//! counts the arguments it is called with. Needs `node`.

use std::path::Path;
use std::process::Command;

const IDL: &str = r#"
interface Target {
  undefined mark(unsigned long first, optional unsigned long second);
  undefined span(unsigned long first, optional unsigned long second, optional unsigned long third);
};
"#;

const HARNESS: &str = r#"
import { Wire, execute } from "./wire.mjs";

const memory = new WebAssembly.Memory({ initial: 1 });
const counts = [];
const target = {
  mark: (...args) => counts.push("mark" + args.length),
  span: (...args) => counts.push("span" + args.length),
};
const wire = new Wire(memory, new Map([[1, target]]));
const u32 = (v) => [v & 255, (v >> 8) & 255, (v >> 16) & 255, v >>> 24];
function send(op, payload) {
  const bytes = [...u32(op), ...u32(payload.length), ...payload];
  new Uint8Array(memory.buffer).set(bytes, 0);
  execute(wire, 0, bytes.length);
}
const self = u32(1), first = u32(7), second = u32(8), third = u32(9);
// mark(first), then mark(first, second).
send(0, [...self, ...first, 0]);
send(0, [...self, ...first, 1, ...second]);
// span(first), span(first, second), span(first, second, third), and the third without the second left as it is.
send(1, [...self, ...first, 0, 0]);
send(1, [...self, ...first, 1, ...second, 0]);
send(1, [...self, ...first, 1, ...second, 1, ...third]);
send(1, [...self, ...first, 0, 1, ...third]);
console.log(counts.join(","));
"#;

#[test]
fn an_optional_argument_left_out_is_left_off_the_call() {
    let wire = x_idl::wire::wire(IDL).unwrap();
    let dir = std::env::temp_dir().join(format!("xidl-optional-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("wire.mjs"), &wire.js).unwrap();
    std::fs::write(dir.join("harness.mjs"), HARNESS).unwrap();
    let out = run(&dir, Command::new("node").arg("harness.mjs"));
    std::fs::remove_dir_all(&dir).ok();

    // The last, `span` with only the third given, is called with all three: the second is not trailing.
    assert_eq!(out.trim(), "mark1,mark2,span1,span2,span3,span3");
}

fn run(dir: &Path, command: &mut Command) -> String {
    let out = command.current_dir(dir).output().expect("the tool runs");
    assert!(
        out.status.success(),
        "{:?}: {}{}",
        command,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}
