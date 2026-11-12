//! Spur's client protocol, version 0, and its transports.
//!
//! The editor server owns all state and never draws. A client says `hello`, then
//! draws what the server sends (a UI tree and surface contents) and sends input back.
//! This crate depends only on serde and `serde_json`, so a client links the protocol
//! and nothing else.
//!
//! Two words: a **message** is one JSON object on the wire; a **frame** is what the
//! server sends for one change to draw: any `ui` and `surface` messages, closed by
//! `flush {frame}`. Clients ack frames, not messages.
//!
//! # Reaching a server
//!
//! Until Spur has a listening socket (a later slice), a program embeds
//! `spur_server::Session`, passes it key binding layers (the server ships none), and
//! attaches clients with [`in_process`] or over a byte stream such as a Unix socket
//! pair (`Session::attach_unix` with [`ClientEnd::unix`]). See the `spur-server`
//! examples.
//!
//! # Conversation
//!
//! 1. Client: `hello` listing the versions it speaks. Server: `welcome` (or `bye`
//!    with a reason), then the first frame: `ui`, one `surface` per surface,
//!    `flush {frame: 1}`.
//! 2. Client: input (`key`, `paste`, `mouse`, `resize`, `layout`), `command`s and
//!    `subscribe`. For each batch of input the server sends, in order: `result` and
//!    `error` messages the input caused, `event`s, then a frame if anything shown
//!    changed, then any `pong`.
//! 3. The client acks each frame it drew (`ack` is cumulative). The server keeps at
//!    most two unacked frames in flight; a slow client gets the latest state, not
//!    every intermediate one. A client must ack, or it stops getting frames.
//! 4. `ping` → `pong {frame}` once every earlier message is handled and the frames
//!    showing it are sent. A key still waiting for a longer binding (a pending
//!    prefix) is handled when it resolves, which may be after the pong.
//! 5. Tool clients (scripts) get no `ui`, `surface` or `flush`.
//! 6. The connection may end without `bye`: `bye` is sent when the server refuses a
//!    client, not when the session shuts down or drops a client that stopped
//!    reading. Treat end of stream as the end of the session.
//!
//! # Example
//!
//! ```text
//! → {"type":"hello","protocols":[0],"kind":"frontend","name":"tui",
//!    "caps":{"truecolor":true,"mouse":true},"size":{"cols":80,"rows":24}}
//! ← {"type":"welcome","protocol":0,"server":"0.1.0","client":1,"session":"main","features":[]}
//! ← {"type":"ui","root":{"id":"root","kind":"column","width":{"type":"fill","weight":1},
//!    "height":{"type":"fill","weight":1},"children":[{"id":"editor","kind":"surface",
//!    "width":{"type":"fill","weight":1},"height":{"type":"fill","weight":1}}]},
//!    "focus":"editor"}
//! ← {"type":"surface","node":"editor","view":{"doc":2,"version":0,"top_line":0,
//!    "line_count":2,"lines":[{"spans":[{"text":"hi"}]},{"spans":[{"text":""}]}],
//!    "cursors":[{"line":0,"col":0,"kind":"primary"}],"selections":[],
//!    "path":"notes.txt","modified":false,"mode":"insert"}}
//! ← {"type":"flush","frame":1}
//! → {"type":"ack","frame":1}
//! → {"type":"key","key":"x"}  {"type":"ping","id":1}
//! ← {"type":"surface",...}  {"type":"flush","frame":2}  {"type":"pong","id":1,"frame":2}
//! → {"type":"command","req":7,"id":"editor.open","args":{"path":"b.txt"}}
//! ← {"type":"result","req":7,"ok":{"doc":3}}  (or "error":{"code":...,"message":...})
//! ```
//!
//! Required fields: `type` on every message; `hello.protocols` and `kind` (`name`,
//! `caps` and `size` default to empty, off, cells and zero); each node's `id`,
//! `kind`, `width` and `height`. Some fields are left out when empty: a node's
//! `children`, `Line.truncated`, `Cursor.peer`, `Span.hl`; `path` is `null` for a
//! document with no file, else absolute. `version` is the
//! document's text version, rising with every edit; `line_count` counts the empty
//! line after a final newline.
//!
//! # Input
//!
//! `key` carries one or more keys in key notation, fed one by one: plain chars
//! (`a`, `A`), named keys in angle brackets (`<Esc>`, `<CR>`, `<Tab>`, `<BS>`,
//! `<Del>`, `<Up>`, `<Down>`, `<Left>`, `<Right>`, `<Home>`, `<End>`, `<PageUp>`,
//! `<PageDown>`, `<Insert>`, `<Space>`, `<F1>`..`<F12>`), modifiers as prefixes
//! (`C-` control, `A-` or `M-` alt, `S-` shift, `D-` super: `<C-s>`, `<S-Left>`), and
//! `<lt>` for a literal `<`. What a key does comes from the bindings the session was
//! given; a key nothing binds is ignored, and bad notation gets an `error`.
//!
//! `paste` inserts text as one edit, never read as keys. `mouse` positions are
//! document lines and char columns: the client maps screen cells to them (tabs, wide
//! chars, scrolling); `press` puts the cursor there, `scroll` moves the view by
//! `lines`.
//!
//! # Sizes
//!
//! A cells client's surface shows `hello.size.rows` lines (at most 500) until it
//! sends `layout`, which sets each surface's rows from then on (later `resize`
//! messages no longer change them; send `layout` again). A pixels client gets no
//! lines until it sends `layout`. Columns are not used to cut lines: each line is
//! sent up to 4,000 chars with `truncated` set beyond that, and the client clips
//! and scrolls horizontally. The surface carries the status (path, mode, modified);
//! the client decides where to draw it.
//!
//! # Commands and events
//!
//! `command` runs any registered command by id (for example `editor.open {path}`,
//! `buffer.save`, `editor.undo`, `view.move_to {line, col}`). Listing commands over
//! the wire is a later addition. Error codes: `unknown_command`, `bad_args`,
//! `failed`, `vetoed`, `permission_denied`, `too_deep`, and from the server
//! `internal` (it caught a panic) and `too_large` (a result over the frame limit).
//!
//! `subscribe` replaces the client's filters: exact event names or `family.*`
//! (`buffer.*`, `selection.changed`, `mode.changed`, `view.closed`,
//! `client.attached`, `client.detached`); an empty list unsubscribes, and a bad
//! filter gets an `error` and unsubscribes. Event `seq` is
//! the session-wide sequence, so a filtered client sees gaps; only `lagged` means
//! events it asked for were dropped.
//!
//! # Compatibility
//!
//! Receivers ignore unknown fields, and every enum has an `Unknown` catch-all: an
//! unknown message is ignored, an unknown node kind is laid out as a column of its
//! children, an unknown size as `fill`, an unknown cursor kind as a peer cursor, an
//! unknown layout as cells, an unknown command outcome as `Reply::Unknown`.
//! Adding fields, messages or variants needs no new version. Over a stream, the
//! server reads only `protocols` from the first message before checking the version,
//! so a newer hello of another shape still gets a readable `bye`.
//!
//! # Wire format
//!
//! Over a byte stream each message is a 4-byte little-endian length and a JSON body
//! of at most [`MAX_FRAME`] bytes. A body that is not JSON, or a known message type
//! that does not decode (a missing field), closes the connection without `bye`.

mod protocol;
mod transport;

pub use protocol::{
    BadRows, Caps, ClientKind, ClientMsg, Cursor, CursorKind, Dim, Layout, Line, MouseAction, Node,
    NodeKind, PROTOCOL_VERSION, Pos, Reply, Rows, Sel, ServerMsg, Size, Span, SurfaceSize,
    SurfaceView, WireError, peek_hello,
};
pub use transport::{
    ClientEnd, Closed, Endpoint, FrameReader, FrameWriter, MAX_FRAME, QUEUE, ServerEnd, in_process,
    pump_writes, read_frame, write_frame,
};

/// Name of this crate, used in diagnostics.
pub const NAME: &str = env!("CARGO_PKG_NAME");

#[cfg(test)]
mod tests {
    #[test]
    fn name_matches_package() {
        assert_eq!(super::NAME, "spur-rpc");
    }
}
