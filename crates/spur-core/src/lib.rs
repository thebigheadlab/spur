//! Minimal editor core: commands, events, context keys (and, next, buffers, views and
//! selections).
//!
//! Every action (key, palette, extension, agent, RPC) runs as a command through
//! [`Core::execute`], so all callers get the same result and agent actions are
//! attributable. Design: `docs/adr/0005-commands-events-context.md`.

mod actor;
mod bus;
mod command;
mod context;
mod core;
pub mod editor;
mod name;
mod when;

pub use actor::{Actor, ClientId};
pub use bus::{
    DEFAULT_JOURNAL, DEFAULT_QUEUE, Delivery, Event, EventBus, Filter, Gap, Subscription,
};
pub use command::{
    Annotations, CommandError, CommandMeta, ErrorCode, RemoteSide, Source, parse_args,
};
pub use context::{Context, ContextLookup, CtxValue, Scoped};
pub use core::{
    AfterFn, AuditLog, AuditRecord, CommandDescription, CommandHandle, Core, Cx, DEFAULT_AUDIT,
    GateFn, Handler, Hook, Invocation, MAX_DEPTH, MiddlewareId, MiddlewareInfo, Phase,
    RegisterError, RewriteFn, Target,
};
pub use name::{
    CORE_NAMESPACES, CommandId, EventName, MAX_NAME_LEN, NameError, RESERVED_EVENT_FAMILIES,
    check_name, events,
};
pub use when::{ParseError, When};

/// Name of this crate, used in diagnostics.
pub const NAME: &str = env!("CARGO_PKG_NAME");

#[cfg(test)]
mod tests {
    #[test]
    fn name_matches_package() {
        assert_eq!(super::NAME, "spur-core");
    }

    #[test]
    fn core_is_send() {
        fn check<T: Send>() {}
        check::<super::Core<()>>();
    }
}
