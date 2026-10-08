# x-idl

Generate native runtime bindings from one Rust declaration and expanded WebIDL
model. Targets include HashLink/Ash, JavaScript runtimes, and Node with
TypeScript declarations.

## TypeScript and Node

```rust
let binding = x_idl::node::generate(
    "gpu",
    "api/gpu.rs",
    include_str!("api/webgpu.idl"),
)?;
std::fs::write(out_dir.join("gpu.rs"), binding.rust)?;
std::fs::write("generated/gpu.ts", binding.typescript)?;
```

Include the generated Rust module in a Node-API addon and expose its `call`
entry point. Bind its JavaScript export once:

```ts
import { bind } from './generated/gpu.js';

const gpu = bind({ call: addon.call });
const instance = gpu.GpuInstance.new();
```

`generate_with_entrypoint` supports multiple generated APIs in one addon.
For declarations alone:

```sh
cargo run --bin xidl-typescript -- gpu api/gpu.rs api/webgpu.idl generated/gpu.ts
```

See [Node binding contracts](docs/node.md).

## Verification

Requires Rust and Node.js 24+ for the integration fixture.

```sh
cargo test --lib
node tests/node-addon/run.mjs
```

The fixture loads a compiled addon and verifies descriptors, typed resources,
tagged events, bytes, large integers, asynchronous completion and invalid input.
CI runs it on Linux, macOS and Windows.

MIT.
