# Browser wire generation

x-idl can generate the two halves of a browser adapter from WebIDL: Rust code
for the guest plugin and JavaScript for the agent that owns browser objects.
Ash and Caribou adapters can use this alongside their native binding model;
the transport is separate from the Node-API target.

```rust
let idl = std::fs::read_to_string("api/browser.idl")?;
let wire = x_idl::wire::wire(&idl)?;
std::fs::write(out.join("browser_wire.rs"), wire.rust)?;
std::fs::write(out.join("browser_wire.mjs"), wire.js)?;
```

The wire encodes supported operations and attributes as numeric commands
with typed binary payloads. It handles resource IDs, strings, enums,
dictionaries, sequences, records, unions and asynchronous replies. Commands
can be batched through a mailbox in shared memory.

For events posted from the agent to the guest, name the WebIDL dictionaries:

```rust
let wire = x_idl::wire::wire_posting(&idl, &["XwEvent"])?;
```

This adds JavaScript posting functions and Rust queue decoding for those
records. See [xwindow's browser definitions](https://github.com/rayzor-blade/xwindow/tree/main/api/spec)
for a concrete schema.

The consuming runtime creates the page, worker, shared memory and canvas,
and starts the generated service. A WebIDL declaration only supplies the
schema; supported operations depend on the browser and adapter backend.
`web_backend` and `hashlink_web_backend` can generate partial adapter backends
with explicit unavailable-operation fallbacks.

## Verification

The wire integration tests generate Rust and JavaScript and execute both
sides, using Node and Rust with the `wasm32-unknown-unknown` target:

```sh
rustup target add wasm32-unknown-unknown
cargo test --locked --test wire_calls --test wire_queue
```
