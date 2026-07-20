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

## Leptos render modes

Generated theme source is render-mode neutral. Final applications select CSR,
hydration, or SSR without the theme dependency plan forcing CSR.

The first-paint compiler owns one canonical bootstrap script semantics with
two security projections: an exact hash-authorized CSR snippet and a
per-response nonce template for SSR. Hydration must attach to deterministic
server component state before adopting the browser bootstrap outcome. Stored
theme selection may change document colors before hydration, but it must not
rewrite the component DOM being hydrated.

## Contributing

See `CONTRIBUTING.md`.

## License

MIT OR Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`.
