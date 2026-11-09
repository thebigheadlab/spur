//! Layered keymaps compiled into one trie per mode, with `when` filters, timeouts,
//! counts, registers, sticky groups and generated which-key menus.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;
use spur_core::{Actor, CommandId, ContextLookup, Invocation, Source, When};

use crate::key::{Key, KeyCode, KeyParseError, format_keys, parse_keys};

/// Default wait for a longer binding when a shorter one also matches (`g` vs `gg`).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(1);

/// One command call inside a binding.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// The command.
    pub command: CommandId,
    /// Static args.
    pub args: Arc<Value>,
}

/// What a binding does.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Run commands in order. The count and register go to the first step.
    Run(Arc<[Step]>),
    /// Hide lower-layer bindings of these keys (`"-cmd"` removal).
    Unbind,
}

/// A key binding.
#[derive(Clone, Debug)]
pub struct Binding {
    /// Mode it applies in.
    pub mode: Arc<str>,
    /// Key sequence.
    pub keys: Vec<Key>,
    /// What it does.
    pub action: Action,
    /// Condition.
    pub when: Option<When>,
    /// Label for which-key.
    pub label: Option<Arc<str>>,
}

/// A labelled prefix (`<Space>` = "+leader"), optionally sticky: after a binding under
/// it fires, the prefix stays active until `<Esc>`.
#[derive(Clone, Debug)]
pub struct Group {
    /// Mode.
    pub mode: Arc<str>,
    /// Prefix.
    pub keys: Vec<Key>,
    /// Label.
    pub label: Arc<str>,
    /// Stays active after a binding fires.
    pub sticky: bool,
}

/// Per-mode dispatch options. A field a layer leaves out keeps the value from layers
/// below; a field it sets (also to `false`) wins.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct ModeOptions {
    /// Digits before a sequence form a count (`5dw`); `0` only continues one.
    #[serde(default)]
    pub counts: Option<bool>,
    /// `"x` before a sequence selects register `x`.
    #[serde(default)]
    pub registers: Option<bool>,
    /// Command run with `{"text": c}` for an unbound printable key (self-insert).
    #[serde(default)]
    pub fallback: Option<CommandId>,
}

/// Mode options after merging the layers.
#[derive(Clone, Debug, Default)]
struct Opts {
    counts: bool,
    registers: bool,
    fallback: Option<CommandId>,
}

/// A layer: bindings from one source (core default, preset, user config, extension).
/// Later layers win.
#[derive(Clone, Debug)]
pub struct Layer {
    /// Name shown by describe-key.
    pub name: Arc<str>,
    /// Where it comes from.
    pub source: Source,
    /// Bindings.
    pub bindings: Vec<Binding>,
    /// Groups.
    pub groups: Vec<Group>,
    /// Mode options set by this layer (merged, later layers win per field).
    pub modes: BTreeMap<Arc<str>, ModeOptions>,
}

/// Serialized form of a layer (presets and config ship as TOML).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerSpec {
    /// Bindings.
    #[serde(default)]
    pub bind: Vec<BindingSpec>,
    /// Groups.
    #[serde(default)]
    pub group: Vec<GroupSpec>,
    /// Mode options by mode name.
    #[serde(default)]
    pub mode: BTreeMap<String, ModeOptions>,
}

/// Serialized binding. Give `command` (+`args`), `run`, or `unbind = true`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingSpec {
    /// Mode, or several separated by spaces (`"normal visual"`).
    pub mode: String,
    /// Key notation.
    pub keys: String,
    /// Command id.
    #[serde(default)]
    pub command: Option<String>,
    /// Args for `command`.
    #[serde(default)]
    pub args: Option<Value>,
    /// Several steps.
    #[serde(default)]
    pub run: Vec<StepSpec>,
    /// Remove lower-layer bindings of these keys.
    #[serde(default)]
    pub unbind: bool,
    /// Condition.
    #[serde(default)]
    pub when: Option<String>,
    /// which-key label.
    #[serde(default)]
    pub label: Option<String>,
}

/// Serialized step.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepSpec {
    /// Command id.
    pub command: String,
    /// Args.
    #[serde(default)]
    pub args: Option<Value>,
}

/// Serialized group.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupSpec {
    /// Mode(s), space separated.
    pub mode: String,
    /// Prefix notation.
    pub keys: String,
    /// Label.
    pub label: String,
    /// Sticky.
    #[serde(default)]
    pub sticky: bool,
}

/// Why a layer did not load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerError {
    /// Which entry (e.g. `bind[3]`).
    pub at: String,
    /// What went wrong.
    pub message: String,
}

impl fmt::Display for LayerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.at, self.message)
    }
}

impl std::error::Error for LayerError {}

impl Layer {
    /// An empty layer.
    pub fn new(name: &str, source: Source) -> Self {
        Self {
            name: name.into(),
            source,
            bindings: Vec::new(),
            groups: Vec::new(),
            modes: BTreeMap::new(),
        }
    }

    /// Builds a layer from TOML.
    ///
    /// # Errors
    ///
    /// Returns [`LayerError`] for TOML errors, bad key notation, ids or `when` clauses.
    pub fn from_toml(name: &str, source: Source, toml: &str) -> Result<Self, LayerError> {
        let spec: LayerSpec = toml::from_str(toml).map_err(|e| LayerError {
            at: name.to_owned(),
            message: e.to_string(),
        })?;
        Self::from_spec(name, source, spec)
    }

    /// Builds a layer from its serialized form.
    ///
    /// # Errors
    ///
    /// Returns [`LayerError`] for bad key notation, command ids or `when` clauses.
    pub fn from_spec(name: &str, source: Source, spec: LayerSpec) -> Result<Self, LayerError> {
        let mut layer = Self::new(name, source);
        for (i, b) in spec.bind.into_iter().enumerate() {
            let at = format!("bind[{i}] ({})", b.keys);
            let err = |m: String| LayerError {
                at: at.clone(),
                message: m,
            };
            let keys = parse_keys(&b.keys).map_err(|e: KeyParseError| err(e.to_string()))?;
            let step = |command: &str, args: Option<Value>| -> Result<Step, LayerError> {
                Ok(Step {
                    command: CommandId::new(command).map_err(|e| err(e.to_string()))?,
                    args: Arc::new(args.unwrap_or(Value::Null)),
                })
            };
            if b.args.is_some() && b.command.is_none() {
                return Err(err(
                    "args go with command (use args inside run steps)".into()
                ));
            }
            if b.mode.split_whitespace().next().is_none() {
                return Err(err("mode is empty".into()));
            }
            let action = match (b.command, b.run.is_empty(), b.unbind) {
                (Some(c), true, false) => Action::Run(Arc::from([step(&c, b.args)?])),
                (None, false, false) => Action::Run(
                    b.run
                        .into_iter()
                        .map(|s| step(&s.command, s.args))
                        .collect::<Result<Vec<_>, _>>()?
                        .into(),
                ),
                (None, true, true) => Action::Unbind,
                _ => return Err(err("give exactly one of command, run or unbind".into())),
            };
            let when = b
                .when
                .as_deref()
                .map(When::parse)
                .transpose()
                .map_err(|e| err(format!("when: {e}")))?;
            for mode in b.mode.split_whitespace() {
                layer.bindings.push(Binding {
                    mode: mode.into(),
                    keys: keys.clone(),
                    action: action.clone(),
                    when: when.clone(),
                    label: b.label.as_deref().map(Arc::from),
                });
            }
        }
        for (i, g) in spec.group.into_iter().enumerate() {
            let keys = parse_keys(&g.keys).map_err(|e| LayerError {
                at: format!("group[{i}]"),
                message: e.to_string(),
            })?;
            if g.mode.split_whitespace().next().is_none() {
                return Err(LayerError {
                    at: format!("group[{i}]"),
                    message: "mode is empty".into(),
                });
            }
            for mode in g.mode.split_whitespace() {
                layer.groups.push(Group {
                    mode: mode.into(),
                    keys: keys.clone(),
                    label: g.label.as_str().into(),
                    sticky: g.sticky,
                });
            }
        }
        layer.modes = spec
            .mode
            .into_iter()
            .map(|(k, v)| (Arc::from(k), v))
            .collect();
        Ok(layer)
    }
}

/// A binding as placed in the compiled trie.
#[derive(Debug)]
struct Candidate {
    layer: usize,
    binding: Binding,
}

#[derive(Debug, Default)]
struct Node {
    children: HashMap<Key, Node>,
    any_char: Option<Box<Node>>,
    /// Highest layer first; within a layer, later definitions first.
    candidates: Vec<Arc<Candidate>>,
    label: Option<Arc<str>>,
    sticky: bool,
}

impl Node {
    fn has_children(&self) -> bool {
        !self.children.is_empty() || self.any_char.is_some()
    }

    /// Removes dead subtrees; true if this node is still useful.
    fn prune(&mut self) -> bool {
        self.children.retain(|_, n| n.prune());
        if self.any_char.as_mut().is_some_and(|n| !n.prune()) {
            self.any_char = None;
        }
        self.has_children()
            || self.label.is_some()
            || self
                .candidates
                .iter()
                .any(|c| matches!(c.binding.action, Action::Run(_)))
    }

    fn child(&self, key: Key) -> Option<(&Node, Option<char>)> {
        if let Some(n) = self.children.get(&key) {
            return Some((n, None));
        }
        let c = key.text()?;
        self.any_char.as_deref().map(|n| (n, Some(c)))
    }

    fn child_mut(&mut self, key: Key) -> &mut Node {
        if key.code == KeyCode::AnyChar {
            self.any_char.get_or_insert_default()
        } else {
            self.children.entry(key).or_default()
        }
    }

    /// The first candidate whose `when` holds; `None` if none does or it is an unbind.
    fn select(&self, cx: &impl ContextLookup) -> Option<&Arc<Candidate>> {
        let c = self
            .candidates
            .iter()
            .find(|c| c.binding.when.as_ref().is_none_or(|w| w.eval(cx)))?;
        matches!(c.binding.action, Action::Run(_)).then_some(c)
    }
}

/// Commands to run for a completed key sequence.
#[derive(Clone, Debug, PartialEq)]
pub struct Dispatch {
    /// The steps.
    pub steps: Arc<[Step]>,
    /// Count prefix.
    pub count: Option<u32>,
    /// Register prefix.
    pub register: Option<char>,
    /// Chars captured by `<char>` keys, merged into each step's args as `char`.
    pub chars: Vec<char>,
    /// The keys that produced it.
    pub keys: Vec<Key>,
}

impl Dispatch {
    /// Invocations for the steps. The count and register go to the first step only.
    pub fn invocations(&self, actor: &Actor) -> Vec<Invocation> {
        self.steps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let args = if let Some(&c) = self.chars.last() {
                    let mut obj = match &*s.args {
                        Value::Object(m) => m.clone(),
                        _ => serde_json::Map::new(),
                    };
                    obj.insert("char".into(), Value::String(c.to_string()));
                    Arc::new(Value::Object(obj))
                } else {
                    s.args.clone()
                };
                let mut inv = Invocation::new(s.command.clone(), actor.clone()).args(args);
                if i == 0 {
                    inv.count = self.count;
                    inv.register = self.register;
                }
                inv
            })
            .collect()
    }
}

/// Result of feeding a key: what to run, then the dispatcher's state.
#[derive(Clone, Debug, PartialEq)]
pub struct Feed {
    /// Run these, in order (two when a key ends an ambiguous prefix and starts anew).
    pub run: Vec<Dispatch>,
    /// State after the key.
    pub status: Status,
}

/// Dispatcher state after a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Nothing pending.
    Idle,
    /// More keys needed; [`Keymap::menu`] lists them.
    Pending,
    /// Nothing bound; these keys were dropped.
    Unmatched(Vec<Key>),
    /// A pending sequence was cancelled with `<Esc>`.
    Cancelled,
}

#[derive(Debug, Default)]
struct Pending {
    keys: Vec<Key>,
    chars: Vec<char>,
    count: Option<u32>,
    register: Option<char>,
    awaiting_register: bool,
    /// A shorter binding waiting for the timeout (`g` while `gg` exists).
    timed: Option<(Instant, Arc<Candidate>)>,
    /// Prefix of an active sticky group.
    sticky: Option<Vec<Key>>,
    /// In a mode with a fallback: when to give up on the prefix and type its keys.
    flush: Option<(Instant, CommandId)>,
    /// Mode the pending keys belong to.
    mode: Option<Arc<str>>,
}

/// One row of a which-key menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuItem {
    /// The next key.
    pub key: String,
    /// Label (binding label, group label or command id).
    pub label: String,
    /// True if it leads to more keys.
    pub group: bool,
}

/// Result of [`Keymap::describe_key`].
#[derive(Clone, Debug, PartialEq)]
pub struct KeyDescription {
    /// What runs now, if anything.
    pub chosen: Option<Described>,
    /// Bindings for the same keys that did not apply.
    pub rejected: Vec<Described>,
    /// More keys can follow.
    pub is_prefix: bool,
}

/// One binding, described.
#[derive(Clone, Debug, PartialEq)]
pub struct Described {
    /// Layer name.
    pub layer: Arc<str>,
    /// Layer source.
    pub source: Source,
    /// The action.
    pub action: Action,
    /// The condition source.
    pub when: Option<String>,
    /// Why it did not apply (failing clause), or that a higher binding won.
    pub reason: Option<String>,
}

/// A keymap problem found by [`Keymap::conflicts`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// Kind.
    pub kind: ConflictKind,
    /// Mode.
    pub mode: Arc<str>,
    /// Keys, in notation.
    pub keys: String,
    /// Layers involved.
    pub layers: Vec<Arc<str>>,
}

/// Conflict categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConflictKind {
    /// Same keys, mode and `when` twice in one layer: only the later one can fire.
    Duplicate,
    /// A binding on a prefix of another: the longer one needs the timeout to be skipped.
    PrefixShadow,
    /// A higher layer masks a lower one (informational).
    LayerMask,
}

/// (layer index, `when` source) of one definition, for conflict checks.
type Def = (usize, Option<String>);

/// The keymap: layers plus the dispatcher state of one client.
pub struct Keymap {
    layers: Vec<Layer>,
    tries: HashMap<Arc<str>, Node>,
    options: HashMap<Arc<str>, Opts>,
    pending: Pending,
    /// Wait for a longer binding.
    pub timeout: Duration,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::new()
    }
}

impl Keymap {
    /// An empty keymap.
    pub fn new() -> Self {
        Self {
            layers: Vec::new(),
            tries: HashMap::new(),
            options: HashMap::new(),
            pending: Pending::default(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Adds a layer on top and recompiles.
    pub fn push_layer(&mut self, layer: Layer) {
        self.layers.push(layer);
        self.compile();
    }

    /// Removes the layer named `name`.
    pub fn remove_layer(&mut self, name: &str) -> bool {
        let before = self.layers.len();
        self.layers.retain(|l| &*l.name != name);
        let removed = self.layers.len() != before;
        if removed {
            self.compile();
        }
        removed
    }

    /// Layer names, bottom first.
    pub fn layers(&self) -> impl Iterator<Item = &str> {
        self.layers.iter().map(|l| &*l.name)
    }

    fn compile(&mut self) {
        self.tries.clear();
        self.options.clear();
        self.pending = Pending::default();
        for (li, layer) in self.layers.iter().enumerate() {
            for (mode, opts) in &layer.modes {
                let o = self.options.entry(mode.clone()).or_default();
                if let Some(v) = opts.counts {
                    o.counts = v;
                }
                if let Some(v) = opts.registers {
                    o.registers = v;
                }
                if opts.fallback.is_some() {
                    o.fallback.clone_from(&opts.fallback);
                }
            }
            for g in &layer.groups {
                let mut node = self.tries.entry(g.mode.clone()).or_default();
                for k in &g.keys {
                    node = node.child_mut(*k);
                }
                node.label = Some(g.label.clone());
                node.sticky = g.sticky;
            }
            for b in &layer.bindings {
                let mut node = self.tries.entry(b.mode.clone()).or_default();
                for k in &b.keys {
                    node = node.child_mut(*k);
                }
                if b.when.is_none() && b.action == Action::Unbind {
                    node.candidates.clear();
                }
                node.candidates.insert(
                    0,
                    Arc::new(Candidate {
                        layer: li,
                        binding: b.clone(),
                    }),
                );
            }
        }
        // Unbinds leave nodes nothing can fire; drop them so they neither delay a
        // shorter binding nor show up as conflicts.
        for root in self.tries.values_mut() {
            root.prune();
        }
    }

    fn node(&self, mode: &str, keys: &[Key]) -> Option<&Node> {
        let mut node = self.tries.get(mode)?;
        for k in keys {
            node = node.child(*k)?.0;
        }
        Some(node)
    }

    /// Keys typed so far (for the statusline), with count and register.
    pub fn pending_keys(&self) -> String {
        let p = &self.pending;
        let mut s = String::new();
        if let Some(r) = p.register {
            s.push('"');
            s.push(r);
        } else if p.awaiting_register {
            s.push('"');
        }
        if let Some(c) = p.count {
            s.push_str(&c.to_string());
        }
        s + &format_keys(&p.keys)
    }

    /// True while keys, a count or a register are pending.
    pub fn is_pending(&self) -> bool {
        let p = &self.pending;
        !p.keys.is_empty() || p.count.is_some() || p.register.is_some() || p.awaiting_register
    }

    /// When [`Keymap::tick`] should be called: a timed-out shorter binding fires, or an
    /// unfinished prefix in a typing mode is typed as text.
    pub fn deadline(&self) -> Option<Instant> {
        let p = &self.pending;
        match (p.timed.as_ref().map(|t| t.0), p.flush.as_ref().map(|f| f.0)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Fires a shorter binding whose timeout passed, or types an abandoned prefix.
    pub fn tick(&mut self, now: Instant) -> Option<Dispatch> {
        if let Some((at, _)) = &self.pending.timed {
            if now < *at {
                return None;
            }
            let (_, cand) = self.pending.timed.take()?;
            return Some(self.fire(&cand));
        }
        let (at, _) = self.pending.flush.as_ref()?;
        if now < *at {
            return None;
        }
        let (_, cmd) = self.pending.flush.take()?;
        let p = std::mem::take(&mut self.pending);
        Self::typed(&cmd, &p, &p.keys)
    }

    /// A fallback dispatch typing the printable keys in `keys`, if any.
    fn typed(cmd: &CommandId, p: &Pending, keys: &[Key]) -> Option<Dispatch> {
        let text: String = keys.iter().filter_map(|k| k.text()).collect();
        (!text.is_empty()).then(|| Dispatch {
            steps: Arc::from([Step {
                command: cmd.clone(),
                args: Arc::new(serde_json::json!({ "text": text })),
            }]),
            count: p.count,
            register: p.register,
            chars: Vec::new(),
            keys: keys.to_vec(),
        })
    }

    /// Drops any pending keys, count and register (keeps nothing sticky).
    pub fn reset(&mut self) {
        self.pending = Pending::default();
    }

    fn fire(&mut self, cand: &Candidate) -> Dispatch {
        let Action::Run(steps) = &cand.binding.action else {
            unreachable!("unbind is never selected")
        };
        let p = std::mem::take(&mut self.pending);
        let d = Dispatch {
            steps: steps.clone(),
            count: p.count,
            register: p.register,
            chars: p.chars,
            keys: p.keys,
        };
        if let Some(prefix) = p.sticky {
            self.pending.keys.clone_from(&prefix);
            self.pending.sticky = Some(prefix);
        }
        d
    }

    /// Feeds one key in `mode`. `now` drives the timeout for ambiguous prefixes.
    pub fn feed(&mut self, key: Key, mode: &str, cx: &impl ContextLookup, now: Instant) -> Feed {
        let mut run = Vec::new();
        // Keys pending in another mode (a sticky group whose binding switched modes, say)
        // do not carry over.
        if self.pending.mode.as_deref().is_some_and(|m| m != mode) {
            self.pending = Pending::default();
        }
        // A key after an ambiguous prefix: continue if it extends the sequence, else fire
        // the shorter binding and handle this key from the start. `<Esc>` cancels.
        if let Some((_, cand)) = self.pending.timed.take() {
            let mut keys = self.pending.keys.clone();
            keys.push(key);
            if self.node(mode, &keys).is_none() {
                if key.code == KeyCode::Esc {
                    self.pending = Pending::default();
                    return Feed {
                        run,
                        status: Status::Cancelled,
                    };
                }
                run.push(self.fire(&cand));
            }
        }
        self.pending.flush = None;
        let status = self.step(key, mode, cx, now, &mut run);
        if self.is_pending() || self.pending.sticky.is_some() {
            self.pending.mode = Some(mode.into());
        }
        Feed { run, status }
    }

    fn status(&self) -> Status {
        if self.is_pending() {
            Status::Pending
        } else {
            Status::Idle
        }
    }

    #[allow(clippy::too_many_lines)]
    fn step(
        &mut self,
        key: Key,
        mode: &str,
        cx: &impl ContextLookup,
        now: Instant,
        run: &mut Vec<Dispatch>,
    ) -> Status {
        let opts = self.options.get(mode).cloned().unwrap_or_default();
        let p = &mut self.pending;
        if p.awaiting_register {
            p.awaiting_register = false;
            if let Some(c) = key.text() {
                p.register = Some(c);
                return Status::Pending;
            }
            self.pending = Pending::default();
            return Status::Cancelled;
        }
        let at_root = p.keys.len() == p.sticky.as_ref().map_or(0, Vec::len);
        if at_root && p.sticky.is_none() {
            if opts.registers && key == Key::char('"') && p.register.is_none() && p.count.is_none()
            {
                p.awaiting_register = true;
                return Status::Pending;
            }
            if opts.counts
                && let Some(d) = key.text().and_then(|c| c.to_digit(10))
                && (d != 0 || p.count.is_some())
            {
                p.count = Some(p.count.unwrap_or(0).saturating_mul(10).saturating_add(d));
                return Status::Pending;
            }
        }
        let mut keys = p.keys.clone();
        keys.push(key);
        let parent = self.node(mode, &keys[..keys.len() - 1]);
        let mut found = parent.and_then(|n| n.child(key));
        // An exact child that can never fire must not hide `<char>` capture.
        if let (Some((n, None)), Some(par), Some(c)) = (found, parent, key.text())
            && n.select(cx).is_none()
            && !n.has_children()
            && let Some(any) = par.any_char.as_deref()
        {
            found = Some((any, Some(c)));
        }
        let Some((node, captured)) = found else {
            let sticky = self.pending.sticky.clone();
            if sticky.is_none()
                && let Some(cmd) = &opts.fallback
            {
                let p = std::mem::take(&mut self.pending);
                if at_root {
                    if let Some(d) = Self::typed(cmd, &p, &[key]) {
                        run.push(d);
                        return Status::Idle;
                    }
                } else {
                    // A prefix that led nowhere: type it, then handle this key afresh.
                    run.extend(Self::typed(cmd, &p, &p.keys));
                    return self.step(key, mode, cx, now, run);
                }
                self.pending = p;
            }
            if key.code == KeyCode::Esc && (self.is_pending() || sticky.is_some()) {
                self.pending = Pending::default();
                return Status::Cancelled;
            }
            // Unbound: drop the sequence (a sticky group stays active).
            self.pending = Pending {
                keys: sticky.clone().unwrap_or_default(),
                sticky,
                ..Pending::default()
            };
            return Status::Unmatched(keys);
        };
        let chosen = node.select(cx).cloned();
        let has_children = node.has_children();
        let sticky_here = node.sticky;
        let p = &mut self.pending;
        p.keys.clone_from(&keys);
        if let Some(c) = captured {
            p.chars.push(c);
        }
        if sticky_here {
            p.sticky = Some(keys.clone());
        }
        match (chosen, has_children) {
            (Some(c), false) => {
                run.push(self.fire(&c));
                self.status()
            }
            (Some(c), true) => {
                p.timed = Some((now + self.timeout, c));
                Status::Pending
            }
            (None, true) => {
                if p.sticky.is_none()
                    && let Some(cmd) = &opts.fallback
                {
                    p.flush = Some((now + self.timeout, cmd.clone()));
                }
                Status::Pending
            }
            (None, false) => {
                if p.sticky.is_none()
                    && let Some(cmd) = &opts.fallback
                {
                    // Nothing fires here (a failing `when`, say): type the keys instead.
                    let p = std::mem::take(&mut self.pending);
                    if let Some(d) = Self::typed(cmd, &p, &keys) {
                        run.push(d);
                        return Status::Idle;
                    }
                }
                let p = &mut self.pending;
                let sticky = p.sticky.clone();
                self.pending = Pending {
                    keys: sticky.clone().unwrap_or_default(),
                    sticky,
                    ..Pending::default()
                };
                Status::Unmatched(keys)
            }
        }
    }

    /// which-key rows for the keys pending in `mode`: groups first, then by key.
    pub fn menu(&self, mode: &str, cx: &impl ContextLookup) -> Vec<MenuItem> {
        let Some(node) = self.node(mode, &self.pending.keys) else {
            return Vec::new();
        };
        Self::menu_of(node, cx)
    }

    fn menu_of(node: &Node, cx: &impl ContextLookup) -> Vec<MenuItem> {
        let mut items: Vec<MenuItem> = node
            .children
            .iter()
            .map(|(k, n)| (k.to_string(), n))
            .chain(node.any_char.as_deref().map(|n| ("<char>".to_owned(), n)))
            .filter_map(|(key, n)| {
                let group = n.has_children();
                let label = if let Some(c) = n.select(cx) {
                    c.binding.label.as_deref().map(str::to_owned).or_else(|| {
                        match &c.binding.action {
                            Action::Run(steps) => steps.first().map(|s| s.command.to_string()),
                            Action::Unbind => None,
                        }
                    })
                } else {
                    None
                };
                let label = match (label, &n.label) {
                    (_, Some(l)) if group => Some(l.to_string()),
                    (Some(l), _) => Some(l),
                    (None, Some(l)) => Some(l.to_string()),
                    (None, None) if group => Some("+more".to_owned()),
                    (None, None) => None,
                }?;
                Some(MenuItem { key, label, group })
            })
            .collect();
        items.sort_by(|a, b| b.group.cmp(&a.group).then_with(|| a.key.cmp(&b.key)));
        items
    }

    /// Explains what `keys` do in `mode` under `cx`: the binding that runs, and why the
    /// others do not.
    pub fn describe_key(
        &self,
        mode: &str,
        keys: &[Key],
        cx: &impl ContextLookup,
    ) -> KeyDescription {
        let Some(node) = self.node(mode, keys) else {
            return KeyDescription {
                chosen: None,
                rejected: Vec::new(),
                is_prefix: false,
            };
        };
        let mut chosen = None;
        let mut rejected = Vec::new();
        for c in &node.candidates {
            let layer = &self.layers[c.layer];
            let mut d = Described {
                layer: layer.name.clone(),
                source: layer.source.clone(),
                action: c.binding.action.clone(),
                when: c.binding.when.as_ref().map(ToString::to_string),
                reason: None,
            };
            if chosen.is_some() {
                d.reason = Some("a higher binding applies".into());
                rejected.push(d);
                continue;
            }
            match c.binding.when.as_ref().and_then(|w| w.explain(cx)) {
                Some(why) => {
                    d.reason = Some(format!("when fails: {why}"));
                    rejected.push(d);
                }
                None if c.binding.action == Action::Unbind => {
                    d.reason = Some("unbound here".into());
                    rejected.push(d);
                    chosen = Some(None);
                }
                None => chosen = Some(Some(d)),
            }
        }
        KeyDescription {
            chosen: chosen.flatten(),
            rejected,
            is_prefix: node.has_children(),
        }
    }

    /// Bindings that run `command` (first step), as (mode, keys, layer).
    pub fn bindings_of(&self, command: &CommandId) -> Vec<(Arc<str>, String, Arc<str>)> {
        let mut out = Vec::new();
        for l in &self.layers {
            for b in &l.bindings {
                if let Action::Run(steps) = &b.action
                    && steps.first().is_some_and(|s| &s.command == command)
                {
                    out.push((b.mode.clone(), format_keys(&b.keys), l.name.clone()));
                }
            }
        }
        out
    }

    /// Duplicate, prefix-shadowing and layer-masking bindings.
    pub fn conflicts(&self) -> Vec<Conflict> {
        let mut out = Vec::new();
        let mut seen: HashMap<(Arc<str>, Vec<Key>), Vec<Def>> = HashMap::new();
        for (li, l) in self.layers.iter().enumerate() {
            for b in &l.bindings {
                seen.entry((b.mode.clone(), b.keys.clone()))
                    .or_default()
                    .push((li, b.when.as_ref().map(ToString::to_string)));
            }
        }
        for ((mode, keys), defs) in &seen {
            let notation = format_keys(keys);
            let names = |ls: &mut dyn Iterator<Item = usize>| -> Vec<Arc<str>> {
                let mut v: Vec<usize> = ls.collect();
                v.dedup();
                v.into_iter().map(|i| self.layers[i].name.clone()).collect()
            };
            for (i, a) in defs.iter().enumerate() {
                if defs[i + 1..].iter().any(|b| b == a) {
                    out.push(Conflict {
                        kind: ConflictKind::Duplicate,
                        mode: mode.clone(),
                        keys: notation.clone(),
                        layers: names(&mut std::iter::once(a.0)),
                    });
                }
            }
            let layers: Vec<usize> = defs.iter().map(|d| d.0).collect();
            if layers.iter().any(|&l| l != layers[0]) {
                out.push(Conflict {
                    kind: ConflictKind::LayerMask,
                    mode: mode.clone(),
                    keys: notation.clone(),
                    layers: names(&mut layers.iter().copied()),
                });
            }
            if self.node(mode, keys).is_some_and(Node::has_children) {
                out.push(Conflict {
                    kind: ConflictKind::PrefixShadow,
                    mode: mode.clone(),
                    keys: notation,
                    layers: names(&mut layers.iter().copied()),
                });
            }
        }
        out.sort_by(|a, b| (a.kind, &a.mode, &a.keys).cmp(&(b.kind, &b.mode, &b.keys)));
        out
    }
}
