//! The command registry, middleware, audit log and execution context.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use serde_json::Value;

use crate::actor::{Actor, ClientId};
use crate::bus::EventBus;
use crate::command::{CommandError, CommandMeta, ErrorCode, Source};
use crate::context::Context;
use crate::name::{CORE_NAMESPACES, CommandId, EventName, RESERVED_EVENT_FAMILIES};

/// Deepest nesting allowed; the top-level call is depth 0.
pub const MAX_DEPTH: u32 = 32;
/// Default audit records kept.
pub const DEFAULT_AUDIT: usize = 1024;

/// A command implementation.
pub type Handler<S> =
    Arc<dyn Fn(&mut Cx<'_, S>, &Value) -> Result<Value, CommandError> + Send + Sync>;
/// Replaces args; runs first.
pub type RewriteFn<S> =
    Arc<dyn Fn(&mut Cx<'_, S>, &CommandId, &Value) -> Option<Value> + Send + Sync>;
/// May veto; sees the final args.
pub type GateFn<S> =
    Arc<dyn Fn(&mut Cx<'_, S>, &CommandId, &Value) -> Result<(), CommandError> + Send + Sync>;
/// Sees the result; cannot change it.
pub type AfterFn<S> =
    Arc<dyn Fn(&mut Cx<'_, S>, &CommandId, &Result<Value, CommandError>) + Send + Sync>;

/// A middleware hook. Hooks are synchronous and in-process (native code only).
pub enum Hook<S> {
    /// Phase 1: rewrite args.
    Rewrite(RewriteFn<S>),
    /// Phase 2: allow or veto.
    Gate(GateFn<S>),
    /// Phase 3: observe the result.
    After(AfterFn<S>),
}

/// Middleware phase, in run order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Rewrite args.
    Rewrite,
    /// Allow or veto.
    Gate,
    /// Observe the result.
    After,
}

impl<S> Hook<S> {
    fn phase(&self) -> Phase {
        match self {
            Self::Rewrite(_) => Phase::Rewrite,
            Self::Gate(_) => Phase::Gate,
            Self::After(_) => Phase::After,
        }
    }
}

/// A resolved command: index plus generation, so a stale handle never hits a command
/// registered later in the same slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CommandHandle {
    index: u32,
    generation: u32,
}

/// Handle for removing middleware.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MiddlewareId(u64);

/// Which command to run.
#[derive(Clone, Debug)]
pub enum Target {
    /// Pre-resolved (keymaps resolve when bindings load).
    Handle(CommandHandle),
    /// By id.
    Id(CommandId),
}

/// One command call.
#[derive(Clone, Debug)]
pub struct Invocation {
    /// The command.
    pub command: Target,
    /// Args (`null` = none). Shared so static binding args are not cloned.
    pub args: Arc<Value>,
    /// Count prefix (`5dw`).
    pub count: Option<u32>,
    /// Register (`"a`).
    pub register: Option<char>,
    /// Who.
    pub actor: Actor,
    /// From which connection.
    pub client: Option<ClientId>,
    /// Core-assigned undo turn for non-agent actors.
    pub turn: u64,
}

impl Invocation {
    /// Calls `command` with no args.
    pub fn new(command: impl Into<Target>, actor: Actor) -> Self {
        Self {
            command: command.into(),
            args: Arc::new(Value::Null),
            count: None,
            register: None,
            actor,
            client: None,
            turn: 0,
        }
    }

    /// Sets the args.
    #[must_use]
    pub fn args(mut self, args: impl Into<Arc<Value>>) -> Self {
        self.args = args.into();
        self
    }

    /// Sets the count.
    #[must_use]
    pub fn count(mut self, count: u32) -> Self {
        self.count = Some(count);
        self
    }

    /// Sets the register.
    #[must_use]
    pub fn register(mut self, register: char) -> Self {
        self.register = Some(register);
        self
    }

    /// Sets the client.
    #[must_use]
    pub fn client(mut self, client: ClientId) -> Self {
        self.client = Some(client);
        self
    }

    /// Sets the turn.
    #[must_use]
    pub fn turn(mut self, turn: u64) -> Self {
        self.turn = turn;
        self
    }
}

impl From<CommandHandle> for Target {
    fn from(h: CommandHandle) -> Self {
        Self::Handle(h)
    }
}

impl From<CommandId> for Target {
    fn from(id: CommandId) -> Self {
        Self::Id(id)
    }
}

/// Why registration failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegisterError {
    /// The id is taken.
    Duplicate(CommandId),
    /// The id is outside the registrant's namespace.
    WrongNamespace {
        /// The id.
        id: CommandId,
        /// The namespace the source may use.
        allowed: String,
    },
}

impl fmt::Display for RegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate(id) => write!(f, "command {id} is already registered"),
            Self::WrongNamespace { id, allowed } => {
                write!(f, "command {id} must be in namespace {allowed}")
            }
        }
    }
}

impl std::error::Error for RegisterError {}

/// One audited command attempt.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AuditRecord {
    /// Wall-clock time the attempt started.
    pub at: SystemTime,
    /// The command, if it was known.
    pub command: Option<CommandId>,
    /// Who.
    pub actor: Actor,
    /// From which connection.
    pub client: Option<ClientId>,
    /// Undo turn (agents: their turn id).
    pub turn: u64,
    /// Nesting depth; 0 = top level.
    pub depth: u32,
    /// `Ok` or the error code.
    pub outcome: Result<(), ErrorCode>,
    /// Time taken, including middleware and nested calls.
    pub duration: Duration,
}

/// Ring of recent [`AuditRecord`]s, kept apart from the event journal so keystrokes
/// never evict events needed for replay. Nested `User` calls are not recorded.
#[derive(Debug)]
pub struct AuditLog {
    records: VecDeque<AuditRecord>,
    capacity: usize,
}

impl AuditLog {
    /// Keeps up to `capacity` records (at least 1).
    pub fn new(capacity: usize) -> Self {
        Self {
            records: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    fn push(&mut self, r: AuditRecord) {
        if self.records.len() == self.capacity {
            self.records.pop_front();
        }
        self.records.push_back(r);
    }

    /// Records, oldest first.
    pub fn records(&self) -> impl ExactSizeIterator<Item = &AuditRecord> + DoubleEndedIterator {
        self.records.iter()
    }
}

/// Middleware as listed by [`Core::describe_command`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MiddlewareInfo {
    /// Hook name.
    pub name: Arc<str>,
    /// Who added it.
    pub source: Source,
    /// Phase.
    pub phase: Phase,
    /// Priority; lower runs first within a phase.
    pub priority: i32,
}

/// Self-documentation for one command.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CommandDescription {
    /// Metadata, including the source.
    pub meta: CommandMeta,
    /// Middleware in run order.
    pub middleware: Vec<MiddlewareInfo>,
}

struct Entry<S> {
    meta: Arc<CommandMeta>,
    handler: Handler<S>,
}

struct Slot<S> {
    generation: u32,
    entry: Option<Entry<S>>,
}

struct Middleware<S> {
    id: MiddlewareId,
    name: Arc<str>,
    source: Source,
    priority: i32,
    hook: Hook<S>,
}

struct Registry<S> {
    slots: Vec<Slot<S>>,
    free: Vec<u32>,
    by_id: HashMap<CommandId, u32>,
    middleware: HashMap<CommandId, Vec<Middleware<S>>>,
    next_mw: u64,
}

impl<S> Registry<S> {
    fn entry(&self, target: &Target) -> Option<&Entry<S>> {
        let index = match target {
            Target::Handle(h) => {
                let slot = self.slots.get(h.index as usize)?;
                return (slot.generation == h.generation)
                    .then_some(slot.entry.as_ref())
                    .flatten();
            }
            Target::Id(id) => *self.by_id.get(id)?,
        };
        self.slots[index as usize].entry.as_ref()
    }
}

struct InvInfo {
    actor: Actor,
    client: Option<ClientId>,
    turn: u64,
    count: Option<u32>,
    register: Option<char>,
}

/// What a command handler sees.
pub struct Cx<'a, S> {
    /// Session state.
    pub state: &'a mut S,
    /// Context keys.
    pub context: &'a mut Context,
    registry: &'a Registry<S>,
    outbox: &'a mut Vec<(EventName, Actor, Value)>,
    audit: &'a mut AuditLog,
    info: &'a InvInfo,
    depth: u32,
    /// The extension whose handler is running, if any; it may only emit `<id>.*`.
    owner: Option<Arc<str>>,
}

impl<S> Cx<'_, S> {
    /// Who is running the command.
    pub fn actor(&self) -> &Actor {
        &self.info.actor
    }

    /// The connection, if any.
    pub fn client(&self) -> Option<ClientId> {
        self.info.client
    }

    /// Undo turn.
    pub fn turn(&self) -> u64 {
        self.info.turn
    }

    /// Count prefix.
    pub fn count(&self) -> Option<u32> {
        self.info.count
    }

    /// Register.
    pub fn register(&self) -> Option<char> {
        self.info.register
    }

    /// Nesting depth; 0 = top level.
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// Metadata of a registered command.
    pub fn meta(&self, id: &CommandId) -> Option<&CommandMeta> {
        self.registry
            .entry(&Target::Id(id.clone()))
            .map(|e| &*e.meta)
    }

    /// Runs another command as the same actor, one level deeper, without a count.
    ///
    /// # Errors
    ///
    /// Returns the command's error, or `too_deep`, `unknown_command`, `vetoed`.
    pub fn execute(&mut self, id: &CommandId, args: &Value) -> Result<Value, CommandError> {
        let info = InvInfo {
            actor: self.info.actor.clone(),
            client: self.info.client,
            turn: self.info.turn,
            count: None,
            register: None,
        };
        let mut child = Cx {
            state: &mut *self.state,
            context: &mut *self.context,
            registry: self.registry,
            outbox: &mut *self.outbox,
            audit: &mut *self.audit,
            info: &info,
            depth: self.depth + 1,
            owner: None,
        };
        run(&mut child, &Target::Id(id.clone()), args)
    }

    /// Queues an event; it is published when the top-level command returns. A handler
    /// registered by an extension may only emit `<its id>.*`; built-in handlers emit
    /// their own events whoever invoked them.
    ///
    /// # Errors
    ///
    /// Returns `permission_denied` if an extension's handler emits outside its namespace.
    pub fn emit(&mut self, name: EventName, payload: Value) -> Result<(), CommandError> {
        if let Some(id) = &self.owner {
            let ns = name.namespace();
            if ns != &**id || RESERVED_EVENT_FAMILIES.contains(&ns) {
                return Err(CommandError::new(
                    ErrorCode::PermissionDenied,
                    format!("extension {id} may not emit {name}"),
                ));
            }
        }
        self.outbox.push((name, self.info.actor.clone(), payload));
        Ok(())
    }
}

fn run<S>(cx: &mut Cx<'_, S>, target: &Target, args: &Value) -> Result<Value, CommandError> {
    let started = Instant::now();
    let at = SystemTime::now();
    let registry = cx.registry;
    let entry = registry.entry(target);
    let result = if cx.depth > MAX_DEPTH {
        Err(CommandError::new(
            ErrorCode::TooDeep,
            format!("nested deeper than {MAX_DEPTH}"),
        ))
    } else if let Some(entry) = entry {
        run_entry(cx, registry, entry, args)
    } else {
        Err(match target {
            Target::Id(id) => CommandError::unknown(id.as_str()),
            Target::Handle(_) => CommandError::unknown("<stale handle>"),
        })
    };
    if cx.depth == 0 || cx.info.actor != Actor::User {
        cx.audit.push(AuditRecord {
            at,
            command: entry.map(|e| e.meta.id.clone()).or_else(|| match target {
                Target::Id(id) => Some(id.clone()),
                Target::Handle(_) => None,
            }),
            actor: cx.info.actor.clone(),
            client: cx.info.client,
            turn: cx.info.actor.turn_id().unwrap_or(cx.info.turn),
            depth: cx.depth,
            outcome: result.as_ref().map(|_| ()).map_err(|e| e.code),
            duration: started.elapsed(),
        });
    }
    result
}

fn run_entry<S>(
    cx: &mut Cx<'_, S>,
    registry: &Registry<S>,
    entry: &Entry<S>,
    args: &Value,
) -> Result<Value, CommandError> {
    let id = &entry.meta.id;
    cx.owner = match &entry.meta.source {
        Source::Extension { id } => Some(id.clone()),
        _ => None,
    };
    let hooks = registry.middleware.get(id).map_or(&[][..], Vec::as_slice);
    let mut args = Cow::Borrowed(args);
    for mw in hooks {
        if let Hook::Rewrite(f) = &mw.hook
            && let Some(new) = f(cx, id, &args)
        {
            args = Cow::Owned(new);
        }
    }
    let result = hooks
        .iter()
        .try_for_each(|mw| match &mw.hook {
            Hook::Gate(f) => f(cx, id, &args),
            _ => Ok(()),
        })
        .and_then(|()| (entry.handler)(cx, &args));
    for mw in hooks {
        if let Hook::After(f) = &mw.hook {
            f(cx, id, &result);
        }
    }
    result
}

/// The editor core: state plus commands, events, context keys and audit. Owned by the
/// core state thread.
pub struct Core<S> {
    /// Session state.
    pub state: S,
    registry: Registry<S>,
    bus: EventBus,
    audit: AuditLog,
    context: Context,
    outbox: Vec<(EventName, Actor, Value)>,
}

impl<S> Core<S> {
    /// A core with default bus and audit sizes.
    pub fn new(state: S) -> Self {
        Self::with_bus(state, EventBus::default())
    }

    /// A core with a given bus.
    pub fn with_bus(state: S, bus: EventBus) -> Self {
        Self {
            state,
            registry: Registry {
                slots: Vec::new(),
                free: Vec::new(),
                by_id: HashMap::new(),
                middleware: HashMap::new(),
                next_mw: 0,
            },
            bus,
            audit: AuditLog::new(DEFAULT_AUDIT),
            context: Context::new(),
            outbox: Vec::new(),
        }
    }

    /// Registers a command. Extensions may use only `<id>.*`; config may use only
    /// `user.*`; core namespaces are reserved for core.
    ///
    /// # Errors
    ///
    /// Returns [`RegisterError`] for a duplicate id or a namespace the source may not use.
    pub fn register(
        &mut self,
        meta: CommandMeta,
        handler: Handler<S>,
    ) -> Result<CommandHandle, RegisterError> {
        let ns = meta.id.namespace();
        let allowed = match &meta.source {
            Source::Core => None,
            Source::Extension { id } => Some(id.to_string()),
            Source::Config { .. } => Some("user".to_owned()),
        };
        if let Some(allowed) = allowed
            && (ns != allowed || CORE_NAMESPACES.contains(&ns))
        {
            return Err(RegisterError::WrongNamespace {
                id: meta.id.clone(),
                allowed,
            });
        }
        let reg = &mut self.registry;
        if reg.by_id.contains_key(&meta.id) {
            return Err(RegisterError::Duplicate(meta.id));
        }
        let entry = Entry {
            meta: Arc::new(meta),
            handler,
        };
        let id = entry.meta.id.clone();
        let index = if let Some(i) = reg.free.pop() {
            reg.slots[i as usize].entry = Some(entry);
            i
        } else {
            reg.slots.push(Slot {
                generation: 0,
                entry: Some(entry),
            });
            u32::try_from(reg.slots.len() - 1).unwrap_or(u32::MAX)
        };
        reg.by_id.insert(id, index);
        Ok(CommandHandle {
            index,
            generation: reg.slots[index as usize].generation,
        })
    }

    /// Registers a closure.
    ///
    /// # Errors
    ///
    /// See [`Core::register`].
    pub fn register_fn<F>(
        &mut self,
        meta: CommandMeta,
        f: F,
    ) -> Result<CommandHandle, RegisterError>
    where
        F: Fn(&mut Cx<'_, S>, &Value) -> Result<Value, CommandError> + Send + Sync + 'static,
    {
        self.register(meta, Arc::new(f))
    }

    /// Removes a command; its handles stop resolving. Middleware on the id stays.
    pub fn unregister(&mut self, id: &CommandId) -> bool {
        let reg = &mut self.registry;
        let Some(index) = reg.by_id.remove(id) else {
            return false;
        };
        let slot = &mut reg.slots[index as usize];
        slot.entry = None;
        slot.generation = slot.generation.wrapping_add(1);
        reg.free.push(index);
        true
    }

    /// Resolves an id to a handle.
    pub fn resolve(&self, id: &CommandId) -> Option<CommandHandle> {
        let index = *self.registry.by_id.get(id)?;
        Some(CommandHandle {
            index,
            generation: self.registry.slots[index as usize].generation,
        })
    }

    /// Metadata of a command.
    pub fn meta(&self, id: &CommandId) -> Option<&CommandMeta> {
        self.registry
            .entry(&Target::Id(id.clone()))
            .map(|e| &*e.meta)
    }

    /// All commands, sorted by id.
    pub fn commands(&self) -> Vec<&CommandMeta> {
        let mut v: Vec<_> = self
            .registry
            .slots
            .iter()
            .filter_map(|s| s.entry.as_ref().map(|e| &*e.meta))
            .collect();
        v.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// Adds middleware on `target`. Within a phase, lower `priority` runs first; ties
    /// run in the order added.
    pub fn add_middleware(
        &mut self,
        target: CommandId,
        name: &str,
        source: Source,
        priority: i32,
        hook: Hook<S>,
    ) -> MiddlewareId {
        let reg = &mut self.registry;
        let id = MiddlewareId(reg.next_mw);
        reg.next_mw += 1;
        let list = reg.middleware.entry(target).or_default();
        list.push(Middleware {
            id,
            name: name.into(),
            source,
            priority,
            hook,
        });
        list.sort_by_key(|m| (m.hook.phase(), m.priority, m.id));
        id
    }

    /// Removes middleware.
    pub fn remove_middleware(&mut self, id: MiddlewareId) -> bool {
        self.registry.middleware.values_mut().any(|list| {
            let before = list.len();
            list.retain(|m| m.id != id);
            list.len() != before
        })
    }

    /// Metadata, source and middleware of a command.
    pub fn describe_command(&self, id: &CommandId) -> Option<CommandDescription> {
        let meta = self.meta(id)?.clone();
        let middleware = self
            .registry
            .middleware
            .get(id)
            .into_iter()
            .flatten()
            .map(|m| MiddlewareInfo {
                name: m.name.clone(),
                source: m.source.clone(),
                phase: m.hook.phase(),
                priority: m.priority,
            })
            .collect();
        Some(CommandDescription { meta, middleware })
    }

    /// Runs a command. Events it emits are published after it returns, in order, also
    /// on error (text commits are not rolled back).
    ///
    /// # Errors
    ///
    /// Returns the command's error, or `unknown_command`, `vetoed`, `too_deep`.
    pub fn execute(&mut self, inv: Invocation) -> Result<Value, CommandError> {
        let info = InvInfo {
            actor: inv.actor,
            client: inv.client,
            turn: inv.turn,
            count: inv.count,
            register: inv.register,
        };
        let mut cx = Cx {
            state: &mut self.state,
            context: &mut self.context,
            registry: &self.registry,
            outbox: &mut self.outbox,
            audit: &mut self.audit,
            info: &info,
            depth: 0,
            owner: None,
        };
        let result = run(&mut cx, &inv.command, &inv.args);
        for (name, actor, payload) in self.outbox.drain(..) {
            self.bus.publish(name, actor, payload);
        }
        result
    }

    /// The event bus.
    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    /// The event bus, for subscribing and core-originated events.
    pub fn bus_mut(&mut self) -> &mut EventBus {
        &mut self.bus
    }

    /// The audit log.
    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    /// Global context keys.
    pub fn context(&self) -> &Context {
        &self.context
    }

    /// Global context keys, mutable.
    pub fn context_mut(&mut self) -> &mut Context {
        &mut self.context
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{Delivery, Filter};
    use crate::command::parse_args;
    use serde_json::json;
    use std::sync::Mutex;

    #[derive(Debug, Default, PartialEq)]
    struct St {
        n: i64,
        log: Vec<String>,
    }

    fn id(s: &str) -> CommandId {
        CommandId::new(s).unwrap()
    }

    fn agent(turn: u64) -> Actor {
        Actor::Agent {
            session_id: "s1".into(),
            agent_kind: "test".into(),
            turn_id: turn,
            tool_call_id: Some("t9".into()),
        }
    }

    fn core() -> Core<St> {
        let mut c = Core::new(St::default());
        c.register_fn(CommandMeta::new(id("core.add"), "add"), |cx, args| {
            #[derive(serde::Deserialize)]
            struct A {
                #[serde(default = "one")]
                by: i64,
            }
            fn one() -> i64 {
                1
            }
            let a: A = parse_args(args)?;
            let times = i64::from(cx.count().unwrap_or(1));
            cx.state.n += a.by * times;
            cx.emit(
                EventName::new("buffer.changed").unwrap(),
                json!({"n": cx.state.n}),
            )?;
            Ok(json!(cx.state.n))
        })
        .unwrap();
        c
    }

    #[test]
    fn duplicates_and_namespaces() {
        let mut c = core();
        let noop = |_: &mut Cx<'_, St>, _: &Value| Ok(Value::Null);
        assert_eq!(
            c.register_fn(CommandMeta::new(id("core.add"), "again"), noop),
            Err(RegisterError::Duplicate(id("core.add")))
        );
        let ext = Source::Extension { id: "git".into() };
        assert!(
            c.register_fn(
                CommandMeta::new(id("git.stage"), "").source(ext.clone()),
                noop
            )
            .is_ok()
        );
        assert!(matches!(
            c.register_fn(CommandMeta::new(id("lsp.x"), "").source(ext), noop),
            Err(RegisterError::WrongNamespace { .. })
        ));
        let core_ns = Source::Extension {
            id: "editor".into(),
        };
        assert!(
            c.register_fn(CommandMeta::new(id("editor.x"), "").source(core_ns), noop)
                .is_err()
        );
        let cfg = Source::Config {
            file: "keys.toml".into(),
            line: 3,
        };
        assert!(
            c.register_fn(CommandMeta::new(id("user.combo"), "").source(cfg), noop)
                .is_ok()
        );
    }

    #[test]
    fn handles_go_stale_after_unregister() {
        let mut c = core();
        let h = c.resolve(&id("core.add")).unwrap();
        assert_eq!(
            c.execute(Invocation::new(h, Actor::User)).unwrap(),
            json!(1)
        );
        assert!(c.unregister(&id("core.add")));
        c.register_fn(CommandMeta::new(id("core.other"), ""), |_, _| {
            Ok(json!("other"))
        })
        .unwrap();
        let e = c.execute(Invocation::new(h, Actor::User)).unwrap_err();
        assert_eq!(e.code, ErrorCode::UnknownCommand);
    }

    #[test]
    fn same_result_for_user_and_agent_and_audit_has_turn() {
        let mut a = core();
        let mut b = core();
        let inv = |actor| {
            Invocation::new(id("core.add"), actor)
                .args(json!({"by": 2}))
                .count(3)
        };
        a.execute(inv(Actor::User)).unwrap();
        b.execute(inv(agent(42))).unwrap();
        assert_eq!(a.state, b.state);
        let rec = b.audit().records().last().unwrap();
        assert_eq!(rec.turn, 42);
        assert_eq!(rec.actor, agent(42));
        assert_eq!(rec.outcome, Ok(()));
        assert_eq!(rec.command, Some(id("core.add")));
    }

    #[test]
    fn unknown_and_bad_args_are_audited() {
        let mut c = core();
        let e = c
            .execute(Invocation::new(id("core.nope"), Actor::User))
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::UnknownCommand);
        let e = c
            .execute(Invocation::new(id("core.add"), Actor::User).args(json!({"by": "x"})))
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::BadArgs);
        let outcomes: Vec<_> = c.audit().records().map(|r| r.outcome).collect();
        assert_eq!(
            outcomes,
            [Err(ErrorCode::UnknownCommand), Err(ErrorCode::BadArgs)]
        );
    }

    #[test]
    fn middleware_phases_and_order() {
        let mut c = core();
        let order = Arc::new(Mutex::new(Vec::<String>::new()));
        let src = Source::Core;
        let log = |o: &Arc<Mutex<Vec<String>>>, s: &str| {
            o.lock().unwrap().push(s.to_owned());
        };
        let o = order.clone();
        c.add_middleware(
            id("core.add"),
            "after",
            src.clone(),
            -100,
            Hook::After(Arc::new(move |_, _, r| {
                log(&o, &format!("after:{}", r.is_ok()));
            })),
        );
        let o = order.clone();
        c.add_middleware(
            id("core.add"),
            "gate",
            src.clone(),
            -100,
            Hook::Gate(Arc::new(move |_, _, args| {
                log(&o, &format!("gate:{args}"));
                Ok(())
            })),
        );
        let o = order.clone();
        c.add_middleware(
            id("core.add"),
            "double",
            src.clone(),
            5,
            Hook::Rewrite(Arc::new(move |_, _, args| {
                log(&o, "rewrite5");
                let by = args.get("by").and_then(Value::as_i64).unwrap_or(1);
                Some(json!({"by": by * 2}))
            })),
        );
        let o = order.clone();
        c.add_middleware(
            id("core.add"),
            "first",
            src,
            1,
            Hook::Rewrite(Arc::new(move |_, _, _| {
                log(&o, "rewrite1");
                Some(json!({"by": 5}))
            })),
        );
        assert_eq!(
            c.execute(Invocation::new(id("core.add"), Actor::User))
                .unwrap(),
            json!(10)
        );
        assert_eq!(
            *order.lock().unwrap(),
            ["rewrite1", "rewrite5", "gate:{\"by\":10}", "after:true"]
        );
        let d = c.describe_command(&id("core.add")).unwrap();
        let names: Vec<_> = d.middleware.iter().map(|m| &*m.name).collect();
        assert_eq!(names, ["first", "double", "gate", "after"]);
    }

    #[test]
    fn gate_veto_reaches_after_hooks() {
        let mut c = core();
        let seen = Arc::new(Mutex::new(None));
        let s = seen.clone();
        let mw = c.add_middleware(
            id("core.add"),
            "policy",
            Source::Core,
            0,
            Hook::Gate(Arc::new(|cx, _, _| match cx.actor() {
                Actor::Agent { .. } => Err(CommandError::vetoed("policy", "agents may not add")),
                _ => Ok(()),
            })),
        );
        c.add_middleware(
            id("core.add"),
            "watch",
            Source::Core,
            0,
            Hook::After(Arc::new(move |_, _, r| {
                *s.lock().unwrap() = Some(r.clone());
            })),
        );
        let e = c
            .execute(Invocation::new(id("core.add"), agent(1)))
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Vetoed);
        assert_eq!(e.data, Some(json!({"by": "policy"})));
        assert_eq!(c.state.n, 0);
        assert_eq!(seen.lock().unwrap().clone(), Some(Err(e)));
        assert!(c.remove_middleware(mw));
        assert!(c.execute(Invocation::new(id("core.add"), agent(1))).is_ok());
    }

    #[test]
    fn nested_calls_and_depth_limit() {
        let mut c = core();
        c.register_fn(CommandMeta::new(id("core.twice"), ""), |cx, _| {
            cx.execute(&id("core.add"), &Value::Null)?;
            cx.execute(&id("core.add"), &Value::Null)
        })
        .unwrap();
        c.register_fn(CommandMeta::new(id("core.recurse"), ""), |cx, _| {
            cx.state.log.push(cx.depth().to_string());
            cx.execute(&id("core.recurse"), &Value::Null)
        })
        .unwrap();
        assert_eq!(
            c.execute(Invocation::new(id("core.twice"), Actor::User))
                .unwrap(),
            json!(2)
        );
        // Nested User calls are not audited.
        assert_eq!(c.audit().records().len(), 1);

        let e = c
            .execute(Invocation::new(id("core.recurse"), agent(1)))
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::TooDeep);
        assert_eq!(c.state.log.len(), 33, "depths 0..=32 run");
        assert_eq!(c.state.log.last().unwrap(), "32");
        // Agent calls are audited at every depth: 0..=33.
        assert_eq!(c.audit().records().len(), 1 + 34);
    }

    #[test]
    fn events_flush_after_return_even_on_error() {
        let mut c = core();
        c.register_fn(CommandMeta::new(id("core.add_then_fail"), ""), |cx, _| {
            cx.execute(&id("core.add"), &Value::Null)?;
            cx.emit(EventName::new("mode.changed").unwrap(), Value::Null)?;
            Err(CommandError::failed("boom"))
        })
        .unwrap();
        let (_, sub) = c.bus_mut().subscribe(Filter::all(), 16, None).unwrap();
        assert!(
            c.execute(Invocation::new(id("core.add_then_fail"), agent(3)))
                .is_err()
        );
        let got: Vec<_> = sub
            .drain()
            .into_iter()
            .map(|d| match d {
                Delivery::Event(e) => (e.name.to_string(), e.actor.clone()),
                Delivery::Lagged { .. } => unreachable!(),
            })
            .collect();
        assert_eq!(
            got,
            [
                ("buffer.changed".to_owned(), agent(3)),
                ("mode.changed".to_owned(), agent(3))
            ]
        );
    }

    #[test]
    fn extensions_emit_only_own_namespace() {
        let mut c = core();
        let ext = || Actor::Extension { id: "git".into() };
        c.register_fn(
            CommandMeta::new(id("git.emit"), "").source(Source::Extension { id: "git".into() }),
            |cx, args| {
                let name = EventName::new(args.as_str().unwrap_or_default())
                    .map_err(|e| CommandError::bad_args(e.to_string()))?;
                cx.emit(name, Value::Null)?;
                Ok(Value::Null)
            },
        )
        .unwrap();
        let run = |c: &mut Core<St>, name: &str| {
            c.execute(Invocation::new(id("git.emit"), ext()).args(json!(name)))
        };
        assert!(run(&mut c, "git.staged").is_ok());
        assert_eq!(
            run(&mut c, "buffer.changed").unwrap_err().code,
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            run(&mut c, "lsp.ready").unwrap_err().code,
            ErrorCode::PermissionDenied
        );
        // A built-in command run by an extension still emits its own events.
        c.register_fn(CommandMeta::new(id("core.touch"), ""), |cx, _| {
            cx.emit(EventName::new("buffer.changed").unwrap(), Value::Null)?;
            Ok(Value::Null)
        })
        .unwrap();
        let (_, sub) = c.bus_mut().subscribe(Filter::all(), 8, None).unwrap();
        assert!(c.execute(Invocation::new(id("core.touch"), ext())).is_ok());
        assert!(
            matches!(sub.try_recv(), Some(Delivery::Event(e)) if e.name.as_str() == "buffer.changed")
        );
    }
}
