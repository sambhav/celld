# Cell packing checks

Run the focused Rust checks from the repository root:

```sh
cargo test --manifest-path tools/packing-checks/Cargo.toml --locked
```

This standalone test crate compiles the production `env_vars.rs` and
`logic/isolate.rs` modules directly. It needs no V8 build. The five tests cover:

- the default density of 32, valid limits from 1 to 32, and invalid settings;
- growth at the configured density for a sequence of 40 cell placements;
- filling live heaps while excluding retiring heaps;
- retaining nonempty heaps and waiting for outstanding requests before freeing.

These checks do not exercise the full runtime or validate S3 durability.
The matching native change was also built and tested with four Python
integration tests before the
[GitHub density benchmark](https://github.com/sambhav/celld-python/actions/runs/33981405520).
That run used upstream `a52f9905425bc41134d817694bdc2c50bcc5e856` plus the
same runtime change, Rust 1.98.1, and the `lab` build profile. Its
[results and reproduction instructions](https://github.com/sambhav/celld-python/tree/43a8e138c57b3f51ad2257ae0957a247ab674083/experiments/packing)
record the CPU/memory tradeoff and the limitations of the local development store.
