# Node and TypeScript target

The Node target uses the same expanded API model as the other runtime targets.
It emits schema-specific Rust conversions and TypeScript descriptor encoders,
plus numeric operation dispatch. It does not walk arbitrary object trees on
every method call.

## Generate an adapter

Add `x_idl` as a build dependency and generate both sides from the same inputs:

```rust
let binding = x_idl::node::generate(
    "gpu",
    std::path::PathBuf::from("api/gpu.rs"),
    include_str!("api/webgpu.idl"),
)?;
std::fs::write(out_dir.join("gpu.rs"), binding.rust)?;
std::fs::create_dir_all("generated")?;
std::fs::write("generated/gpu.ts", binding.typescript)?;
```

Include the generated Rust module in a Node-API addon and expose its `call`
entry point. Bind its JavaScript export once:

```ts
import { bind } from './generated/gpu.js';

const gpu = bind({ call: addon.call });
const instance = gpu.GpuInstance.new();
```

For TypeScript output alone:

```sh
cargo run --bin xidl-typescript -- gpu api/gpu.rs api/webgpu.idl generated/gpu.ts
```

[The compiled fixture](../tests/node-addon) contains a build script, native
backend and JavaScript integration test. See [runtime integration](runtimes.md)
for the corresponding Ash/HashLink, Rayzor and Caribou generators.

## Runtime ownership

Use `xidl-node` for the generated model's carrier types and `napi` /
`napi-derive` for the addon entry point. Generated entry points enter a runtime
scope, convert arguments, call the backend and convert the result.

A scope confines borrowed arguments to one synchronous native call. Text uses
owned UTF-8 storage; synchronous byte inputs borrow checked views.
`Rooted<Buffer>` snapshots borrowed bytes before retention and shares already
owned buffers. Mutable byte outputs write directly into the supplied view.
Views are checked again before use so detached backing stores cannot become
stale native pointers.

Futures retain their state and payload independently of call scratch space.
Native workers settle owned results; conversion and JavaScript promise
completion happen on the owning Node thread.

## Types and validation

Generated interfaces expose typed descriptors, numeric enum unions, tagged
input unions and tagged events. Resource methods close over their binding.
Native resources use opaque externals with runtime type checks.

64-bit integers and pointers use `bigint`; numeric arguments are range checked.
Null native resources remain null. Unit futures resolve to undefined.

Detached buffers, shared byte storage, wrong resource types, worker calls and
native reentrancy are rejected. The runtime converts backend errors and caught
panics into JavaScript exceptions. Backend native operations remain responsible
for their own validation and synchronization.

Each generated binding contains a fingerprint of its expanded model.
`bind()` compares it with the native entry point before constructing the API.
Rebuild the native module and TypeScript declarations together on mismatch.

## Backend reuse

`node::generate_with_entrypoint(namespace, declaration, webidl, entrypoint)`
selects a unique exported function, allowing independent APIs to share one
addon. Keep the existing backend modules and supply `xidl-node` at their runtime
adapter boundary.

Generated TypeScript is valid before formatting. Format it with the consuming
project's Prettier configuration after generation, and check in the result if
the package should type check without a native build.
