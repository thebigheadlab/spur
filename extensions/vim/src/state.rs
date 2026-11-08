//! The preset's own state, kept on the editor as extension state.

use std::collections::HashMap;
use std::sync::Arc;

use spur_core::editor::{Call, Editor, ViewId};

/// An operator waiting for its range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingOp {
    /// `delete`, `change`, `yank`, `indent`, `outdent`, `lower`, `upper`, `toggle_case`.
    pub op: Arc<str>,
    /// Count typed before the operator; multiplies the motion's count.
    pub count: Option<u32>,
    /// Register typed before the operator.
    pub register: Option<char>,
    /// Mode to return to after the operator.
    pub return_mode: Arc<str>,
}

/// A command line being typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cmdline {
    /// `:` for commands, `/` or `?` for search.
    pub prefix: char,
    /// Text typed so far.
    pub text: String,
    /// Mode to return to.
    pub return_mode: Arc<str>,
}

/// Everything the preset remembers.
#[derive(Debug, Default)]
pub struct VimState {
    /// Operators waiting for a motion, per view.
    pub ops: HashMap<ViewId, PendingOp>,
    /// The last change, for `.`.
    pub last_change: Vec<Call>,
    /// Macros by register.
    pub macros: HashMap<char, Vec<Call>>,
    /// A macro being recorded.
    pub macro_recording: Option<(char, Vec<Call>)>,
    /// Last `f`/`t`/`F`/`T`: (char, till, backward).
    pub last_find: Option<(char, bool, bool)>,
    /// Last search pattern and direction.
    pub last_search: Option<(String, bool)>,
    /// The command line being edited.
    pub cmdline: Option<Cmdline>,
}

/// The preset's state.
pub fn vim(ed: &mut Editor) -> &mut VimState {
    ed.ext.get_mut::<VimState>()
}

/// The focused view's pending operator.
pub fn pending(ed: &Editor) -> Option<PendingOp> {
    ed.ext.get::<VimState>()?.ops.get(&ed.focus).cloned()
}

/// Sets or clears the focused view's pending operator.
pub fn set_pending(ed: &mut Editor, op: Option<PendingOp>) {
    let focus = ed.focus;
    let st = vim(ed);
    match op {
        Some(op) => {
            st.ops.insert(focus, op);
        }
        None => {
            st.ops.remove(&focus);
        }
    }
}
