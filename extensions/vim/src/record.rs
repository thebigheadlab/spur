//! Records changes for `.` and keys for macros, by watching dispatches.

use spur_core::Core;
use spur_core::editor::{Call, Editor};
use spur_keymap::{Dispatch, Observer, Ran};

use crate::state::{pending, vim};

/// Commands that `.` and macro recording leave out.
const NOT_RECORDED: &[&str] = &[
    "vim.repeat",
    "editor.undo",
    "editor.redo",
    "vim.macro_record",
    "vim.macro_play",
    "vim.cmdline_open",
    "vim.cmdline_insert",
    "vim.cmdline_backspace",
    "vim.cmdline_cancel",
    "vim.cmdline_submit",
];

/// The recorder.
#[derive(Default)]
pub struct Recorder {
    /// A change being recorded for `.`, and whether it changed text yet.
    recording: Option<(Vec<Call>, bool)>,
    /// A macro was being recorded when the dispatch started.
    macro_before: bool,
}

impl Observer for Recorder {
    fn before(&mut self, core: &Core<Editor>, _d: &Dispatch) {
        self.macro_before = core
            .state
            .ext
            .get::<crate::VimState>()
            .is_some_and(|v| v.macro_recording.is_some());
    }

    fn after(&mut self, core: &mut Core<Editor>, ran: &Ran<'_>) {
        let ed = &mut core.state;
        let skip = ran
            .dispatch
            .steps
            .first()
            .is_some_and(|s| NOT_RECORDED.contains(&s.command.as_str()));
        if !skip {
            let op_pending = pending(ed).is_some();
            if self.recording.is_none()
                && !ran.groups_before
                && (ran.changed || op_pending || ran.groups_after)
            {
                self.recording = Some((Vec::new(), false));
            }
            if let Some((rec, rec_changed)) = &mut self.recording {
                rec.extend(ran.calls.iter().cloned());
                *rec_changed |= ran.changed;
                if !op_pending
                    && !ran.groups_after
                    && let Some((rec, true)) = self.recording.take()
                {
                    vim(ed).last_change = rec;
                }
            }
        }
        if self.macro_before
            && let Some((_, m)) = &mut vim(ed).macro_recording
        {
            m.extend(ran.calls.iter().cloned());
        }
    }
}
