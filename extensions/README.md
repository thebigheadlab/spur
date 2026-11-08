# Bundled extensions

First-party features beyond the core (LSP, git, picker, file tree, terminal, agents, keymap presets, themes) live here as extensions. They use only the public extension API, so any of them can be disabled without breaking the editor.

| Extension | What it adds |
|---|---|
| [`vim`](vim/) | Optional Vim keys: modes, operator grammar, `:` line, `.` repeat, macros. `spur --preset vim`. |

Until the extension host (#30) lands, bundled extensions are Rust crates the `spur` binary links behind a cargo feature and loads only on request.
