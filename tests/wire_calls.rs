//! Calls whose results the wire carries in ways other than a value, both
//! halves together: the generated Rust, built for wasm, encodes the calls
//! and reads the replies, while the generated JavaScript, under Node, runs
//! them against stand-in objects. A sequence of interfaces comes back
//! under consecutive handles; an operation answering with bytes the page
//! owns also writes the program's bytes into them. Needs `rustc` with the
//! `wasm32-unknown-unknown` target, and `node`.

use std::path::Path;
use std::process::Command;

const IDL: &str = r#"
interface Item {
  readonly attribute DOMString name;
};
interface Source {
  sequence<Item> items();
  ArrayBuffer range(unsigned long offset, unsigned long size);
};
"#;

/// A program that encodes one batch, then reads each reply as text.
const GUEST: &str = r#"
#[allow(dead_code, non_camel_case_types, clippy::all)]
mod wire {
    include!("wire.rs");
}
use core::sync::atomic::{AtomicI32, Ordering::SeqCst};
use wire::*;

#[repr(C)]
struct Reply {
    state: AtomicI32,
    len: u32,
    address: u32,
    cap: u32,
}

static mut BUFFERS: [[u8; 64]; 4] = [[0; 64]; 4];
static mut REPLIES: [Reply; 4] = [const { Reply { state: AtomicI32::new(0), len: 0, address: 0, cap: 0 } }; 4];
static WRITTEN: [u8; 4] = [7, 8, 9, 10];
static mut BATCH: Vec<u8> = Vec::new();
static mut OUT: String = String::new();

fn reply(i: usize) -> &'static mut Reply {
    let r = unsafe { &mut (*core::ptr::addr_of_mut!(REPLIES))[i] };
    r.address = unsafe { (&*core::ptr::addr_of!(BUFFERS))[i].as_ptr() as usize as u32 };
    r.cap = 64;
    r
}

fn at(r: &Reply) -> u32 {
    r as *const Reply as usize as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn batch() -> u32 {
    let mut e = Encoder::new();
    let source = Handle(1);
    // Items kept from handle 100; the second one's name.
    e.source_items(source, Handle(100), at(reply(0)));
    e.item_get_name(Handle(101), at(reply(1)));
    // Four bytes written at 2, then all eight read back.
    let bytes = Bytes { address: WRITTEN.as_ptr() as usize as u32, len: 4 };
    e.source_range_write(source, at(reply(2)), &2, &4, &bytes);
    e.source_range(source, at(reply(3)), &0, &8);
    unsafe {
        *core::ptr::addr_of_mut!(BATCH) = e.bytes;
        (&*core::ptr::addr_of!(BATCH)).as_ptr() as usize as u32
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn batch_len() -> u32 {
    unsafe { (&*core::ptr::addr_of!(BATCH)).len() as u32 }
}

#[unsafe(no_mangle)]
pub extern "C" fn read() {
    let out = unsafe { &mut *core::ptr::addr_of_mut!(OUT) };
    let body = |i: usize| {
        let r = reply(i);
        let len = r.len as usize;
        (r.state.load(SeqCst), unsafe { &(&*core::ptr::addr_of!(BUFFERS))[i][..len] })
    };
    let (state, bytes) = body(0);
    out.push_str(&format!("items {state} {:?};", u32::decode(&mut Decoder::new(bytes))));
    let (state, bytes) = body(1);
    out.push_str(&format!("name {state} {:?};", String::decode(&mut Decoder::new(bytes))));
    let (state, bytes) = body(2);
    out.push_str(&format!("write {state} {};", bytes.len()));
    let (state, bytes) = body(3);
    out.push_str(&format!("range {state} {bytes:?}"));
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
import { Wire, execute } from "./wire.mjs";

const { instance } = await WebAssembly.instantiate(readFileSync("guest.wasm"));
const guest = instance.exports;
// A source whose range is a view of one mapped buffer, as a page's is.
const mapped = new ArrayBuffer(8);
const source = {
  items: () => [{ name: "first" }, { name: "second" }],
  range: (offset, size) => new Uint8Array(mapped, offset, size),
};
const wire = new Wire(guest.memory, new Map([[1, source]]));
execute(wire, guest.batch(), guest.batch_len());
guest.read();
console.log(new TextDecoder().decode(new Uint8Array(guest.memory.buffer, guest.out_ptr(), guest.out_len())));
"#;

#[test]
fn a_sequence_of_objects_comes_back_under_handles_and_bytes_go_into_a_range() {
    let wire = x_idl::wire::wire(IDL).unwrap();
    let dir = std::env::temp_dir().join(format!("xidl-calls-{}", std::process::id()));
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

    assert_eq!(
        out.trim(),
        r#"items 1 Some(2);name 1 Some("second");write 1 0;range 1 [0, 0, 7, 8, 9, 10, 0, 0]"#
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
