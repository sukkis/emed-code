test:
    cargo test --features local

fmt: 
    cargo fmt --check

clippy:
    cargo clippy -- -D warnings
    
ci:
    cargo test

all: fmt clippy ci
