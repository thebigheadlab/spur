# Spur

A native, agent-first editor and development environment, with a terminal (TUI) and a desktop (GUI) frontend.

> Status: pre-alpha. Scaffolding only. The plan lives in the [epic](https://github.com/thebigheadlab/spur/issues/1).

## Vision

- **Customizable without chaos.** Declarative TOML for settings, keymaps and layouts; typed TypeScript extensions; WASM for heavy plugins.
- **Its own way of working.** One mode: typing types. Everything else is on the Ask card (`ctrl+space`), which opens at the caret and takes a command, a line number or text to find. Selections grow by meaning (`alt+up`: word, line, block, file).
- **Bring your muscle memory if you want.** Key presets ship as optional extensions (Vim first: `spur --preset vim`); none of them is in the core.
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
| `crates/spur-rpc` | Session server protocol and transports |
| `crates/spur-tui` | Terminal frontend |
| `crates/spur-gui` | Desktop frontend (winit + wgpu) |
| `crates/spur` | The `spur` binary |
| `extensions/` | Bundled first-party extensions (`extensions/vim`: optional Vim keys) |
| `docs/adr/` | Architecture decision records |

## Build

```sh
cargo build
cargo test
cargo run
```

The toolchain is pinned in `rust-toolchain.toml`; `rustup` installs it automatically.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in Spur by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
