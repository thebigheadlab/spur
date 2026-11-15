# 8. Palette, keys and settings

- Status: proposed
- Date: 2026-11-14
- Builds on: ADR 0005 (commands, events, context), ADR 0006 (editor server and protocol v0), ADR 0007 (UX packs and the terminal client). Plan: epic #1, slice 3.

Slice 3 ships in parts: 3a the palette, 3b keys (3b-1 actions and keys, 3b-2 editing commands, 3b-3 find), 3c settings. Each part adds its own section here.

## Context

ADR 0007 gave Spur its shell and left recorded debt: the `ui.*` commands sat outside the core command table, key hints were typed by hand, and one "show sidebar view" pattern was written out for every view, key form and click. Slice 3 adds the center palette (the locked "Ask/command box"), the default keys and the settings page. The look, the words and the keys stay in the pack; the server owns all state; clients paint.

## 3a: the palette

### `ui.*` commands are core commands with a client host

`ui.*` commands are registered in the core table, so they go through `Cx::execute`, middleware, auditing and events. Their handler cannot see a client's UI state, so `Core::execute_with` takes an optional `ClientHost`, the calling client's side of the call. The `ui` handler takes it with `Cx::take_host`, runs the command against the client's UI state, the shared pack state and the editor state, and puts it back. A nested call gets no host. A handler or middleware hook that belongs to an extension gets none either (hooks now run as their own extension): client UI goes through `ui.*` commands.

Steps that change the session itself (moving focus back when a region is hidden, dropping a client on quit, running a pack action as a key press) cannot run while the core is borrowed for the call. The host returns them as effects, which `run_as` applies after the call.

`run_as` skips scrolling and redrawing peers only when a `ui.*` call left the active view's document, version and selection unchanged. A call that moved the cursor (the palette's go to line) scrolls, redraws peers and ends the undo step like a cursor move by mouse.

A focus change made by any `ui.*` command (including the palette opening and closing) publishes `ui.focused`.

### Pack actions

`[actions.<id>]`, in `pack.toml` or `actions.toml`, names a step list with a title, a `when`, the key modes it is bound in (default `insert`) and its keys (strings, or `{ keys, when }` for a key with its own condition). The loader expands each action into keymap bindings, placed before `keys.toml`'s own so a later binding in `keys.toml` (a special case for one region) wins. `keys.toml` bindings and `on_click` entries may say `action = "<id>"` instead of repeating steps; a click on an action applies only while the action's `when` holds. A combined condition that does not parse is a load error. (3b-1 replaces the special cases with an action's own cases and binds every key to `ui.action.run`.)

This pays two ADR 0007 debts: a binding names one action instead of repeating its steps, and key hints come from the keymap.

### Key hints

A text template may contain `{key:<action>}`. It prints the action's *live key*: its first key that, pressed now in the mode the node is read in, runs this action's steps or a binding from the pack's own layer (the pack's special cases for that key count as the action), and is not masked by another layer. The mode is that of the node's region, or of the focused region for nodes outside regions and in overlays. Keys print through `[keys.display]` (modifier prefixes and key names), so the words stay in the pack.

A palette row for an action, and an overlay's `on_dismiss` action, run what a press of the live key would run, or the action's own steps if no key is live. The palette therefore runs what its hint shows. (3b-1: hints count only bindings that run the action, and rows run the action itself.)

### Overlays and inputs

`[overlays.<id>]` names a node drawn over the shell: `top`, `width`, `max_height`, `dim` (percent the shell fades toward the theme's `overlay.dim`) and `on_dismiss` (an action). An overlay is shown while the `ui.overlay` context key equals its id; its root is an extra root for reachability checks. A press outside an open overlay runs `on_dismiss` and is dropped. A press on the overlay's padding counts as inside.

`input` nodes are text fields owned by the server: `text` is the placeholder, with `placeholder_style`, an optional key `mode` while focused, and `source` (`palette`). Text lives in the client's UI state and is sent as `input {node, text, cursor, anchor}` (cursor and anchor in graphemes). Editing commands are `ui.input.insert`, `delete_backward`, `delete_forward`, `move {by, back, extend}`, `clear` and `focus`; a mode's `fallback` inserts typed text. Line breaks and tabs become spaces; a field holds at most 4,096 bytes.

Protocol additions: `ui` carries `overlays`; `input` messages; `ListView.generation` rises with each query or scope change so a client can tell which query rows answer; list items carry `key`, `detail`, match ranges and section rows. While focus is inside an overlay, the overlay's lists draw their selection as focused (the input has the cursor, the list shows the selection).

### The palette

`[palette]` names the overlay, the region it focuses and its input; `prefixes` maps one typed character on an empty query to a scope (`>` actions, `:` line). Scopes are `all`, `files`, `recent`, `actions` and `line`. `ui.palette.open {scope, query}` opens it (or switches scope, keeping the query); backspace in an empty query returns to the scope it opened with. Actions are listed and run as the region that had focus before the palette would run them.

Rows are built per frame from enum-dispatched sources: recent documents, actions that apply (titled, bound in the mode, `when` true), the line row, and the matched files. Small sources are matched on the session thread; files are matched by `nucleo` off it.

**File index.** A walker thread (the `ignore` crate: `.gitignore` and `.ignore` honoured, hidden files skipped, links not followed, at most 200,000 files) starts with the session. The first walk streams paths straight into every client's matcher. A walk older than 30 seconds is redone when a palette opens; that walk runs aside while the old paths keep serving (one at a time), and swaps in only if the files changed, after which each matcher refills. While a matcher refills, a selected file that has not come back stays selected. Saving a file the index does not list marks it stale, once per file until the next walk; missed save events mark it stale too.

**Matcher.** One `nucleo` instance per client, configured for paths. A query that extends the previous one reuses its matches. `nucleo`'s notify is coalesced into one `MatchReady` per client until the session thread has ticked.

**Selection.** The selection is stored by row id and index and follows the first row until the person moves it. A selected file that moves is found again among the best 10,000 matches. Enter runs the row as it was last drawn or chosen in the current query generation, so a tick between drawing and Enter cannot change what runs. When Enter arrives before any frame showed the current query (keys in one batch, a paste), the session waits up to 100 ms for the file matches to finish, so the row run is the current query's best.

**Go to line.** `:` or ctrl+g takes `line` or `line:column` (1-based on screen); a number past the end goes to the last line. `view.move_to`'s column is optional and defaults to the first non-blank.

### Deviations from the slice plan

- Sources are an enum, not a `ListSource` trait: four sources, one consumer.
- `ui.page` waits for the settings page (3c).
- `nucleo` ranks a word-start match in a folder equal to one in the file name; ties go to the shorter path.
- An action's `when` in the palette is evaluated with only `ui.focus` changed to the remembered region.
- Hints for multi-key alternatives (`full_keys`) wait for 3b.
- A click on blank dimmed space with no node under it does not dismiss the overlay.
- `MatchReady` handling is not timed on its own; it is inside "keystroke to settled rows".
- Palette keystroke, server time: the budget was 0.1 ms; measured 0.27 to 0.33 ms. It includes a full frame (tree evaluation for the shell plus the palette), which alone costs about as much as a typing frame. Caching the evaluated tree when only lists and inputs change is recorded below.

### Latency

`cargo run --release -p spur-server --example latency` (3,000 keys per typing scenario; the palette rows use generated workspaces 6 folders deep, names 6 to 20 characters, queries `src`, `hlt`, `mainrs`, `zzzz` typed and erased 5 times):

| Scenario | p50 ms | p99 ms | Budget (p50) |
|---|---|---|---|
| Typing, Rust file, 300x100, 1 client | 0.37 | 0.74 | parity with `main` (interleaved: 0.33 both) |
| Palette first open, 100,000 files | 2.7 | | < 50 |
| Palette later opens, 100,000 files | 0.60 | 0.87 | < 2 |
| Palette keystroke, server, 100,000 files | 0.27 | 0.55 | < 0.1 (see deviations) |
| Keystroke to settled rows, 10,000 files | 2.0 | 3.7 | < 2 |
| Keystroke to settled rows, 100,000 files | 8.0 | 19.5 | < 16 |

The typing budget is relative: interleaved runs against `main` on the same machine gave 0.29 to 0.39 ms for both.

### Consequences

- Packs bind, click and hint through named actions; hints follow key changes without edits.
- `ui.*` calls are audited, can be intercepted by middleware and publish events.
- A second client implements overlays (dim, rect, padding hit), inputs (placeholder, cursor, scroll) and section rows; it evaluates nothing.
- Recorded debt:
  - ~~An action's behaviour can be split between its own steps and `keys.toml` special cases for one key.~~ Paid in 3b-1.
  - Palette, Files tree and (soon) settings lists branch on the list id in several places; a list reference type should own move, scroll, select, window and activate before 3c.
  - Cursor positions and match ranges are grapheme indices; a web client may segment differently. Server-owned inputs have no composition (IME) model, and key display is per pack, not per client (macOS needs cmd).
  - Rendering writes `palette.note` and records recent documents; both belong where state changes.
  - Every palette frame evaluates the whole tree; a frame where only lists and inputs changed could reuse it.
  - Each client has its own `nucleo` instance and threads; revisit with many clients.

## 3b-1: actions and keys

### Actions hold their own cases

An action has its own steps and, optionally, ordered `[[actions.<id>.case]]` entries, each with a required `when` and its own steps. What an action runs now is its first case whose `when` holds, else its own steps; nothing if the action's own `when` is false. Cases are first-wins; `keys.toml` bindings and `on_click` lists stay last-wins. An action's or case's steps may not call `ui.action.run`, and an action key may not capture `<char>`.

This pays the 3a debt: `ux_fresh`'s `keys.toml` loses its 17 special-case bindings (ctrl+b hides a shown sidebar, a view's key again goes back to the code); they are cases of `toggle_sidebar`, `toggle_agent`, `agent_show` and the four sidebar views. A matrix recorded on `main` (15 keys and the 7 palette rows, in each focus, with each sidebar view, at 99, 100, 143 and 144 columns either side of where the columns stop fitting, with each column open or closed: 4,224 cases) gives the same results after.

### One resolver: `ui.action.run`

Every action key is an ordinary binding with one step, `ui.action.run {id}`. So are `keys.toml` and `on_click` entries that say `action = "<id>"`. The command checks the id and that the action applies now (else it fails), and returns an effect; the action runs after the call. The session then resolves the action's case against the client's scope and runs its steps, each as a top-level call, the count and register going to the first step. Palette rows and overlay `on_dismiss` run the same way. Each step is its own undo step: an editing action that must undo as one is one command.

When a client's keymap is assembled, every binding in any layer whose steps are exactly `ui.action.run {id}` gets the action's `when` beside its own and the action's title as its label. A key therefore falls through to a lower binding or the mode's fallback when the action does not apply, and which-key shows the title. Actions run inside one another at most 4 deep (Enter on a palette row is an action running an action). An overlay's `on_dismiss` action may not have a `when`, so a press outside always dismisses. A binding that runs an action may not capture `<char>`.

`Action::press` and the rule that any binding of the pack's own layer counts as the action are gone. A key's hint is now its first key whose chosen binding runs `ui.action.run` of that very action. A palette row runs the action it names, not whatever a key bound to it would run.

The keymap's dispatch is unchanged: no named-action variant, at the cost of one extra audited call per action key.

### Keys a legacy terminal can send

`hello.caps.key_encoding` is `full` (every key and modifier arrives as pressed: the kitty keyboard protocol, a window) or `legacy` (xterm-style bytes; the default, and what an unknown value means). The terminal client sends `full` when it turned the kitty protocol on, and always on Windows, where crossterm reads key events directly.

For a legacy client, hints skip keys that cannot arrive as themselves, per `spur_keymap::legacy_can_send`: ctrl only on a to z (not i, m; not h, j on purpose), space, `/` and 4 to 6; no ctrl+shift on letters, no shift on other chars, no modifiers on Enter, Tab, Backspace and Esc except shift+Tab, no alt+`[` or alt+`O`, no super, F1 to F12 only, and F3 without modifiers (a modified F3 reads as a cursor position report). Every key of a chord must pass. The rule models xterm-style terminals as crossterm reads them; another client sends `full` or gets its own value. A test builds the bytes such a terminal sends for every key with every mix of ctrl, alt and shift, reads them as crossterm 0.29 does (a copy of its private parser), and checks the rule says a key arrives exactly when it comes back as itself. Both the encoder and the parser are models, so the test checks the rule against them, not against a terminal; it runs on Unix.

The terminal client reads ctrl+7 as ctrl+/ without full keys (both are byte 0x1F). `ux_fresh` lists the full key first and backups after it (ctrl+shift+e, then alt+1, then ctrl+k e; ctrl+shift+z, then ctrl+y), so a legacy terminal's hint names a backup.

### A calmer top bar

The top bar's key chips (save, go to, files, agent, quit, and each panel's keys) read as crowded. The owner picked, from three options, a "command pill": the focused panel's name, at most two keys of its own (Files: enter open, esc code), and one soft pill styled like the palette's field, `› Go to file or action  ctrl+space`, which shrinks to `› ctrl+space` below 120 columns and opens the palette when clicked. Every other key is listed in the palette with its action. Pack-only: `topbar.toml` and the theme tokens `key`, `hint.sep`, `pill`, `pill.mark` and `pill.key`.

### Deviations from the plan

- The one-time "some shortcuts need a newer terminal" notice is not built: the top bar hides key hints while a notice shows, and the hints already name keys that work.
- Esc after a pending chord prefix (needed once Esc is bound in the code) moves to 3b-2 with its first user.
- Per-client key display (macOS) waits for the desktop client; the list reference type waits for 3c.

### Latency

Every action key now makes one more audited call (`ui.action.run`). Interleaved with `main` on the same machine, 3 runs each of 3,000 keys, p50 round trip in ms (branch / main):

| Scenario | Branch | `main` |
|---|---|---|
| ctrl+b at 300x100 (the sidebar's hide and show cases) | 0.26 to 0.36 | 0.27 to 0.31 |
| Down and Up in the Files tree, 160x45 (list keys are actions) | 0.18 to 0.20 | 0.22 to 0.24 |
| Typing at 300x100 | 0.30 to 0.36 | 0.27 to 0.36 |
| Save at 160x45 (syncs the file; noisy) | 0.83 to 1.42 | 0.86 to 1.30 |

All at parity within the run-to-run spread.
