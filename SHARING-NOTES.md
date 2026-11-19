Zarncode source sharing package

Current source snapshot, including the new harness work.
Git metadata/files, AI chat/session history, personal configuration, hidden files, build caches, and compiled binaries are excluded.

Build: cargo build
Offline preview: cargo run -- --preview
Offline harness checks: cargo test --test harness
Rust toolchain: 1.97.1 (see rust-toolchain.toml).

Live use needs a configured provider. Kimi is implemented; Zarnkina, Zarnlib and Zarngate remain roadmap work.
Verification limitation: 16 pre-existing Windows unit-test failures remain; the 22 harness integration checks pass.
