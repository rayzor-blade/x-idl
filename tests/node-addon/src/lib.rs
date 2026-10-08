#![allow(
    non_snake_case,
    dead_code,
    unused_mut,
    improper_ctypes_definitions,
    clippy::all,
    unsafe_op_in_unsafe_fn
)]
mod runtime {
    pub use xidl_node::*;
}
use runtime::{Buffer, BufferMut, Enum, ErrorKind, Future, NativeEnum, Rooted, Text, host};
include!(concat!(env!("OUT_DIR"), "/binding.rs"));

mod backend {
    use super::*;
    pub unsafe fn other_open() -> i32 {
        42
    }
    pub unsafe fn device_open(c: &Config) -> i32 {
        assert_eq!(c.title.get().as_str(), "hello");
        assert_eq!(c.number, 9_007_199_254_740_993);
        assert_eq!(c.mode, 9);
        assert_eq!(c.optional.as_ref().unwrap().get().as_str(), "extra");
        assert_eq!(c.optional_mode, Some(3));
        assert_eq!(c.choices, [3, 9]);
        assert_eq!(c.settings[0].0.get().as_str(), "gain");
        assert_eq!(c.settings[0].1, 2.0);
        assert_eq!(c.nested.gain, 0.5);
        assert_eq!(c.children[0].gain, 0.25);
        assert_eq!(c.bytes.as_ref().unwrap().get().as_slice(), [4, 5]);
        assert!(matches!(
            c.payload,
            Some(Payload::Nested(Nested { gain: 0.75 }))
        ));
        7
    }
    pub unsafe fn device_text(_: i32) -> Text {
        Text::new("native ✓")
    }
    pub unsafe fn device_event(_: i32) -> Event {
        Event::Data {
            mode: Mode::Quiet,
            bytes: VariantBytes(vec![1, 2, 3]),
            detail: Detail::Text {
                text: "event".into(),
                stamp: 9_007_199_254_740_993,
            },
        }
    }
    pub unsafe fn device_echo(_: i32, value: i64) -> i64 {
        value
    }
    pub unsafe fn device_mode(_: i32, mode: Option<i32>) -> i32 {
        mode.unwrap_or(-1)
    }
    pub unsafe fn device_bytes(_: i32) -> Buffer {
        Buffer::new(&[7, 8, 9])
    }
    pub unsafe fn device_write(_: i32, target: BufferMut) {
        target.as_slice_mut().copy_from_slice(&[8, 7, 6]);
    }
    pub unsafe fn device_async(handle: i32, fail: bool) -> Future<Device> {
        let future = Future::new();
        let rooted = Rooted::new(future);
        std::thread::spawn(move || {
            if fail {
                rooted.get().reject(Text::new("async rejected").value());
            } else {
                rooted.get().resolve_boxed(Box::new(Device { handle }));
            }
        });
        future
    }
    pub unsafe fn device_done(_: i32) -> Future<()> {
        let future = Future::new();
        future.resolve(runtime::Value::null());
        future
    }
    pub unsafe fn device_fail(_: i32) {
        host::raise(ErrorKind::Type, "backend validation");
    }
    pub unsafe fn device_panic(_: i32) {
        panic!("backend panic");
    }
}
