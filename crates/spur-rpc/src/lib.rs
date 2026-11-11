//! Spur's client protocol, version 0, and its transports.
//!
//! The editor server owns all state and never draws. A client says `hello`, then
//! draws what the server sends (a UI tree and surface frames) and sends input back.
//! This crate depends only on serde, so a client links the protocol and nothing else.
//!
//! # Conversation
//!
//! 1. Client: `hello` listing the versions it speaks. Server: `welcome` (or `bye`
//!    with a reason), then `ui`, one `surface` per surface, `flush {frame: 1}`.
//! 2. Client: input (`key`, `paste`, `mouse`, `resize`, `layout`), `command`s and
//!    `subscribe`. Server, per change: `event`s, then `ui` if it changed, changed
//!    `surface`s, then `flush`. A `result` answers each `command`.
//! 3. The client acks each flush it drew (`ack` is cumulative). The server keeps at
//!    most two unacked frames in flight; a slow client gets the latest state, not
//!    every intermediate one. A client must ack, or it stops getting frames.
//! 4. `ping` → `pong {frame}` once every earlier message is handled and the frames
//!    showing it are sent. A key still waiting for a longer binding (a pending
//!    prefix) is handled when it resolves, which may be after the pong.
//! 5. Tool clients (scripts) get no `ui`, `surface` or `flush`.
//!
//! # Compatibility
//!
//! Receivers ignore unknown fields, and every enum has an `Unknown` catch-all: an
//! unknown message is ignored, an unknown node kind is laid out as a column of its
//! children, an unknown size as `fill`, an unknown cursor kind as a peer cursor.
//! Adding fields, messages or variants needs no new version.
//!
//! # Framing
//!
//! Over a byte stream each message is a 4-byte little-endian length and a JSON body
//! of at most [`MAX_FRAME`] bytes. A bad frame closes the connection.

mod protocol;
mod transport;

pub use protocol::{
    Caps, ClientKind, ClientMsg, Cursor, CursorKind, Dim, Layout, Line, MouseAction, Node,
    NodeKind, PROTOCOL_VERSION, Pos, Reply, Sel, ServerMsg, Size, Span, SurfaceSize, SurfaceView,
    WireError, peek_hello,
};
pub use transport::{
    ClientEnd, Closed, Endpoint, FrameReader, FrameWriter, MAX_FRAME, ServerEnd, in_process,
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
