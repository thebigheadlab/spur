# 5. Commands, events and context keys

- Status: proposed
- Date: 2026-11-08
- Spec: #21 (command registry and event bus); architecture #3. Used by #23 (keymap), #24 (Vim preset), #35 (MCP export). (ADR 0004 is reserved for large files and the journal, see ADR 0003.)

## Context

Every action in Spur (key, palette, extension, agent, RPC) must go through one command layer, so that all callers get the same result, agent actions can be attributed to a turn, and commands can later be exported as MCP tools without wrappers. Keymaps, menus and views need one shared condition language. Key dispatch has a 50 µs budget (#23), so the command path cannot allocate per call.

## Decisions

### Actor vs client

`Actor` is the principal: `User`, `Agent { session_id, agent_kind, turn_id, tool_call_id }`, `Extension { id }`, `Remote { client }` (a scripted RPC principal) or `System { source }`. The connection is a separate `ClientId`. A human is `User` from any frontend. (Amended by ADR 0006: a person's undo is per window, keyed by author and view, so a client that connects again starts a new undo history until resume (slice 4) carries its view over.)

| Actor | Text `AuthorId` | Undo turn |
|---|---|---|
| User | `User` | `Invocation.turn` (core-assigned) |
| Agent | `Agent(session_id)` | `turn_id` |
| Extension | `Extension(id)` | `Invocation.turn` |
| Remote | `Rpc(client)` | `Invocation.turn` |
| System | `External(source)` | `Invocation.turn` |

### Names

Command ids, event names and context keys share one grammar: two or more dot-separated segments of `[a-z][a-z0-9_]*`, no `__`, at most 64 bytes. MCP export maps `.` to `__`, which is reversible. Extensions register only `<id>.*`, config only `user.*`; `core`, `editor`, `view`, `buffer`, `bus` and `command` belong to the core.

### Commands

Args and results are `serde_json::Value`, with a JSON Schema and MCP annotations (`read_only`, `destructive`, `idempotent`, `open_world`) as metadata. Count and register are typed fields of `Invocation`, so Vim's `5dw` and `"ayy` add no JSON. Callers can resolve an id once to a `CommandHandle` (index + generation); a handle goes stale when its command is unregistered. `Core<S>` is generic over the session state, so the registry is tested on its own and the editor state plugs in next.

Nested calls (`Cx::execute`) keep the actor; depth 0 is the top level and depth > 32 fails with `too_deep`.

### Middleware

Three fixed phases per command: **rewrite** (replace args), **gate** (veto, sees the final args) and **after** (sees the result). Within a phase, lower priority runs first and ties keep the order added. A gate always runs after every rewrite, so an approval cannot be undone by a later hook. Hooks are synchronous and in-process, so only native code adds them; out-of-process extensions observe via events. `around` is not provided: rewrite, gate and after cover logging, permission checks and arg rewriting, and no hook can skip a command silently.

### Events

Events (`seq`, `name`, `actor`, `payload`) get a sequence number from 1. A ring journal (default 4096) serves `replay(since)` (exclusive), or reports a `Gap` when the events are gone. `subscribe(filter, capacity, since)` returns the backlog and a live queue together, so nothing is missed or duplicated.

Each subscriber has a bounded queue (default 256). Publishing never waits: when a queue is full the event is counted as missed, and the subscriber receives one `Lagged { missed, first_missed_seq }` in order before the next event, or on its next receive. Filters are exact names or `prefix.*`.

Events a command emits go to an outbox and are published when the top-level call returns, also on error, because text commits are not rolled back. Handlers an extension registers may emit only `<id>.*` (built-in commands emit their own events whoever invokes them); the families `buffer`, `selection`, `mode`, `view`, `client`, `extension`, `command`, `bus`, `agent`, `changeset` and `checkpoint` are reserved.

### Audit

Each command attempt (including unknown, vetoed and too-deep ones) produces a typed `AuditRecord` with actor, client, turn, depth, outcome and duration. Records go to their own ring (default 1024), not the event journal, so typing cannot evict agent events needed for replay. Nested calls by `User` are not recorded; nested calls by agents and extensions are.

### Context keys and `when`

Context keys hold `Bool`, `Int`, `Str` or `List` values. Scopes are layered through a `ContextLookup` trait (later scopes win). A generation counter lets which-key and menus cache results.

`when` supports `!`, `&&`, `||`, parentheses, `==`/`!=` (a bare word on the right is a string: `editor.mode == normal`), `<`, `<=`, `>` and `>=` on integers, `=~ /re/i`, and `[not] in` (list membership or substring). A missing key is falsy, and different types are never equal. Clauses are parsed once and regexes compiled at parse time. `explain` returns the first failing clause with the key's actual value, for `describe-key`.

## Consequences

- Measured on a 4 vCPU container (`cargo bench -p spur-core`): a 4-clause `when` takes 78 ns, and executing a no-arg command with audit takes 135 ns. That leaves room for the 50 µs dispatch budget.
- JSON args are untyped at the handler boundary. Handlers parse with `parse_args` (`null` counts as `{}`), and schema validation for RPC callers is left to #22.
- Middleware is native-only until the extension host (#30) defines an async equivalent.
