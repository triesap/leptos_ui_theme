# leptos_ui_theme

`leptos_ui_theme` is a design-token compiler and CLI for Leptos applications
built with `leptos_ui_kit`. It validates local DTCG 2025.10 token and resolver
files against the installed UI kit, then generates CSS, Rust theme metadata,
first-paint code, HTML integration, and a project lock file.

## Quick start

Run these commands from the application directory:

```text
leptos_ui_theme init
leptos_ui_theme add <theme-id> --from-contract-defaults
leptos_ui_theme build
leptos_ui_theme check
leptos_ui_theme doctor --strict
```

`build` produces the same output for the same inputs and is safe to repeat.
`check`, `list`, `explain`, and `doctor --strict` inspect the project without
writing. The CLI does not edit kit-owned files or application dependencies.

## Leptos render modes

Generated theme code supports CSR, hydration, and SSR. The `leptos` and
`web_ui_primitives` dependencies must select the same delivery feature: `csr`,
`hydrate`, or `ssr`. Shared libraries select no delivery feature. Browser theme
preferences are applied after hydration so the server and browser start from
the same rendered state.

## Contributing

See `CONTRIBUTING.md`.

## License

MIT OR Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`.
