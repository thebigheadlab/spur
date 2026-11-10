# Bundled extensions

First-party features beyond the core (LSP, git, picker, file tree, terminal, agents, keymap presets, themes) live here as extensions. They use only the public extension API, so any of them can be disabled without breaking the editor.

## Experience packs

An experience pack says what the editor is like: theme tokens, keys and the UI tree, as one TOML file. The server evaluates it and every frontend draws the result, so a pack works in the terminal, the desktop app and the web alike.

| Path | What |
|---|---|
| `ux_fresh/pack.toml` | Fresh, the default experience |
| `fallback.toml` | Minimal pack used when another pack fails to load |

Both are embedded in the `spur` binary. Try your own with `spur --ux my-pack.toml`.

The extension host for code extensions is tracked in #30.
