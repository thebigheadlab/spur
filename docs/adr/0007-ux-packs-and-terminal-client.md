# 7. UX packs, the UI model and the terminal client

- Status: proposed
- Date: 2026-11-14
- Builds on: ADR 0005 (commands, events, context), ADR 0006 (editor server and protocol v0). Plan: epic #1, slice 2.

## Context

ADR 0006 made the core an editor server whose clients draw what it sends, with a fixed bare tree of one editor surface. Slice 2 gives Spur its first real look: the locked three-column shell (top bar, switchable left sidebar with an icon rail, code in the middle, agent column on the right) in four themes (Graphite by default, Paper as the light one, Midnight and Sand), drawn in a terminal. The look, layout and keys must stay out of the core, so they come from a *UX pack*, and the terminal client must stay a thin renderer, so any later client (desktop, web) can draw the same pack.

Terms: a *pack* is a folder of TOML files (or the same files embedded in the binary) that supplies nodes, themes, key bindings, UI state keys and user-facing messages. A *region* is a focusable part of the shell (`editor`, `sidebar`, `agent`) with its own key mode. A *notice* is the one-line message the shell shows (quit prompt, failed command, broken pack). A *token* is a named style (foreground, background, attributes) from the theme.

## Decisions

### The pack is data, evaluated on the server

A pack is a closed set of files: `pack.toml` (identity, root, default theme, state keys and their allowed values, named conditions, regions, icons, languages, messages), `settings.toml`, `layout.toml`, `components/*.toml`, `keys.toml` (a `spur-keymap` layer, unchanged) and `themes/*.toml`. Every table is closed (an unknown key is an error), and any other `.toml` file is an error, so typos and misplaced files are noticed. Nodes are flat tables keyed by id with a `children` list, not nested tables, so components can live in separate files and errors carry a line.

Nodes may carry `when` (shown only while the clause holds), `active` (switches `style` to `style_active`), text templates (`{doc.name}`), icons, sizes in cells (`fit`, `fill`, a number, or a weight), and `on_click` actions shaped like key bindings (a command or a `run` list, each with an optional `when`; the last matching one runs). `when` uses the ADR 0005 language over the context. `[conditions]` names clauses that other clauses read as `cond.<name>`, so the width rules for the columns are written once.

The server evaluates the pack per client into a plain node tree (`when`, active styles, templates and icons already resolved) and sends it with the theme's token map. Clients never evaluate clauses or templates. Loading checks the whole pack and reports every error as `file:line: path: message`: ids, cycles, regions, styles against the theme, command ids against the registered commands, every context key a clause or template reads, glyph widths, and every message id the server uses.

Units are terminal cells for every client in protocol v0, including the sizes a client sends (`hello.size`, `resize`, `layout`). A pixel client divides its window by its cell size before sending and multiplies what it receives; a later protocol version may add pixel sizes. The pack's width rules (`client.cols >= 100` for the sidebar, `>= 144` for the agent column) are worked out from the cell layout and the 64-cell minimum for the code.

Trust: a pack can bind and click any registered command, like a keymap. Packs are trusted like the editor's own settings; untrusted packs are out of scope until extensions get capabilities.

### Per-client UI state and the `ui.*` commands

Each frontend has a `UiState`: the pack's state keys (from `[state]`), the focused region, the notices, and per-list state. Before a key is fed and before the tree is evaluated, the server writes `ui.*`, `client.*`, `doc.*`, `workspace.name` and `cond.*` into the client's context scope, so key bindings and nodes read the same values. Keys are fed in the mode of the focused region; after every command and key the server checks that the focused region is still shown and otherwise moves focus to the editor.

`ui` is a reserved command namespace. The `ui.*` commands (`set`, `toggle`, `cycle`, `focus`, `list.move`, `list.page`, `list.activate`, `tree.expand`, `tree.collapse`, `quit`) are a table in `spur-server`, not core commands, because a core handler cannot reach a client's UI state. `run_as` sends them to the table before its own focus bookkeeping, so a `ui.*` command may itself run an editor command (activating a file runs `editor.open`) without the outer call undoing the inner call's view change. Key dispatch, `on_click` and the wire `command` message all go through `run_as`. A tool client gets `failed` ("no UI state"). A failed step that the person triggered (a key, a click, a paste) becomes the status notice; the wire `error` message is kept for protocol misuse such as bad key notation.

Limit, to revisit in slice 3: `ui.*` commands are not in `Core::commands()`, cannot be called through `Cx::execute`, bypass core middleware and auditing, and publish no events.

### Lists and the Files tree

A list node names a source; the one source in this slice is `workspace.files`, the workspace folder as a tree. Folders are read when first expanded (sorted folders first, then case-insensitive), capped at 10,000 entries with a "more" row; an unreadable folder shows an error row. Item ids are paths relative to the root. Symlinked folders are shown but not followed. A save re-reads the saved file's folder, so a new file appears.

Lists travel in their own `list` message carrying only the visible window, sent when the window, selection or items change, so typing never resends the tree. The client reports each list's height in `layout`, and the server windows the list to it.

### Protocol additions (v0, changed in place)

Protocol v0 is unreleased, so it changes in place:

- Nodes gain `style`, `padding`, `gap`, `clickable`, `alt` (an ASCII fallback for an icon glyph), surface options (`gutter`, `current_line`, `tab_width`), and the kinds `card`, `text`, `icon` and `list`.
- Server messages: `theme {tokens}` (sent before the first `ui` and after any change) and `list {node, view}`.
- `mouse` gains `item` (a list row's id); `layout` reports list nodes as well as surfaces.
- The `ui` message's `focus` is the focused region's node id. The client shows the terminal cursor only when focus is on a surface, and draws the selected row with the focused token only inside the focused region.
- A surface is sent in full when its node is new in the tree, its size changed, it shows another document, or the client sent `refresh`. A new `ui` alone no longer forces a full surface.

Fixed token names (`spur_rpc::tokens`) let a client draw the parts the tree does not name (selected rows, gutter, current line, cursors). A theme defines tokens by dotted names and the server resolves each fixed name through its dotted parents, so a client never needs fallback rules.

### Notices, quit and rescue

The shell shows one notice, by priority: the quit prompt, then the last failed command from a key or click, then a broken pack. `ui.quit` quits at once when no document is modified; otherwise it shows the quit prompt naming the first modified document, and a second quit while the prompt shows quits. Any other key clears the prompt.

When the editor ends any other way (a signal, a lost client, a panic) the launcher writes every modified document to a rescue copy in the per-user state folder (`$XDG_STATE_HOME/spur/rescue`, else `~/.local/state/spur/rescue`, `%LOCALAPPDATA%\spur\rescue` on Windows; relative values are ignored; folders 0700, files 0600, created with `create_new` and synced, names cleaned of control chars and cut to fit; never next to the original or in the shared temp folder), prints the paths and exits 1. If the session thread itself is broken, it writes the copies through the same sink before it ends. The launcher waits up to 3 s for the session's list of modified documents, and dropping a session waits at most 2 s for its thread, so a stuck session cannot keep the process from exiting.

### Reloading the pack

When the pack comes from a folder (`spur --ux DIR`), a watcher thread hashes the pack's files every 250 ms and sends them to the session when anything changed. The session loads them under the command panic guard. On success it swaps the pack, migrates each client's UI state (values that still fit are kept; a changed type or a value no longer allowed resets to the new default; removed keys leave the state and the scope), swaps the pack's key layer and redraws. On failure it keeps the old pack, logs every error and shows the first as the pack notice, also to clients that attach later. Clients are migrated on copies first and switched together, so a panic while reloading leaves every client on the old pack (and shows an internal-error notice). The watcher's starting hash is taken before the pack is first loaded, so an edit made during startup is still seen. The embedded pack is never watched.

### The terminal client

`spur-tui` depends only on `spur-rpc`, `crossterm` and `unicode-width`; it never links the server, core or keymap. It lays out the tree in cells (one pass per container along its main axis; fixed and `fit` sizes first, `fill` shares the rest by weight), paints styles as tone areas without box lines, and writes only the cells that changed, inside synchronised-output markers, in one write. Every string from the server is cleaned before it reaches the grid, so file names and text cannot inject escape sequences. Without true colour, theme colours map to distinct xterm-256 entries so tones do not merge. Keys become `spur-keymap` notation; where the terminal supports the kitty keyboard protocol, ctrl+shift keys arrive distinct. Mouse presses and the wheel only (no motion tracking).

A reader thread and a forwarder thread feed one channel; the main thread drains everything queued, so a burst becomes one `key` message and one draw. The client never blocks on sending: a server that leaves its queue full is treated as gone, so a stop signal is always handled. A press resolves against what was drawn (the item ids and lines of that frame), not against messages that arrived since. A drop guard and the launcher's panic hook (on the client thread only) restore the terminal.

### The launcher

`spur [--ux DIR] [--theme NAME] [--] [PATH...]` starts an in-process session with the embedded `ux_fresh` (or `--ux`) and runs the terminal client on it. Slice 4 replaces the in-process session with connect-or-spawn. Logs go to `spur/log/spur-<pid>.log` in the per-user state folder, created only when something is logged. Exit codes: 0 after quit, 1 after any other end, 2 for usage, pack and start errors.

### Look stays out of the core

The `no_look` test covers the core crates and `spur-tui`: no hex colours, no key notation (except in the client's key translation) and no non-ASCII string literals. User-facing server text comes from the pack's `[messages]` and `[languages]`.

## Deviations from the slice plan

- The generic windowed list and the Files provider stay together in `files.rs`; a separate list module waits for the second list source.
- Message templates live on the pack (`Pack::message`) instead of a separate module.
- A surface is sent in full after a resize, not row-diffed (simpler; resizes are rare).
- `client.kind` and `client.name` are still written to the client scope; a slice 1 test reads them.
- `ui.list.activate` from the wire `command` message reports a failed `editor.open` as a status notice, like a key, not as the command's error.
- Save latency: the slice budget was p50 < 0.3 ms for a save with the modified dot changing. `buffer.save` syncs the file to disk (about 1.35 ms on the test machine), so the absolute number is about 1.8 ms. The pack's part (evaluation and frames) adds about 0.26 ms over a bare save, within budget; the budget is taken to hold for the UI part only.
- The 300x100 full-redraw budget (5 ms) is checked in release builds (about 3.4 ms measured); debug builds, which CI runs, allow 50 ms.
- Syntax colours are not in this slice; the code view is plain text until the owner picks between a line tokeniser and waiting for tree-sitter.

## Latency

`cargo run --release -p spur-server --example latency` (10,000 keys per scenario, the client sends `layout` like the terminal client):

| Scenario | p50 ms | p99 ms | Budget (p50) |
|---|---|---|---|
| Typing, 80x24, ux_fresh | 0.158 | 0.271 | < 0.2 |
| Typing, 300x100, ux_fresh | 0.184 | 0.387 | < 0.2 |
| Tree Down/Up in a 5,000-entry folder | 0.218 | 0.336 | < 0.3 |
| Save, bare session | 1.524 | 2.131 | reference |
| Save with the modified dot, ux_fresh | 1.787 | 2.498 | see deviations |

## Consequences

- Changing the look, layout or keys means editing TOML, with errors reported by file and line and live reload while the editor runs.
- A second client (desktop, web) reuses the evaluated tree, tokens and lists; it implements only layout, painting and input.
- The pack format is deliberately small: `when`, conditions, templates and keymap-shaped actions. Anything more needs a written case first.
- Recorded debt:
  - `ui.*` commands are outside the core command table (no middleware, auditing or events).
  - `layout {surfaces}` now also carries lists and should be renamed in v1.
  - Reading a slow network folder blocks the session (a reader thread later); the session inbox is still unbounded (from ADR 0006).
  - The pack repeats itself: one "show sidebar view" pattern is written out for 4 views, 3 key forms and the rail's clicks. Before packs exist outside the repo, bindings should take a list of keys and/or named `[actions]` that keys and `on_click` share.
  - Conditions cannot read each other, so `keys.toml`'s width conditions repeat the numbers in the `*_shown` conditions.
  - `style` / `style_active` allow one alternate state; a second (hover, disabled) should become an ordered `styles = [{ when, style }]` list. `format = 0` must match exactly and every table is closed, so a compatibility rule is needed before the format is published.
  - What was last sent to a client is split between `Client` (surface) and `UiState` (theme, tree, lists); each list's window is rebuilt every frame; the Files tree has one global generation; the theme's tokens are built per client.
  - `UiState.trees` and the `ui.list.*` commands assume the Files tree; a list module comes with the second list source.
  - The terminal client mixes the protocol mirror (applying `ui`, `list`, `surface`, acks, `layout`) with terminal I/O; the desktop client should start from a transport-neutral model, which may later move into `spur-rpc`.
