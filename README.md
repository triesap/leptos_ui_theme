# leptos_ui_theme

`leptos_ui_theme` compiles DTCG 2025.10 design tokens for Leptos applications
using `leptos_ui_kit`. It validates tokens against the installed kit and
generates CSS, Rust theme data, startup theme code, HTML changes, and a lock
file.

## Usage

```text
leptos_ui_theme init
leptos_ui_theme add <theme-id> --from-contract-defaults
leptos_ui_theme build
leptos_ui_theme check
leptos_ui_theme doctor --strict
```

Run these commands from the application directory. `build` is repeatable for
the same inputs. `check`, `list`, `explain`, and `doctor --strict` inspect
without writing. The CLI does not change kit files or dependencies.

## Render modes

Generated code supports client-side rendering (`csr`), hydration (`hydrate`),
and server-side rendering (`ssr`). Applications must enable the same render
feature for `leptos` and `web_ui_primitives`. Shared libraries enable none.

See [CONTRIBUTING.md](CONTRIBUTING.md). Licensed under MIT OR Apache-2.0; see
[LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
