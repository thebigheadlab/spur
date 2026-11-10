# Spur

A native, modal, agent-first editor and development environment, with a terminal (TUI) and a desktop (GUI) frontend.

> Status: pre-alpha. An editor server, a terminal frontend and one experience pack work end to end. The plan lives in the [epic](https://github.com/thebigheadlab/spur/issues/1).

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
| `crates/spur-server` | The editor server: sessions, per-client state, UI model, experience packs |
| `crates/spur-rpc` | Client/server protocol and transports |
| `crates/spur-tui` | Terminal frontend: draws what the server sends |
| `crates/spur-gui` | Desktop frontend (winit + wgpu) |
| `crates/spur` | The `spur` launcher |
| `extensions/` | Bundled first-party extensions, including the default experience pack |
| `docs/adr/` | Architecture decision records |

## Build

```sh
cargo build
cargo test
cargo run
```

## Try it

```sh
cargo run -- src/main.rs          # edit a file; ctrl+space opens Ask, ctrl+q quits
cargo run -- serve                # run a session without a UI...
cargo run -- attach               # ...and attach to it from any number of terminals
cargo run -- --ux my-pack.toml .  # use your own keys, colours and layout
```

The core is a server and never draws. What the editor looks like and which keys it uses come from an experience pack, a single TOML file: copy [`extensions/ux_fresh/pack.toml`](extensions/ux_fresh/pack.toml) and edit it. See [ADR 0006](docs/adr/0006-server-core-and-experience-packs.md).

The toolchain is pinned in `rust-toolchain.toml`; `rustup` installs it automatically.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in Spur by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
