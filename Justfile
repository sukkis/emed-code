test:
    cargo test --features local

mistral:
    cargo test --features local --test real_mistral_smoke_test

ollama:
    cargo test --features local --test real_ollama_smoke_test

fmt:
    cargo fmt --check

clippy:
    cargo clippy -- -D warnings
    
ci:
    cargo test

all: fmt clippy ci
