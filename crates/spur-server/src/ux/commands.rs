//! The `ui.*` commands: per-client UI state changes, run by the server itself (a
//! core command handler cannot reach a client's UI state).

/// Every `ui.*` command id.
pub const UI_COMMANDS: &[&str] = &[
    "ui.set",
    "ui.toggle",
    "ui.cycle",
    "ui.focus",
    "ui.list.move",
    "ui.list.page",
    "ui.list.activate",
    "ui.tree.expand",
    "ui.tree.collapse",
    "ui.quit",
];
