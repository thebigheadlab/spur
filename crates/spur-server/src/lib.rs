//! The editor server. It owns documents, views, commands and the UI model, and serves
//! any number of clients over [`spur_rpc`]. It never draws: what the editor looks like
//! comes from an experience [`Pack`] (data), and how it is drawn is up to each frontend.
//!
//! This crate must not depend on a terminal, window or web library.

mod ask;
pub mod daemon;
mod files;
mod fuzzy;
mod pack;
mod session;
mod syntax;
mod ui;

pub use pack::{Pack, PackError};
pub use session::{Attacher, Session, SessionConfig};

/// Name of this crate, used in diagnostics.
pub const NAME: &str = env!("CARGO_PKG_NAME");
