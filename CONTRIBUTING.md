# Contributing to Bloop

Thanks for your interest in contributing to bloop!

## Getting Started

```bash
# Clone the repo
git clone https://github.com/jaikoo/bloop.git
cd bloop

# Build (core only)
cargo build

# Build with all features
cargo build --features "analytics,llm-tracing"

# Run tests
cargo test

# Run LLM tracing tests
cargo test --features llm-tracing --test llm_tracing_test
```

## Code Style

- Use `rustfmt` defaults — run `cargo fmt` before committing
- Run `cargo clippy` and fix any warnings

## Pull Requests

1. Fork the repo and create a feature branch
2. Make your changes with tests where appropriate
3. Run `cargo fmt` and `cargo clippy`
4. Run `cargo test` (and `cargo test --features "analytics,llm-tracing"` if touching those modules)
5. Open a PR with a clear description of what changed and why

## Reporting Issues

Use [GitHub Issues](https://github.com/jaikoo/bloop/issues). Include:
- Steps to reproduce
- Expected vs actual behavior
- Bloop version and OS

## License

By contributing, you agree that your contributions will be licensed under the Apache License 2.0.
