//! The protocol between the editor server and its clients, plus transports.
//!
//! The server owns all editor state. A client (terminal, desktop, web, script) sends
//! input and draws what the server sends: a UI tree ([`Ui`]), theme tokens
//! ([`Style`] by name) and editor surface frames ([`SurfaceFrame`]). Frontends depend on
//! this crate only, never on the core.
//!
//! Protocol v0 is JSON in length-prefixed frames. In-process clients use the same
//! message enums over channels without serializing.

mod protocol;
mod transport;

pub use protocol::{
    Align, Anchor, Caps, ClientKind, ClientMsg, Color, Cursor, CursorKind, Dim, Float, ListRow,
    Node, NodeKind, PROTOCOL_VERSION, Pad, Pos, RowKind, Selected, ServerMsg, Size, Span, Style,
    SurfaceFrame, Ui,
};
pub use transport::{
    ClientEnd, Endpoint, MAX_FRAME, ServerEnd, framed, in_process, read_frame, write_frame,
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
