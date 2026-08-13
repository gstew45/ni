# ni

A grid-battle arena where bot processes fight over gRPC. They demand shrubbery.

Ni is the running example for the **Wires & Boundaries** trace — see
[`wires-and-boundaries.md`](wires-and-boundaries.md) for the blog series,
[`ni-design.md`](ni-design.md) for the game design, and
[`implementation-plan.md`](implementation-plan.md) for how it gets built.

## Layout

```
proto/ni/v1/ni.proto   the contract — engine is the gRPC client, bots are servers
crates/ni-proto        generated types and stubs (tonic-build, vendored protoc)
crates/ni-game         pure, deterministic rules engine — no gRPC, no async, no I/O
crates/ni-engine       match engine + thin runner CLI + text renderer
crates/ni-bot          reference bot
```

## Building

```sh
cargo build            # no system protoc needed — the build script vendors it
cargo test --workspace
```

Proto linting and breaking-change checks use [buf](https://buf.build):
`buf lint`, and `buf breaking --against '.git#branch=main'` in CI.
