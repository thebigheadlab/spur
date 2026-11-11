# 6. Editor server and protocol v0

- Status: proposed
- Date: 2026-11-11
- Builds on: ADR 0003 (text model), ADR 0005 (commands, events, context). Architecture #3.

## Context

The core is an editor server. Every UI (terminal, desktop, web) and every tool is a client of it, locally or remotely, and the server holds no look: no colours, glyphs or key bindings. `main` had an editor state (`spur-core::editor`) that no client could reach. This slice adds the server and the first wire protocol, with no UX pack, frontend, daemon or reconnect yet.

## Decisions

### Crates

- `spur-rpc` holds the protocol as plain serde types and the transports. It depends only on serde, so any client (including a future web or third-party one) can use it without the editor.
- `spur-server` holds the session: one thread that owns `Core<Editor>` and serves any number of clients.
- `spur-core` and `spur-text` gained what several clients need (below). Nothing in them knows about clients beyond a view's owner.

### Session thread

One thread owns the editor state; nothing else touches it. Each connection has a reader thread that decodes frames into the session inbox (one channel message per read, so a burst arrives as one batch) and a writer thread that drains the client's outbox. Per loop iteration the session handles at most 64 inbox batches, runs due keymap timeouts, renders every dirty frontend, then hands each client one batch. A client whose outbox (1,024 batches) is full is dropped.

A command or render that panics is caught; the client gets an `error` and the session keeps serving.

### Per-client state

Each client has its own view, size, keymap state (pending keys, counts, timeouts) and context scope (`client.kind`, `client.name` and the view keys), layered over the core context when keys are looked up. Documents are shared.

The core has one `editor.focus`. To run a client's command the session sets focus to the client's view, runs it, reads focus back (a command such as `editor.open` may move it to another view of the same client) and resets focus to a fixed anchor view. A command that moves focus to a view the client does not own is undone by that reset.

Core changes for this:

1. Views record their owning client; `editor.open` reuses only the caller's view of a document, else adds one. Spawn-time files are opened by the system with no owner.
2. Each view numbers its undo turns from `view_id << 32`, so two clients' edits never join one undo step. Undo restores a recorded selection only in the view that made the edit; other views only shift.
3. Edits keep other views' first visible line on the same text, using the pre-edit snapshot.
4. `viewport_of`, `scroll_view`, `remove_view` and `view_keys` address any view, not only the focused one.
5. `editor.open` refuses folders and files over 64 MiB.

Undo stays per person and document, as with split views in other editors: a human is `User` from every client (ADR 0005), and undo takes that person's newest step, which may have been made in another window. Per-window undo needs a selective undo in `spur-text` and is left as a follow-up decision.

When a client leaves, every view it owns is removed (`view.closed`) and the others redraw without its cursor.

### Principals

Frontends run commands as `Actor::User`. Tools run as `Actor::Remote { client: "<name>#<id>" }`, so their edits are attributed and undone separately, and they are sent no UI or frames. Attach and detach are published as `client.attached` / `client.detached` by `Actor::System { source: "server" }` with `{client, kind, name}`.

### Protocol v0

Messages are JSON objects tagged by `type`.

- Client: `hello {protocols, kind, name, caps, size}`, `key {key}` (key notation), `paste {text}` (one edit and one undo step, never read as keys), `mouse {node, action, line, col, lines}`, `resize`, `layout {surfaces}`, `command {req, id, args}`, `subscribe {filters}`, `ack {frame}`, `ping {id}`, `bye`.
- Server: `welcome {protocol, server, client, session, features}`, `ui {root, focus}`, `surface {node, view}`, `flush {frame}`, `event {seq, name, actor, payload}`, `lagged`, `result {req, ok | error}`, `error {message}`, `pong {id, frame}`, `bye {reason}`.

Order: the first message must be `hello`; the server reads only its `protocols` field first, so a hello from a newer client with fields this server cannot decode still gets a clear `bye` instead of a parse failure. The server answers with `welcome`, then the first frame. A frame is any `ui` and `surface` messages followed by `flush {frame}`; frames are sent only when something changed.

The UI tree is a fixed bare tree in this slice (one editor surface); UX packs replace it from slice 2. A surface carries the visible lines (at most 4,000 chars each, with a `truncated` flag), cursors (primary, secondary, and peers' cursors with their client id), selections, path, modified flag, mode and document version.

Compatibility: every enum has an `Unknown` catch-all and every node carries `children`, so an older receiver keeps working when a newer sender adds a message, node kind or field. Unknown containers lay out as a column; an unknown size is fill.

Framing over byte streams: a 4-byte little-endian length, then the JSON body; frames over 16 MiB are refused. All messages of a batch go out in one write.

### Flow control and ping

The server keeps at most two frames unacked per client; `ack {frame}` is cumulative. While a client is at the limit its state keeps changing but it gets no frames; its next ack releases the latest state, never a backlog. Events are delivered every iteration regardless.

`pong` is sent after every frame that the messages before the `ping` caused, so `key` then `ping` measures key-to-frame time, and a client can wait for `pong` to know it is up to date. While a frontend is at the frame limit, its pong waits for the frame.

### Transports

In-process channels (tests, embedding) and length-prefixed JSON over any byte stream, with a Unix socket helper that shuts the socket down when either side leaves. Listening sockets, a daemon and reconnect come in slice 4.

## Latency

`cargo run --release -p spur-server --example latency` sends 10,000 keys per scenario over Unix socket pairs, each key written together with a `ping`, and times each key to its `pong` (the frame has arrived and been decoded). Server time is measured on the session thread, from taking the first message to handing the batch to the writer. Budget: 0.2 ms.

Measured on a 4-core cloud Linux VM, release build:

| Scenario | p50 ms | p99 ms | server p50 | server p99 |
|---|---|---|---|---|
| 80x24, 1 client | 0.156 | 0.329 | 0.041 | 0.122 |
| 80x24, 2 clients | 0.196 | 0.318 | 0.077 | 0.163 |
| 300x100, 1 client | 0.333 | 0.605 | 0.112 | 0.271 |
| 300x100, 2 clients | 0.443 | 0.891 | 0.236 | 0.497 |

80x24 meets the p50 budget. 300x100 (200 lines of ~300 chars with multi-byte text, about 30 KB of JSON per frame) does not: every frame resends all visible lines, and encoding and decoding them dominates. The planned fix is to send only changed lines (line ids or a line diff in `surface`), which a later protocol version can add without breaking v0 clients. CI does not gate on these numbers; shared runners are too noisy.

## Consequences

- Any process that speaks spur-rpc can drive the editor; the terminal and desktop frontends will be clients like the test harness.
- The server sends whole visible lines per frame; large viewports cost more until line diffs land.
- Undo is per person, not per window, until selective undo exists.
- Key bindings come only from layers given to the session; the examples carry a small layer of their own.
