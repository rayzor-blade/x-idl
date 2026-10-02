//! The queue an agent posts records to, both halves together: the
//! generated Rust, built for wasm, takes records from its ring, while the
//! generated JavaScript, under Node, posts them into the same memory. The
//! ring is small enough that posting wraps it and fills it. Needs `rustc`
//! with the `wasm32-unknown-unknown` target, and `node`.

use std::path::Path;
use std::process::Command;

const IDL: &str = r#"
enum Kind { "a", "b" };
dictionary Record {
  required Kind kind;
  long n;
  DOMString text;
  double x;
};
"#;

/// A program with a 64-byte ring, which writes what it takes as text.
const GUEST: &str = r#"
#[allow(dead_code, non_camel_case_types, clippy::all)]
mod wire {
    include!("wire.rs");
}
use wire::*;

static QUEUE: Queue = Queue::new();
static mut RING: [u8; 64] = [0; 64];
static mut OUT: String = String::new();

#[unsafe(no_mangle)]
pub extern "C" fn queue() -> u32 {
    QUEUE.attach(unsafe { &mut *core::ptr::addr_of_mut!(RING) });
    &QUEUE as *const Queue as usize as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn drain() {
    let ring = unsafe { &*core::ptr::addr_of!(RING) };
    let out = unsafe { &mut *core::ptr::addr_of_mut!(OUT) };
    while let Some(record) = QUEUE.take::<Record>(ring) {
        out.push_str(&format!("{:?} {:?} {:?} {:?}|", record.kind, record.n, record.text, record.x));
    }
    out.push_str(&format!("dropped {};", QUEUE.dropped.load(core::sync::atomic::Ordering::SeqCst)));
}

#[unsafe(no_mangle)]
pub extern "C" fn out_ptr() -> u32 {
    unsafe { (&*core::ptr::addr_of!(OUT)).as_ptr() as usize as u32 }
}

#[unsafe(no_mangle)]
pub extern "C" fn out_len() -> u32 {
    unsafe { (&*core::ptr::addr_of!(OUT)).len() as u32 }
}
"#;

const HARNESS: &str = r#"
import { readFileSync } from "node:fs";
import { Queue, post_Record } from "./wire.mjs";

const { instance } = await WebAssembly.instantiate(readFileSync("guest.wasm"));
const guest = instance.exports;
const queue = new Queue(guest.memory, guest.queue());
const posted = [];
posted.push(post_Record(queue, { kind: "a", n: 1 }));
posted.push(post_Record(queue, { kind: "b", text: "hello", x: 1.5 }));
guest.drain();
for (let i = 0; i < 4; i++) posted.push(post_Record(queue, { kind: "a", n: 10 + i }));
guest.drain();
for (let i = 0; i < 6; i++) posted.push(post_Record(queue, { kind: "b", n: 20 + i, text: "xy" }));
guest.drain();
const out = new TextDecoder().decode(new Uint8Array(guest.memory.buffer, guest.out_ptr(), guest.out_len()));
console.log(JSON.stringify({ posted, out }));
"#;

#[test]
fn javascript_posts_and_rust_takes_records_around_a_ring() {
    let wire = x_idl::wire::wire_posting(IDL, &["Record"]).unwrap();
    let dir = std::env::temp_dir().join(format!("xidl-queue-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("wire.rs"), &wire.rust).unwrap();
    std::fs::write(dir.join("wire.mjs"), &wire.js).unwrap();
    std::fs::write(dir.join("guest.rs"), GUEST).unwrap();
    std::fs::write(dir.join("harness.mjs"), HARNESS).unwrap();

    run(
        &dir,
        Command::new("rustc").args([
            "--edition",
            "2024",
            "--target",
            "wasm32-unknown-unknown",
            "--crate-type",
            "cdylib",
            "-O",
            "guest.rs",
            "-o",
            "guest.wasm",
        ]),
    );
    let out = run(&dir, Command::new("node").arg("harness.mjs"));
    std::fs::remove_dir_all(&dir).ok();

    // A record is its length and its encoding padded to four: 16 bytes for
    // `{a, n}`, 28 with "hello" and `x`, 24 for `{b, n, "xy"}`. Four bytes
    // stay free, and a record that would cross the end leaves a marker and
    // starts again at zero, so the ring's 64 bytes hold three or two.
    assert_eq!(
        out.trim(),
        concat!(
            r#"{"posted":[true,true,true,true,true,false,true,true,false,false,false,false],"out":""#,
            r#"A Some(1) None None|B None Some(\"hello\") Some(1.5)|dropped 0;"#,
            r#"A Some(10) None None|A Some(11) None None|A Some(12) None None|dropped 1;"#,
            r#"B Some(20) Some(\"xy\") None|B Some(21) Some(\"xy\") None|dropped 5;"}"#
        )
    );
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
