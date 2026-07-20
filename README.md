# leptos_ui_theme

`leptos_ui_theme` is a source-first design-token compiler and CLI for Leptos
applications built with `leptos_ui_kit`. It validates local DTCG 2025.10 token
and resolver files against an installed kit contract, then generates
deterministic CSS, Rust theme metadata, first-paint bootstrap code, HTML
integration, and a lock file.

Install the CLI and initialize it from an application directory:

```text
leptos_ui_theme init
leptos_ui_theme add <theme-id> --from-contract-defaults
leptos_ui_theme build
leptos_ui_theme check
leptos_ui_theme doctor --strict
```

`build` is deterministic and idempotent. `check`, `list`, `explain`, and
`doctor --strict` inspect the project without writing. The tool never edits
kit-owned files or application dependencies.

## Contributing

See `CONTRIBUTING.md`.

## License

MIT OR Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`.
