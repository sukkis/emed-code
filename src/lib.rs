//! A keyboard-driven AI coding assistant that lives in your terminal —
//! pair with a local Ollama model or Mistral's cloud API without
//! leaving your terminal or tmux session.
//!
//! [`core`] holds conversation state, provider round-trips, and tool
//! execution — the part of this crate most worth reading if you're
//! integrating against it directly rather than running the `emed-code`
//! binary. [`tui`] renders the terminal interface on top of it; [`cli`]
//! parses the command-line flags `main.rs` uses to wire the two
//! together.

pub mod cli;
pub mod core;
pub mod tui;
