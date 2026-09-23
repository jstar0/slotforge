# Contributing to SlotForge

Thanks for helping improve the reservation engine. Keep changes small, deterministic, and easy to audit.

## Local checks

```bash
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo run --release -- demo --output-dir outputs/demo
cargo run --release -- benchmark --output-dir outputs/benchmark
```

Do not add real attendee data, credentials, customer names, copied assets, or production exports. New behavior needs a focused test and an update to the architecture or benchmark note when the contract changes.
