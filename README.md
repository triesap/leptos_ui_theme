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

Kit discovery consumes a typed `installed-kit-capability.json`; it does not
reinterpret the kit generator's own install lock. `kit.capabilityPaths` and an
optional `kit.contractPath` are relative to a caller-provided security
workspace root. Token inputs, HTML, and outputs remain relative to the
directory containing `leptos-ui-theme.json`. Library consumers use
`ThemeCompiler::load_with_workspace` and
`leptos_ui_theme_codegen::build_with_workspace` when those roots differ.
Capability fingerprints omit physical paths, so an otherwise identical
installation retains its identity after relocation.

Patched HTML must contain exactly one line-bounded
`<!-- leptos-ui-theme:anchor -->`. The compiler inserts or reconciles only its
managed region after that anchor and preserves all existing Trunk links and
other app-owned markup.

## Contributing

See `CONTRIBUTING.md`.

## License

MIT OR Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`.
