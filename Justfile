test:
    cargo test --features local

ci:
    cargo fmt --check
    cargo clippy -- -D warnings
    cargo test

all: ci
