# Contributing

Requirements: Windows 11, Rust via rustup (the pinned toolchain installs itself), and Visual Studio 2022 Build Tools with the C++ workload and the Windows 11 SDK.

Build and test:

    cargo build --workspace
    cargo test --workspace
    cargo fmt --check
    cargo clippy --workspace --all-targets -- -D warnings

Commit style: `type: summary`, for example `feat: add clock widget` or `fix: restore taskbar on panic`. Common types: feat, fix, chore, docs, test, refactor.
