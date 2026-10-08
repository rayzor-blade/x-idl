# Runtime integration

Ash/HashLink, Rayzor and Caribou share an API declaration and backend operations.
The generators adapt resources, descriptors, enums, events and asynchronous
results to each runtime's conventions. [Node/TypeScript](node.md) consumes the
same declaration through its own adapter.

## Shared inputs

In an adapter's `build.rs`, load the declaration by path and the WebIDL as text:

```rust
let api = std::path::PathBuf::from("api/devices.rs");
let idl = std::fs::read_to_string("api/devices.idl")?;
let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
let library = x_idl::Library("devices");
```

Use `""` for `idl` when the declaration has no WebIDL imports. A declaration
can also be supplied as source text, including `include_str!(...)`.
`Library` names the native library and prefixes its ABI symbols; use a distinct
name for each binding loaded into a program.

The examples below use these inputs. Native source goes into `OUT_DIR` and
is included in the adapter beside its backend and runtime types:

```rust
mod backend;
include!(concat!(env!("OUT_DIR"), "/devices.rs"));
```

The adapter supplies the target's carrier types and host operations, such as
`Text`, `Buffer`, roots, futures and errors. Backend functions named by
`#[native(...)]` perform the actual work. Generating a binding does not
implement its backend.

## Ash / HashLink

Generate native primitive resolvers and the matching Haxe files:

```rust
let native = library.generate_hashlink("devices", api.clone(), &idl)?;
std::fs::write(out.join("devices.rs"), native)?;

let files = library.haxe(
    "devices",
    api,
    &idl,
    x_idl::haxe::Runtime::HashLink,
)?;
let haxe_root = std::path::PathBuf::from("generated/haxe");
for file in files {
    let path = haxe_root.join(file.path);
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(path, file.source)?;
}
```

Add `generated/haxe` to the application's Haxe class path. The generated
`@:hlNative` annotations resolve against `devices.hdll` on native targets,
or the runtime adapter's `devices.wasm` in an Ash browser build.

Resource references cross the native boundary as integer handles. Descriptor
records use GC-finalized native abstracts; asynchronous methods use
`ash.Future<T>` and the Ash Future ABI. The HashLink wrappers normalize native
values to Haxe's ABI, including `f32` values crossing as doubles.

The adapter owns roots and error delivery. Native errors must reach the
runtime after Rust temporaries have been released, because a HashLink throw
can skip Rust destructors.

For Haxe generation alone, the CLI's `ash` and `hashlink` targets are aliases:

```sh
cargo run --bin xidl-haxe -- \
  --idl api/devices.idl --namespace devices \
  --declaration api/devices.rs ash generated/haxe
```

This CLI uses the default native library name `xidl`. Use `Library(...).haxe`
when the binding has its own library name, as in the build example above.

## Rayzor

Generate the native model and registration with the exact Haxe package name:

```rust
let native = library.generate_rayzor_in(
    "devices",
    "rayzor.devices",
    api.clone(),
    &idl,
    &[],
)?;
std::fs::write(out.join("devices.rs"), native)?;
let files = library.haxe(
    "rayzor.devices",
    api,
    &idl,
    x_idl::haxe::Runtime::Rayzor,
)?;
```

Write `files` under the Haxe class path using the same loop as above.
The native output includes C exports, `XIDL_METHODS` and
`xidl_runtime_symbols()` for the adapter's package registration. Method-table
class names must match the Haxe extern package, including its namespace.
The generated Haxe surface uses `rayzor.concurrent.Future<T>` for asynchronous
results.

Rayzor's adapter supplies its own text, buffer, root, future and error
carriers. The final `adapter_resources` argument lists resources whose wrappers
it provides itself, allowing a wrapper to retain runtime-specific metadata.
Pass `&[]` when the generated wrappers are sufficient.

## Caribou

Generate resource wrappers, schemas and the plugin table:

```rust
let native = x_idl::generate_caribou("devices", api, &idl)?;
std::fs::write(out.join("devices.rs"), native)?;
```

The output uses `caribou_abi` types and emits a `caribou_abi::plugin!` table.
Caribou's frontends discover the API through that plugin metadata. Resources
are typed native objects, records retain rooted values where necessary, and
enums and tagged events have generated schemas. Backend resource operations
still use the shared native handles.

`x_idl::generate(...)` is the compatibility spelling of
`generate_caribou(...)` used by existing adapters.

## Existing integrations

- [xgpu-bindgen](https://github.com/rayzor-blade/xgpu/tree/main/crates/xgpu-bindgen)
  selects WebGPU declarations and generates each runtime's GPU API.
- [xwindow-bindgen](https://github.com/rayzor-blade/xwindow/tree/main/crates/xwindow-bindgen)
  generates window bindings, key/cursor catalogs and browser wire code.

Both keep runtime-specific adapters around shared backend operations. Consult
their adapter implementations for runtime initialization and carrier types.

Run `cargo test --locked --lib` to check generation and ABI conventions.
Runtime execution should also be tested in the consuming adapter; the Node
fixture alone does not exercise Ash, Rayzor or Caribou execution.
