# 6. Editor server and protocol v0

- Status: proposed
- Date: 2026-11-11
- Builds on: ADR 0003 (text model), ADR 0005 (commands, events, context). Architecture: issue #3.

## Context

The core is an editor server. Every UI (terminal, desktop, web) and every tool is a client of it, locally or remotely, and the server holds no look: no colours, glyphs or key bindings. `main` had an editor state (`spur-core::editor`) that no client could reach. This slice adds the server and the first wire protocol, with no UX pack, frontend, daemon or reconnect yet.

Terms: a *slice* is one step of the build plan in the epic (#1): slice 1 is this server, slice 2 the first UX pack and terminal client, slice 4 a daemon with listening sockets and reconnect. A *UX pack* is an extension that supplies the look, layout (the UI tree) and key bindings. An undo *turn* is the number that groups edits into one undo step (ADR 0005). The *anchor view* is the editor's own first view, where `editor.focus` rests between client commands.

## Decisions

### Crates

- `spur-rpc` holds the protocol as plain serde types and the transports. It depends only on serde, so any client (including a future web or third-party one) can use it without the editor.
- `spur-server` holds the session: one thread that owns `Core<Editor>` and serves any number of clients.
- `spur-core` and `spur-text` gained what several clients need (below). Nothing in them knows about clients beyond a view's owner.

### Session thread

One thread owns the editor state; nothing else touches it. Each connection has a reader thread that decodes messages into the session inbox (one channel message per read, so a burst arrives as one batch). A stream connection also has a writer thread that drains the client's outbox; an in-process client's outbox is its endpoint's own queue. Per loop iteration the session handles at most 64 inbox batches, runs due keymap timeouts, renders every dirty frontend, then hands each client one batch. A client whose outbox (1,024 batches) is full is dropped.

A panic while handling a client's message or running its command is caught per message: the client gets an `error` (and a `command` still gets its `result`, with code `internal`), and the rest of its input and the session go on. A panic while drawing a client's frame drops that client with `bye`; the others keep being served. A client removed during a pass (it left, stalled or its frame failed) makes the session render again before it waits, so peers never keep showing it. A stalled stream client's socket is shut down, which also frees a writer stuck on a peer that stopped reading.

### Per-client state

Each client has its own view, size, keymap state (pending keys, counts, timeouts) and context scope (`client.kind`, `client.name` and the view keys), layered over the core context when keys are looked up. Documents are shared.

The core has one `editor.focus`. To run a client's command the session sets focus to the client's view, runs it, reads focus back (a command such as `editor.open` may move it to another view of the same client) and resets focus to a fixed anchor view. A command that moves focus to a view the client does not own is undone by that reset.

Core changes for this:

1. Views record their owning client; `editor.open` reuses only the caller's view of a document, else adds one. Spawn-time files are opened by the system with no owner. `view.move_to` (a mouse press), `end_undo_step`, `scroll_by` and `primary_point` are core APIs, so the server never edits a view's fields itself.
2. Each view numbers its undo turns from `view_id << 32`, so two clients' edits never join one undo step. Undo restores a recorded selection only in the view that made the edit; other views only shift.
3. Edits keep other views' first visible line on the same text, using the pre-edit snapshot.
4. `viewport_of`, `scroll_view`, `remove_view` and `view_keys` address any view, not only the focused one. `viewport_of` cuts each line to a width while reading it, so a huge line never costs more than the width.
5. `editor.open` refuses folders and files over 64 MiB, and `open` and `save` store absolute paths, so one file has one document whatever spelling opened it.

Undo stays per person and document, as with split views in other editors: a human is `User` from every client (ADR 0005), and undo takes that person's newest step, which may have been made in another window. Per-window undo needs a selective undo in `spur-text` and is left as a follow-up decision.

When a client leaves, every view it owns is removed (`view.closed`) and the others redraw without its cursor.

### Principals

Frontends run commands as `Actor::User`. Tools run as `Actor::Remote { client: "<name>#<id>" }`, so their edits are attributed and undone separately, and they are sent no UI or frames. Attach and detach are published as `client.attached` / `client.detached` by `Actor::System { source: "server" }` with `{client, kind, name}`.

### Protocol v0

Messages are JSON objects tagged by `type`.

- Client: `hello {protocols, kind, name, caps, size}`, `key {key}` (one or more keys in key notation, fed one by one), `paste {text}` (one edit and one undo step, never read as keys), `mouse {node, action, line, col, lines}`, `resize {size}`, `layout {surfaces}`, `command {req, id, args}`, `subscribe {filters}`, `ack {frame}`, `ping {id}`, `bye`.
- Server: `welcome {protocol, server, client, session, features}`, `ui {root, focus}`, `surface {node, view}`, `flush {frame}`, `event {seq, name, actor, payload}`, `lagged {missed, first_missed_seq}`, `result {req, ok | error}`, `error {message}`, `pong {id, frame}`, `bye {reason}`.

Order: the first message must be `hello`; over a stream the server reads only its `protocols` field first, so a hello from a newer client with fields this server cannot decode still gets a clear `bye` instead of a parse failure. The server answers with `welcome`, then the first frame. A frame is any `ui` and `surface` messages followed by `flush {frame}`; frames are sent only when something changed.

The UI tree is a fixed bare tree in this slice (one editor surface); UX packs replace it from slice 2. A surface carries the visible lines (at most 500 rows of at most 4,000 chars, with a `truncated` flag), cursors (primary, secondary, and peers' cursors with their client id), selections, the absolute path (clients shorten it), modified flag, mode and document version.

A frontend is redrawn when its own input or a peer's changes what it shows, and also whenever its document's version differs from the one in its last frame, so an edit nobody announced (a tool editing by id, later) still reaches it.

Compatibility: every enum has an `Unknown` catch-all (a command outcome too) and every node carries `children`, so an older receiver keeps working when a newer sender adds a message, node kind or field. Unknown containers lay out as a column; an unknown size is fill.

Framing over byte streams: a 4-byte little-endian length, then the JSON body; frames over 16 MiB are refused, and a command result that would exceed that becomes a `too_large` error. All messages of a batch go out in one write. A reader hands on every complete frame that arrived together and never waits for one that has only partly arrived.

### Flow control and ping

The server keeps at most two frames unacked per client; `ack {frame}` is cumulative. While a client is at the limit its state keeps changing but it gets no frames; its next ack releases the latest state, never a backlog. Events are delivered every iteration regardless.

`pong` is sent after every frame that the messages before the `ping` caused, so `key` then `ping` measures key-to-frame time, and a client can wait for `pong` to know it is up to date. While a frontend is at the frame limit, its pong waits for the frame.

### Transports

In-process channels (tests, embedding) and length-prefixed JSON over any byte stream, with a Unix socket helper that shuts the socket down when either side leaves. Outboxes and client-side queues are bounded (1,024 batches); the server drops a client whose outbox is full. The session inbox is not bounded yet; it needs a limit before slice 4 accepts remote connections. Listening sockets, a daemon and reconnect come in slice 4.

## Latency

`cargo run --release -p spur-server --example latency` sends 10,000 keys per scenario over Unix socket pairs, each key written together with a `ping`, and times each key to its `pong` (the frame has arrived and been decoded). Server time is measured on the session thread, from taking the first message to handing the batch to the writer. Budget: 0.2 ms at p50; p99 is reported alongside.

Measured on a 4-core cloud Linux VM, release build (run-to-run spread on this shared machine is about ±10%):

| Scenario | p50 ms | p99 ms | server p50 | server p99 |
|---|---|---|---|---|
| 80x24, 1 client | 0.166 | 0.211 | 0.041 | 0.064 |
| 80x24, 2 clients | 0.202 | 0.282 | 0.080 | 0.130 |
| 300x100, 1 client | 0.327 | 0.528 | 0.103 | 0.223 |
| 300x100, 2 clients | 0.472 | 0.793 | 0.240 | 0.484 |

80x24 meets the p50 budget with one client and is at it with two; most of the round trip is the two socket hops and thread wakeups, not the server. 300x100 (200 lines of ~300 chars with multi-byte text, about 30 KB of JSON per frame) does not: every frame resends all visible lines, and encoding and decoding them dominates. The planned fix is to send only changed lines (line ids or a line diff in `surface`), which a later protocol version can add without breaking v0 clients. CI does not gate on these numbers; shared runners are too noisy.

## Follow-ups

- Client ids are connection numbers today; resume (slice 4) needs ids that outlive a connection.
- The server tracks one surface per client; UX packs with several surfaces (slice 2) need per-surface view, size and last frame.
- The global context's `editor.*` keys hold whichever client ran last; anything evaluating `when` for a client must use the client's scope.

## Consequences

- Any process that speaks spur-rpc can drive the editor; the terminal and desktop frontends will be clients like the test harness.
- The server sends whole visible lines per frame; large viewports cost more until line diffs land.
- Undo is per person, not per window, until selective undo exists.
- Key bindings come only from layers given to the session; the examples carry a small layer of their own.
