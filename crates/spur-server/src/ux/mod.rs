//! UX packs: the look of a session, loaded from TOML, and the per-client UI model
//! built from it.

pub mod commands;
pub mod eval;
pub mod files;
pub mod pack;
pub mod state;

use files::Files;

/// The region of the code surface; focus returns here.
pub const EDITOR_REGION: &str = "editor";
/// The list source of the Files tree.
pub const FILES_SOURCE: &str = "workspace.files";
use pack::{Pack, Theme};

/// The session's UX: the loaded pack and what every client shares.
#[derive(Debug)]
pub struct Ux {
    /// The pack.
    pub pack: Pack,
    /// The session's theme override.
    pub theme: Option<String>,
    /// The Files listing cache.
    pub files: Files,
    /// The workspace folder's name.
    pub workspace: String,
    /// The first error of the last failed reload.
    pub pack_error: Option<String>,
    /// Rises with each pack loaded, so clients get the theme again.
    pub generation: u64,
}

impl Ux {
    /// The theme in use, and its name.
    pub fn theme(&self) -> Option<(&str, &Theme)> {
        self.pack.theme_named(self.theme.as_deref())
    }
}
