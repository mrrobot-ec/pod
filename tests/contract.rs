// Cargo only builds `tests/*.rs` and `tests/*/main.rs`; this shim makes the
// harness in `tests/contract/mod.rs` a test crate without a Cargo.toml edit.
#[path = "support/mod.rs"]
mod support;

#[path = "contract/mod.rs"]
mod contract;
