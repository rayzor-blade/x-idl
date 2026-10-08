# x-idl

Generate bindings for **Ash/HashLink, Rayzor, Caribou, and Node/TypeScript**
from a shared API declaration. x-idl powers the runtime adapters in
[xgpu](https://github.com/rayzor-blade/xgpu) and
[xwindow](https://github.com/rayzor-blade/xwindow).

Declare resource methods, descriptors and events once. Import types and
constants from WebIDL where needed. Each target generates the bindings for
its runtime; the native backend implements the operations.

## Supported targets

| Target | Generated output | Guide |
| --- | --- | --- |
| Ash / HashLink | Haxe externs, native primitive resolvers, record wrappers and Ash futures | [Ash / HashLink](docs/runtimes.md#ash--hashlink) |
| Rayzor | Haxe externs, C exports, method descriptors and runtime symbol registration | [Rayzor](docs/runtimes.md#rayzor) |
| Caribou | Typed resource wrappers, record/enum schemas and a plugin export table for its frontends | [Caribou](docs/runtimes.md#caribou) |
| Node / TypeScript | TypeScript bindings and a Node-API entry point with typed conversions | [Node / TypeScript](docs/node.md) |

For browser adapters, x-idl also generates a
[Rust/JavaScript wire protocol](docs/browser.md) from WebIDL.

## One declaration, multiple runtimes

An API declaration uses Rust syntax:

```rust
struct Options {
    label: Text,
    enabled: bool,
}

trait Device {
    #[native(device_open)]
    fn open(options: &Options) -> Box<Device>;

    #[native(device_label)]
    fn label(this: &Device) -> Text;
}
```

`Options` becomes a descriptor, `Device` a resource, and `#[native(...)]`
selects the backend function. The generator reads this declaration; it is
not compiled as an ordinary Rust trait implementation. WebIDL imports can
supply dictionary fields, enum values, constants and union alternatives.

Add x-idl to the adapter's build dependencies:

```toml
[build-dependencies]
x_idl = { git = "https://github.com/rayzor-blade/x-idl" }
```

Use the matching generator in `build.rs`. For example, an Ash adapter can
produce its native bindings and Haxe surface together:

```rust
let library = x_idl::Library("devices");
let api = std::path::PathBuf::from("api/devices.rs");
let native = library.generate_hashlink("devices", api.clone(), "")?;
let haxe = library.haxe("devices", api, "", x_idl::haxe::Runtime::HashLink)?;
```

The empty string is the WebIDL source; replace it with its contents when
using imports. Pass declaration **source text** as a string, or a **file
path** as `PathBuf`.

See [runtime integration](docs/runtimes.md) for writing the generated files,
including them in an adapter, and generating Rayzor and Caribou bindings.
Runtime adapters supply memory ownership, futures and error handling; the
backend remains shared.

## Verification

Run the generator tests, including Ash/HashLink, Rayzor, Caribou and
TypeScript generation:

```sh
cargo test --locked --lib
```

The Node target also has a compiled addon fixture (Node.js 24+):

```sh
node tests/node-addon/run.mjs
```

CI runs the generator tests on Linux and the Node fixture on Linux, macOS
and Windows.
The browser wire has separate [integration checks](docs/browser.md#verification).

MIT. See [LICENSE](LICENSE).
