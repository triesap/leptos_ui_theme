# Contributing

Thanks for your interest in contributing to `leptos_ui_theme`.

## Ways to help

- Report bugs and regressions
- Improve documentation and examples
- Add compiler, CLI, and browser test coverage

## Development setup

This repository is a Rust workspace. Typical checks are:

- `rustup run 1.92.0 cargo fmt --all -- --check`
- `rustup run 1.92.0 cargo check --workspace --all-targets --all-features --locked`
- `rustup run 1.92.0 cargo test --workspace --all-targets --all-features --locked`

## Pull request checklist

- Keep changes focused and well-scoped
- Add or update tests when behavior changes
- Keep public APIs documented
- Avoid introducing new unsafe code
- Keep generated output deterministic

## Code style

- Use idiomatic Rust
- Prefer small, composable helpers
- Favor clear, explicit APIs over cleverness

## Accessibility

Changes that affect rendered behavior should include browser coverage for
contrast, forced colors, reduced motion, focus visibility, and first paint.

## License

By contributing, you agree that your contributions are released under the
project license (MIT OR Apache-2.0).
