# Spur

A native, modal, agent-first editor and development environment, with a terminal (TUI) and a desktop (GUI) frontend.

> Status: pre-alpha. The editor server, protocol v0, the first UX pack and the terminal client work; agents, search and git views come next. The plan lives in the [epic](https://github.com/thebigheadlab/spur/issues/1).

## Vision

- **Customizable without chaos.** Declarative TOML for settings, keymaps and layouts; typed TypeScript extensions; WASM for heavy plugins.
- **Any editor's muscle memory.** Keymap presets for Vim, Helix/Kakoune, Emacs, VS Code, JetBrains and Sublime.
- **Native and lightweight.** Rust core, TUI plus a winit + wgpu GUI on macOS, Linux and Windows.
- **Minimal core.** Everything beyond the core is an extension, first-party features included.
- **Agents are first-class.** Terminal agents with hooks, ACP and MCP, with review, permissions and checkpoints.
- **Remote is first-class.** The core always runs as a session server; frontends are clients, locally or over SSH.
- **Edits itself.** Change anything through settings or by asking an agent.

## Layout

| Path | Purpose |
|---|---|
| `crates/spur-text` | Text engine: rope, anchors, undo |
| `crates/spur-core` | Buffers, views, commands, events, config |
| `crates/spur-keymap` | Keymap and mode engine |
| `crates/spur-ext` | Extension host and capabilities |
| `crates/spur-rpc` | Client protocol (v0) and transports |
| `crates/spur-server` | The editor server: one editor state, many clients |
| `crates/spur-tui` | Terminal frontend |
| `crates/spur-gui` | Desktop frontend (winit + wgpu) |
| `crates/spur` | The `spur` binary |
| `extensions/` | Bundled first-party extensions |
| `docs/adr/` | Architecture decision records |

## Build

```sh
cargo build
cargo test
cargo run
```

The toolchain is pinned in `rust-toolchain.toml`; `rustup` installs it automatically.

Two examples (Unix only) show the server at work:

```sh
# Two clients edit one file; a plain debug view of both clients' frames.
cargo run --release -p spur-server --example two_clients
# Key-to-frame latency over Unix sockets.
cargo run --release -p spur-server --example latency
```

The server and protocol are described in [ADR 0006](docs/adr/0006-editor-server-and-protocol-v0.md); UX packs and the terminal client in [ADR 0007](docs/adr/0007-ux-packs-and-terminal-client.md).

## Run

```sh
cargo run --release -- src/main.rs      # open a file
cargo run --release -- path/to/project  # open a folder (an untitled document)
cargo run --release -- --theme paper    # another bundled theme: graphite, paper, midnight, sand
```

`spur [--ux DIR] [--theme NAME] [--] [PATH...]`. The shell has a top bar, an icon rail with a switchable sidebar (Files works now), the code in the middle and the agent column on the right. Columns hide themselves as the terminal narrows so the code keeps at least 64 cells.

| Keys | Action |
|---|---|
| ctrl+s, ctrl+z, ctrl+shift+z or ctrl+y | Save, undo, redo |
| ctrl+b | Show or hide the sidebar |
| ctrl+alt+b (or ctrl+k a) | Show or hide the agent column |
| ctrl+shift+e / j / f / g | Sidebar: Files, Sessions, Search, Git |
| alt+1..4, or ctrl+k then e / j / f / g | The same, for terminals that cannot send ctrl+shift |
| Up, Down, PageUp, PageDown, Right, Left, Enter | Move in the Files tree, expand, collapse, open |
| Esc | Back to the code |
| ctrl+q | Quit (asks once if anything is unsaved) |

The mouse works too: click a rail icon, a file or the code; the wheel scrolls.

Terminal notes:

- ctrl+shift keys need a terminal with the kitty keyboard protocol (kitty, WezTerm, foot, Ghostty, recent iTerm2); elsewhere use the alt or ctrl+k forms.
- Inside tmux, ctrl+b is the tmux prefix: press it twice.
- On macOS, alt+digit needs Option set to act as Meta in the terminal's settings.
- Set `SPUR_ICONS=ascii` if the rail icons look wrong (the Linux console gets ASCII icons by default).

If Spur ends in any way other than a quit, modified files are saved as rescue copies under `~/.local/state/spur/rescue/` (or `$XDG_STATE_HOME/spur/rescue/`), and their paths are printed.

## Change the look

The look, layout and keys come from a UX pack, [`extensions/ux_fresh`](extensions/ux_fresh), which is built into the binary. To change it, copy the folder and run with `--ux`:

```sh
cp -r extensions/ux_fresh ~/my-pack
cargo run --release -- --ux ~/my-pack src/main.rs
```

Edit any file in `~/my-pack` (for example `accent` in `themes/graphite.toml`, `theme` in `pack.toml`, or a binding in `keys.toml`) and the running editor picks it up within a second. A mistake keeps the last good pack and shows the error, with its file and line, at the bottom of the screen.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in Spur by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
