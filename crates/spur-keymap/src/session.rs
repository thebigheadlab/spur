//! An in-process editing session: the core, the editor state and one client's keymap.
//! Presets can watch each dispatch through an [`Observer`] (to record changes, for
//! example). Frontends feed keys and draw [`spur_core::editor::Viewport`]s. With the
//! daemon (#22) this moves server-side unchanged.

use std::sync::Arc;
use std::time::Instant;

use spur_core::editor::{
    Call, EDIT_MODE, Editor, Message, ModeSpec, ask, register_editor_commands,
};
use spur_core::{Actor, CommandId, Core, Invocation, Source, Target};

use crate::key::{Key, KeyCode, Mods};
use crate::keymap::{Dispatch, Keymap, Layer, LayerError, Status};

/// Spur's default keys.
pub const DEFAULT_LAYER: &str = include_str!("default.toml");
/// What one dispatch did, for observers.
pub struct Ran<'a> {
    /// The dispatch.
    pub dispatch: &'a Dispatch,
    /// Its steps as calls (with count and register).
    pub calls: Vec<Call>,
    /// The focused document's text changed.
    pub changed: bool,
    /// The mode before grouped edits into one undo step.
    pub groups_before: bool,
    /// The mode after does.
    pub groups_after: bool,
}

/// Watches dispatches. Presets use it for repeat and macro recording.
pub trait Observer: Send {
    /// Before a dispatch runs.
    fn before(&mut self, _core: &Core<Editor>, _dispatch: &Dispatch) {}
    /// After it ran.
    fn after(&mut self, core: &mut Core<Editor>, ran: &Ran<'_>);
}

/// Core + editor + keymap for one user.
pub struct Session {
    /// The core with the editor state.
    pub core: Core<Editor>,
    /// The keymap and dispatcher state.
    pub keymap: Keymap,
    /// Who the keys belong to.
    pub actor: Actor,
    observers: Vec<Box<dyn Observer>>,
    /// Read an unbound alt+char as `esc` then the char. Frontends turn this off when the
    /// terminal reports real alt keys (kitty keyboard protocol).
    pub split_alt: bool,
    /// Counts keys and pastes, so frontends can tell a repeated message from an old one.
    pub seq: u64,
}

impl Session {
    /// A session with Spur's default keys.
    ///
    /// # Panics
    ///
    /// Only if the built-in layer is invalid, which tests rule out.
    pub fn new() -> Self {
        let mut core = Core::new(Editor::new());
        register_editor_commands(&mut core).unwrap_or_else(|e| unreachable!("{e}"));
        let mut s = Self {
            core,
            keymap: Keymap::new(),
            actor: Actor::User,
            observers: Vec::new(),
            split_alt: true,
            seq: 0,
        };
        s.load_layer(
            Layer::from_toml("default", Source::Core, DEFAULT_LAYER)
                .unwrap_or_else(|e| unreachable!("{e}")),
        );
        s.set_default_mode(EDIT_MODE);
        s
    }

    /// Rebuilds the Ask card's command list (titles and key hints) from the registry
    /// and the keymap. Called after layers change.
    pub fn refresh_entries(&mut self) {
        let mode = self.core.state.default_mode.clone();
        let keymap = &self.keymap;
        let entries = ask::index(self.core.commands(), |id| {
            let id = spur_core::CommandId::new(id).ok()?;
            keymap
                .bindings_of(&id)
                .into_iter()
                .find(|b| b.0 == mode)
                .map(|b| b.1)
        });
        self.core.state.entries = entries;
    }

    /// Adds a dispatch observer.
    pub fn add_observer(&mut self, o: Box<dyn Observer>) {
        self.observers.push(o);
    }

    /// Loads a keymap layer from TOML on top.
    ///
    /// # Errors
    ///
    /// Returns [`LayerError`] if the layer does not parse.
    pub fn load_toml(&mut self, name: &str, source: Source, toml: &str) -> Result<(), LayerError> {
        self.load_layer(Layer::from_toml(name, source, toml)?);
        Ok(())
    }

    /// Adds a layer and registers the editor modes it describes.
    pub fn load_layer(&mut self, layer: Layer) {
        self.keymap.push_layer(layer);
        let ed = &mut self.core.state;
        for (name, o) in self.keymap.modes() {
            let spec = ed.modes.entry(Arc::from(name)).or_insert_with(|| ModeSpec {
                name: name.into(),
                groups_edits: false,
                block_cursor: false,
                inclusive_selection: false,
                linewise_selection: false,
            });
            if let Some(v) = o.groups_edits {
                spec.groups_edits = v;
            }
            if let Some(v) = o.block_cursor {
                spec.block_cursor = v;
            }
            if let Some(v) = o.inclusive_selection {
                spec.inclusive_selection = v;
            }
            if let Some(v) = o.linewise_selection {
                spec.linewise_selection = v;
            }
        }
    }

    /// Sets the mode for new views and the focused view.
    pub fn set_default_mode(&mut self, mode: &str) {
        self.set_default_mode_only(mode);
        self.refresh_entries();
    }

    fn set_default_mode_only(&mut self, mode: &str) {
        let ed = &mut self.core.state;
        ed.default_mode = mode.into();
        let focus = ed.focus;
        if let Some(v) = ed.views.get_mut(&focus) {
            v.mode = mode.into();
        }
    }

    /// The focused view's mode.
    pub fn mode(&self) -> Arc<str> {
        self.core.state.view().mode.clone()
    }

    /// Cursor shape hint for the current mode (`block`, `bar`, `underline`).
    pub fn cursor_shape(&self) -> &str {
        let mode = self.mode();
        self.keymap
            .mode_options(&mode)
            .and_then(|o| o.cursor.as_deref())
            .unwrap_or("bar")
    }

    /// Feeds a key and runs what it dispatches.
    pub fn key(&mut self, key: Key, now: Instant) -> Status {
        let mode = self.mode();
        // Without the kitty keyboard protocol, `esc` then a quick key arrives as alt+key.
        // When alt+key means nothing here, treat it as the two keys. Not on the Ask
        // card's command list, where it would close the card and type into the text;
        // on Find it closes find and types over the match, as esc then the key would.
        let on_list = self
            .core
            .state
            .ask
            .as_ref()
            .is_some_and(|a| a.kind != spur_core::editor::AskKind::Find);
        if self.split_alt
            && key.mods == Mods::ALT
            && !on_list
            && let KeyCode::Char(c) = key.code
            && !self.keymap.binds(&mode, key)
        {
            self.key(Key::named(KeyCode::Esc), now);
            return self.key(Key::char(c), now);
        }
        self.core.state.message = None;
        self.seq += 1;
        let feed = self.keymap.feed(key, &mode, self.core.context(), now);
        for d in &feed.run {
            self.run(d);
        }
        feed.status
    }

    /// Fires a timed-out ambiguous binding, if due.
    pub fn tick(&mut self, now: Instant) {
        if let Some(d) = self.keymap.tick(now) {
            self.run(&d);
        }
    }

    fn calls(&self, d: &Dispatch) -> Vec<Call> {
        d.invocations(&self.actor)
            .into_iter()
            .filter_map(|inv| match inv.command {
                Target::Id(command) => Some(Call {
                    command,
                    args: inv.args,
                    count: inv.count,
                    register: inv.register,
                }),
                Target::Handle(_) => None,
            })
            .collect()
    }

    /// Runs a dispatch: its steps in order, stopping at the first error (shown as a
    /// message), then tells observers what happened.
    pub fn run(&mut self, d: &Dispatch) {
        for o in &mut self.observers {
            o.before(&self.core, d);
        }
        let ed = &self.core.state;
        let doc = ed.view().doc;
        let version = ed.doc().buffer.version();
        let groups_before = ed.mode().groups_edits;

        for inv in d.invocations(&self.actor) {
            if let Err(e) = self.core.execute(inv) {
                self.core.state.message = Some(Message {
                    text: e.message,
                    error: true,
                });
                break;
            }
        }
        self.core.state.clamp_cursors();

        if self.observers.is_empty() {
            return;
        }
        let ed = &self.core.state;
        let ran = Ran {
            dispatch: d,
            calls: self.calls(d),
            changed: ed.view().doc == doc && ed.doc().buffer.version() != version,
            groups_before,
            groups_after: ed.mode().groups_edits,
        };
        for o in &mut self.observers {
            o.after(&mut self.core, &ran);
        }
    }

    /// Runs one command outside the keymap (file open at startup, bracketed paste). An
    /// error is also shown in the statusline.
    ///
    /// # Errors
    ///
    /// Returns the error message if the id is invalid or the command fails.
    pub fn command(
        &mut self,
        id: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = CommandId::try_from(id).map_err(|e| e.to_string())?;
        let res = self
            .core
            .execute(Invocation::new(id, self.actor.clone()).args(args));
        self.core.state.clamp_cursors();
        res.map_err(|e| {
            self.core.state.message = Some(Message {
                text: e.message.clone(),
                error: true,
            });
            e.message
        })
    }

    /// Pastes text the way the current mode takes typed text (its fallback command),
    /// as one dispatch so observers record it. Prompts get the first line only.
    pub fn paste(&mut self, text: &str) {
        let mode = self.mode();
        let insert = CommandId::new("editor.insert_text").unwrap_or_else(|e| unreachable!("{e}"));
        let command = self
            .keymap
            .mode_options(&mode)
            .and_then(|o| o.fallback.clone())
            .unwrap_or_else(|| insert.clone());
        let text = if command == insert {
            text.to_owned()
        } else {
            text.lines().next().unwrap_or_default().to_owned()
        };
        let d = Dispatch {
            steps: Arc::from([crate::keymap::Step {
                command,
                args: Arc::new(serde_json::json!({ "text": text })),
            }]),
            count: None,
            register: None,
            chars: Vec::new(),
            keys: Vec::new(),
        };
        self.core.state.message = None;
        self.seq += 1;
        self.run(&d);
    }

    /// True once the user asked to quit.
    pub fn should_quit(&self) -> bool {
        self.core.state.quit
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}
