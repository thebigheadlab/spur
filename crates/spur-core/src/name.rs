//! Dotted names shared by commands, events and context keys.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Longest allowed name, in bytes.
pub const MAX_NAME_LEN: usize = 64;

/// Why a name was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameError {
    /// The rejected name.
    pub name: String,
    /// What is wrong with it.
    pub reason: &'static str,
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid name {:?}: {}", self.name, self.reason)
    }
}

impl std::error::Error for NameError {}

/// Checks one segment: `[a-z][a-z0-9_]*` without `__`.
pub fn check_segment(seg: &str) -> Result<(), &'static str> {
    let mut chars = seg.chars();
    match chars.next() {
        None => return Err("empty segment"),
        Some(c) if c.is_ascii_lowercase() => {}
        Some(_) => return Err("segment must start with a-z"),
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        return Err("segment may contain only a-z, 0-9 and _");
    }
    // `__` is the MCP separator, so a trailing `_` would not round-trip.
    if seg.contains("__") {
        return Err("segment may not contain __");
    }
    if seg.ends_with('_') {
        return Err("segment may not end with _");
    }
    Ok(())
}

/// Checks a dotted name with at least `min_segments` segments.
pub fn check_name(name: &str, min_segments: usize) -> Result<(), NameError> {
    let err = |reason| NameError {
        name: name.to_owned(),
        reason,
    };
    if name.len() > MAX_NAME_LEN {
        return Err(err("longer than 64 bytes"));
    }
    let mut n = 0;
    for seg in name.split('.') {
        check_segment(seg).map_err(err)?;
        n += 1;
    }
    if n < min_segments {
        return Err(err("needs a namespace: use namespace.name"));
    }
    Ok(())
}

macro_rules! dotted_name {
    ($(#[$doc:meta])* $ty:ident, $min:expr) => {
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $ty(Arc<str>);

        impl $ty {
            /// Validates and wraps `name`.
            ///
            /// # Errors
            ///
            /// Returns [`NameError`] if `name` breaks the dotted name grammar.
            pub fn new(name: &str) -> Result<Self, NameError> {
                check_name(name, $min)?;
                Ok(Self(name.into()))
            }

            /// The name as text.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// The first segment.
            pub fn namespace(&self) -> &str {
                self.0.split('.').next().unwrap_or_default()
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({:?})", stringify!($ty), &*self.0)
            }
        }

        impl TryFrom<String> for $ty {
            type Error = NameError;
            fn try_from(s: String) -> Result<Self, NameError> {
                Self::new(&s)
            }
        }

        impl TryFrom<&str> for $ty {
            type Error = NameError;
            fn try_from(s: &str) -> Result<Self, NameError> {
                Self::new(s)
            }
        }

        impl From<$ty> for String {
            fn from(n: $ty) -> String {
                n.0.to_string()
            }
        }
    };
}

dotted_name!(
    /// A command id, `namespace.verb_noun`. The namespace is the owning extension id or
    /// a core namespace.
    CommandId,
    2
);

dotted_name!(
    /// An event name, e.g. `buffer.changed` or `agent.turn.start`.
    EventName,
    2
);

impl CommandId {
    /// Name for MCP tool export: `.` becomes `__`. Reversible since segments never
    /// contain `__`.
    pub fn to_mcp_name(&self) -> String {
        self.0.replace('.', "__")
    }

    /// Inverse of [`CommandId::to_mcp_name`].
    ///
    /// # Errors
    ///
    /// Returns [`NameError`] if the mapped name is invalid.
    pub fn from_mcp_name(name: &str) -> Result<Self, NameError> {
        Self::new(&name.replace("__", "."))
    }
}

/// Core command and event namespaces. Extensions may not use them. `ui` belongs to
/// the editor server's per-client UI commands (UX packs).
pub const CORE_NAMESPACES: &[&str] = &["core", "editor", "view", "buffer", "bus", "command", "ui"];

/// Event families only core (or, later, a capability holder) may publish.
pub const RESERVED_EVENT_FAMILIES: &[&str] = &[
    "buffer",
    "selection",
    "mode",
    "view",
    "client",
    "extension",
    "command",
    "bus",
    "agent",
    "changeset",
    "checkpoint",
];

/// Reserved event names. Families in [`RESERVED_EVENT_FAMILIES`] cover the
/// `agent.session.*`, `agent.tool_call.*` and `changeset.*` sub-families.
#[allow(missing_docs)]
pub mod events {
    pub const BUFFER_CHANGED: &str = "buffer.changed";
    pub const BUFFER_SAVED: &str = "buffer.saved";
    pub const SELECTION_CHANGED: &str = "selection.changed";
    pub const MODE_CHANGED: &str = "mode.changed";
    pub const VIEW_OPENED: &str = "view.opened";
    pub const VIEW_CLOSED: &str = "view.closed";
    pub const CLIENT_ATTACHED: &str = "client.attached";
    pub const CLIENT_DETACHED: &str = "client.detached";
    pub const EXTENSION_ACTIVATED: &str = "extension.activated";
    pub const EXTENSION_FAILED: &str = "extension.failed";
    pub const AGENT_TURN_START: &str = "agent.turn.start";
    pub const AGENT_TURN_END: &str = "agent.turn.end";
    pub const AGENT_STATE_CHANGED: &str = "agent.state.changed";
    pub const AGENT_PERMISSION_REQUESTED: &str = "agent.permission.requested";
    pub const AGENT_PERMISSION_RESOLVED: &str = "agent.permission.resolved";
    pub const AGENT_EDIT_APPLIED: &str = "agent.edit.applied";
    pub const CHECKPOINT_CREATED: &str = "checkpoint.created";
    pub const CHECKPOINT_RESTORED: &str = "checkpoint.restored";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar() {
        for ok in [
            "editor.move_down",
            "git.stage_hunk",
            "agent.turn.start",
            "a.b2",
        ] {
            assert!(CommandId::new(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "editor",
            "Editor.move",
            "editor.Move",
            "editor..move",
            "editor.move-down",
            "editor.move__down",
            "1x.move",
            "editor.",
            ".editor",
            &format!("a.{}", "b".repeat(63)),
        ] {
            assert!(CommandId::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn mcp_round_trip() {
        let id = CommandId::new("git.stage_hunk").unwrap();
        assert_eq!(id.to_mcp_name(), "git__stage_hunk");
        assert_eq!(CommandId::from_mcp_name("git__stage_hunk").unwrap(), id);
        assert_eq!(id.namespace(), "git");
        // A trailing `_` would map `x_.y` to `x___y`, which reads back as `x._y`.
        assert!(CommandId::new("x_.y").is_err());
        assert!(CommandId::from_mcp_name("x___y").is_err());
    }

    #[test]
    fn serde_validates() {
        let id: CommandId = serde_json::from_str("\"editor.save\"").unwrap();
        assert_eq!(id.as_str(), "editor.save");
        assert!(serde_json::from_str::<CommandId>("\"nope\"").is_err());
    }
}
