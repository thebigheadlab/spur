# 6. The core is a server; every UI is a client or a pack

- Status: proposed
- Date: 2026-11-10
- Builds on ADR 0005 (commands, events, context keys). Supersedes the "built-in frontends" part of the architecture spec (#3).

## Context

The editor must look and behave the same in a terminal, a desktop window and a browser, and people must be able to change the look and the keys without touching Rust. If each frontend owns its look, every customization has to be redone per platform, and the core cannot be driven remotely or by tools.

## Decisions

### Three layers

| Layer | Crate / path | Knows about |
|---|---|---|
| Server | `spur-server` (uses `spur-core`, `spur-keymap`, `spur-text`) | Documents, views, commands, keymap state, the UI model. No colours, glyph grids, windows or key defaults. |
| Protocol | `spur-rpc` | Plain data messages and transports. Depends on no other Spur crate. |
| Frontend | `spur-tui` (later desktop, web) | How to draw on one platform. Depends on `spur-rpc` only. |

What the editor is *like* is an **experience pack**: one TOML file (`extensions/ux_fresh/pack.toml`) with theme tokens, a keymap layer and a UI tree. The server evaluates it; frontends draw the result. A test checks that the server crates do not depend on a terminal or window library.

### One actor per session, state per client

A session is one thread that owns `Core<Editor>`. Each attached client has its own view per document, keyboard focus, keymap state (pending keys), context keys, file tree state and Ask state. Before a client's command runs, the session points `Editor::focus` at that client's view, so the existing `editor.*` commands need no change. Edits by one client map the other clients' selections (already done by the text engine). Clients see each other's cursors.

### UI as resolved data

A pack defines named nodes (`row`, `column`, `text`, `spacer`, `surface`, `list`, `input`) and floats. Text binds to context keys (`{buffer.name}`), nodes and spans can be hidden with `when`, and a span can show the key bound to a command in the current focus, so key hints cannot go stale. The server resolves all of this per client and sends a plain tree; a frontend never evaluates a condition. List filtering, selection and text input are server state. No extension code runs between a key press and the frame.

The editor surface is not a node tree: it is sent as frames of visible lines with semantic token names (`syntax.keyword`), cursors and selections. The frontend reports the size its layout gave the surface; the server scrolls.

UI commands (`ask.open`, `files.next`, `ui.toggle_files`, ...) are ordinary commands. Their handlers queue a request that the session applies to the calling client, so they stay bindable, listable and scriptable like any other command.

### Protocol v0

JSON messages in length-prefixed frames (4-byte length, 16 MiB limit). Client to server: `hello`, `key` (key notation), `text`, `resize`, `surface_size`, `click` and `scroll` (node id plus row/column, never screen coordinates), `command`, `ack`, `ping`, `bye`. Server to client: `welcome`, `theme`, `ui`, `surface`, `flush`, `pong`, `bye`. Everything between two `flush` messages is one atomic frame. In-process clients pass the same enums through channels without serializing; tests run every scenario over both.

Sessions are served on a Unix socket in a 0700 per-user folder (`spur serve`, `spur attach`, `spur ls`). `spur [PATH]` runs a private session in-process.

## Consequences

- A new frontend is a renderer for seven node kinds plus the surface; it gets every pack for free.
- Changing a key, a colour or the layout is a data edit; a broken pack falls back to a bundled minimal one with a notice.
- Measured on a 2,000-line file with a 50-row surface: about 0.15 ms of server time per key press, 0.39 ms for the full round trip including a ping. This is above the 0.2 ms target; frames are rebuilt in full each time.

## Not yet done

Diffs for surface frames and UI trees, virtualized lists, MessagePack, version skew handling, Windows named pipes, starting the daemon on demand, live pack switching, per-client packs, predictive echo for remote links, command results over the protocol, and the desktop and web frontends.
