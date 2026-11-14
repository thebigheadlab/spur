# Bundled extensions

First-party features beyond the core (LSP, git, picker, file tree, terminal, agents, keymap presets, themes) live here as extensions. They use only the public extension API, so any of them can be disabled without breaking the editor.

| Extension | What it is |
|---|---|
| [`ux_fresh`](ux_fresh) | The default UX pack: the shell's layout, themes (Graphite, Paper, Midnight, Sand), key bindings and messages, as TOML. The `spur` binary embeds it; `spur --ux DIR` loads a copy and reloads it on every change. Format: [ADR 0007](../docs/adr/0007-ux-packs-and-terminal-client.md) and [ADR 0008](../docs/adr/0008-palette-keys-and-settings.md). |

A UX pack is data only, and is the one exception to the rule above for now: the server loads it directly (not through the extension API), and the editor needs one. The Files tree it shows is provided by the server for now (`workspace.files`); it moves behind the extension API once the extension host (#30) lands.
