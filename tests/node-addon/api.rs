enum Mode { Quiet = 3, Loud = 9 }
enum Detail { Text { text: Text, stamp: i64 } }
enum Event { None, Data { mode: Enum<Mode>, bytes: Buffer, detail: Detail } }
enum Payload { Device(Device), Nested(Nested) }
struct Nested { gain: f64 }
struct Config {
    title: Text,
    number: i64,
    mode: Enum<Mode>,
    optional: Option<Text>,
    optional_mode: Option<Enum<Mode>>,
    choices: Vec<Enum<Mode>>,
    settings: Map<Text, f64>,
    nested: Nested,
    children: Vec<Nested>,
    bytes: Option<Buffer>,
    payload: Option<Payload>,
}
trait Other {
    #[native(other_open)] fn open() -> Box<Other>;
}
trait Device {
    #[native(device_open)] fn open(config: &Config) -> Box<Device>;
    #[native(device_text)] fn text(this: &Device) -> Text;
    #[native(device_event)] fn event(this: &Device) -> Event;
    #[native(device_echo)] fn echo(this: &Device, value: i64) -> i64;
    #[native(device_mode)] fn mode(this: &Device, mode: Option<Enum<Mode>>) -> i32;
    #[native(device_bytes)] fn bytes(this: &Device) -> Buffer;
    #[native(device_write)] fn write(this: &Device, target: BufferMut);
    #[native(device_async)] fn later(this: &Device, fail: bool) -> Future<Device>;
    #[native(device_done)] fn done(this: &Device) -> Future<()>;
    #[native(device_fail)] fn fail(this: &Device);
    #[native(device_panic)] fn panic(this: &Device);
}
