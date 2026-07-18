# leptos_ui_theme

`leptos_ui_theme` is a source-first design-token compiler and CLI for Leptos
applications built with `leptos_ui_kit`. It validates local DTCG token sources
against the installed kit contract, then generates deterministic CSS, Rust
theme metadata, first-paint bootstrap code, and a lock file.

Typical usage:

```text
leptos_ui_theme init
leptos_ui_theme add <theme-id> --from-contract-defaults
leptos_ui_theme build
leptos_ui_theme check
leptos_ui_theme doctor --strict
```

The compiler validates closed project and kit-contract models, resolves local
DTCG resolver sources, and writes byte-stable theme artifacts. `build` is
idempotent, while `check`, `list`, `explain`, and strict `doctor` are read-only.

## Contributing

See `CONTRIBUTING.md`.

## License

MIT OR Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`.
