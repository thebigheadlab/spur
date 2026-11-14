//! The editor server. One session thread owns the editor state (documents, views,
//! commands, keymaps) and serves any number of clients over [`spur_rpc`]. Each client
//! has its own view, size, key state and context keys; documents are shared.
//!
//! The server never draws and holds no look: no colours, glyphs or key bindings. Key
//! bindings arrive as keymap layers in [`SessionConfig`] (from UX packs, later).
//!
//! Design: `docs/adr/0006-editor-server-and-protocol-v0.md`.

mod client;
mod frame;
mod input;
mod lang;
mod session;
mod ux;

pub use session::{Ended, Session, SessionConfig, SessionError};
pub use ux::pack::{Pack, PackError, PackSource};

/// Name of this crate, used in diagnostics.
pub const NAME: &str = env!("CARGO_PKG_NAME");

#[cfg(test)]
mod tests {
    #[test]
    fn name_matches_package() {
        assert_eq!(super::NAME, "spur-server");
    }
}
