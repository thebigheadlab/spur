//! Message types. Every type is plain data with a JSON form, so a client in any
//! language can implement the protocol.
//!
//! Compatibility: receivers ignore unknown fields, and every enum has an `Unknown`
//! catch-all, so a newer sender never fails an older receiver's decode. Container
//! nodes keep their `children` whatever their kind, so an unknown container still
//! shows what is inside it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The protocol version this crate speaks.
pub const PROTOCOL_VERSION: u32 = 0;

/// What kind of client is attaching.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    /// Draws the UI for a person.
    Frontend,
    /// A script or tool: runs commands, draws nothing.
    Tool,
    /// A kind this version does not know; treated as a tool.
    #[serde(other)]
    Unknown,
}

/// How a client lays out the UI tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// On a character grid (a terminal). Sizes are cells.
    #[default]
    Cells,
    /// In pixels (a window). Protocol v0 still speaks cells to every client: a
    /// pixel client divides its window by its cell size before it sends a size,
    /// and multiplies the sizes it receives.
    Pixels,
    /// A layout this version does not know.
    #[serde(other)]
    Unknown,
}

/// What a client can do. Every field defaults, so `{}` is valid.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Caps {
    /// 24-bit colour.
    pub truecolor: bool,
    /// Sends mouse input.
    pub mouse: bool,
    /// Can show images.
    pub images: bool,
    /// Cell or pixel layout.
    pub layout: Layout,
    /// Typed components the client draws itself (none are defined in v0).
    pub native: Vec<String>,
}

/// A size in cells, for every client in protocol v0 (see [`Layout`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    /// Width.
    pub cols: u32,
    /// Height.
    pub rows: u32,
}

/// What a mouse did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseAction {
    /// A button went down.
    Press,
    /// The wheel turned.
    Scroll,
    /// An action this version does not know; ignored.
    #[serde(other)]
    Unknown,
}

/// The space a client's layout gave one surface or list node (the name predates
/// lists).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceSize {
    /// The surface or list node's id.
    pub node: String,
    /// Text rows (for a list: visible items).
    pub rows: u32,
    /// Text columns (for a surface: without the gutter).
    pub cols: u32,
}

/// Client to server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// First message. The server answers with `welcome` or `bye`.
    Hello {
        /// Every protocol version the client speaks.
        protocols: Vec<u32>,
        /// Kind of client.
        kind: ClientKind,
        /// Display name, e.g. `tui`.
        #[serde(default)]
        name: String,
        /// What it can do.
        #[serde(default)]
        caps: Caps,
        /// Its drawing area.
        #[serde(default)]
        size: Size,
    },
    /// One or more keys in key notation, fed one by one: `a`, `<C-s>`, `<S-Left>`,
    /// `<Space>`, `<lt>` (see the crate docs).
    Key {
        /// The key.
        key: String,
    },
    /// Pasted text; inserted as one edit, never read as keys.
    Paste {
        /// The text.
        text: String,
    },
    /// Mouse input on a node.
    Mouse {
        /// Node id.
        node: String,
        /// What happened.
        action: MouseAction,
        /// For `press` on a surface: document line.
        #[serde(default)]
        line: u32,
        /// For `press` on a surface: char column.
        #[serde(default)]
        col: u32,
        /// For `scroll`: lines, positive is down.
        #[serde(default)]
        lines: i32,
        /// For `press` on a list: the [`Item::id`] under the pointer.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<String>,
    },
    /// The client's area changed.
    Resize {
        /// New size.
        size: Size,
    },
    /// The space the client's layout gave each surface and list node. Sent after any
    /// draw in which that set of nodes, or one of their sizes, changed.
    Layout {
        /// One entry per surface and list node.
        surfaces: Vec<SurfaceSize>,
    },
    /// Runs a command as this client; answered by `result`.
    Command {
        /// Request id, echoed in the result.
        req: u64,
        /// Command id.
        id: String,
        /// Args (`null` = none).
        #[serde(default)]
        args: Value,
    },
    /// Replaces this client's event filters (`buffer.*`, `selection.changed`). An empty
    /// list unsubscribes.
    Subscribe {
        /// Exact event names or `prefix.*`.
        filters: Vec<String>,
    },
    /// The client drew every frame up to and including this one (cumulative).
    Ack {
        /// Frame number from `flush`.
        frame: u64,
    },
    /// Asks for a `pong` once every earlier message is handled and its frames are sent.
    Ping {
        /// Echoed back.
        id: u64,
    },
    /// The client lost its copy of what it showed (see [`SurfaceView::resolve`]): the
    /// next surface built after this arrives is sent in full. Surfaces already on
    /// their way may still carry `rows`; skip those.
    Refresh,
    /// The client is leaving.
    Bye,
    /// A message this version does not know; ignored.
    #[serde(other)]
    Unknown,
}

/// A command error as sent on the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireError {
    /// Error code, e.g. `unknown_command`, `bad_args`, `failed`.
    pub code: String,
    /// What went wrong, for people.
    pub message: String,
    /// Extra data, if the command gave any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// The outcome of a command: `"ok": value` or `"error": {...}`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    /// The command's result.
    Ok(Value),
    /// Why it failed.
    Error(WireError),
    /// An outcome this version does not know. Never sent.
    #[serde(skip_serializing)]
    Unknown,
}

impl<'de> Deserialize<'de> for Reply {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Read as a map so that an outcome added later decodes as `Unknown`; a derived
        // flattened enum would fail the whole message instead.
        let mut fields = serde_json::Map::<String, Value>::deserialize(d)?;
        if let Some(v) = fields.remove("ok") {
            return Ok(Self::Ok(v));
        }
        match fields.remove("error") {
            Some(e) => serde_json::from_value(e)
                .map(Self::Error)
                .map_err(serde::de::Error::custom),
            None => Ok(Self::Unknown),
        }
    }
}

/// Server to client.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// The server accepted the client.
    Welcome {
        /// The protocol version chosen.
        protocol: u32,
        /// Server version.
        server: String,
        /// This client's id.
        client: u64,
        /// Session name.
        session: String,
        /// Optional server behaviour a client can rely on (none in v0).
        #[serde(default)]
        features: Vec<String>,
    },
    /// The whole UI tree; sent only when it changed.
    Ui {
        /// The tree.
        root: Node,
        /// Id of the node with keyboard focus.
        #[serde(default)]
        focus: Option<String>,
    },
    /// The theme: every style name the server may send, resolved to a token. Sent
    /// before the first `ui` and again when the theme changes.
    Theme {
        /// Theme name (its file name in the pack).
        name: String,
        /// A dark theme.
        #[serde(default)]
        dark: bool,
        /// Style name to token. A name not in the map changes nothing.
        tokens: BTreeMap<String, Token>,
    },
    /// The visible window of one list node; sent when it changed, and again when the
    /// node reappears in the tree.
    List {
        /// The list node's id.
        node: String,
        /// Its window.
        list: ListView,
    },
    /// What one surface shows; sent only when it changed.
    Surface {
        /// The surface node's id.
        node: String,
        /// Its content.
        view: SurfaceView,
    },
    /// A core event that matches this client's filters.
    Event {
        /// Sequence number, from 1.
        seq: u64,
        /// Event name.
        name: String,
        /// Who caused it, e.g. `{"kind":"user"}`.
        actor: Value,
        /// Event data.
        payload: Value,
    },
    /// Events were dropped for this client because it fell behind.
    Lagged {
        /// How many.
        missed: u64,
        /// Sequence number of the first one.
        first_missed_seq: u64,
    },
    /// Answer to `command`.
    #[serde(rename = "result")]
    CommandResult {
        /// The request id.
        req: u64,
        /// The outcome.
        #[serde(flatten)]
        reply: Reply,
    },
    /// A non-fatal problem with this client's input. Only this client sees it.
    Error {
        /// What went wrong.
        message: String,
    },
    /// Everything since the last flush is one frame: draw it now, then ack it.
    Flush {
        /// Frame number, from 1.
        frame: u64,
    },
    /// Answer to `ping`.
    Pong {
        /// The ping's id.
        id: u64,
        /// The last frame number sent.
        frame: u64,
    },
    /// The server is closing this connection.
    Bye {
        /// Why.
        reason: String,
    },
    /// A message this version does not know; ignored.
    #[serde(other)]
    Unknown,
}

/// One dimension of a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Dim {
    /// As large as the content.
    Fit,
    /// A share of the space left over, by weight.
    Fill {
        /// Relative weight.
        weight: u32,
    },
    /// Exactly this many cells.
    Units {
        /// How many.
        n: u32,
    },
    /// A size this version does not know; laid out as `fill` with weight 1.
    #[serde(other)]
    Unknown,
}

impl Dim {
    /// `fill` with weight 1.
    pub const FILL: Self = Self::Fill { weight: 1 };
}

/// What a node is. The node's own fields (`children`) do not depend on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// Children stacked top to bottom.
    Column,
    /// Children side by side.
    Row,
    /// An editor surface; its content arrives in `surface` messages.
    Surface,
    /// Empty space.
    Spacer,
    /// A column a pixel client may draw as a rounded panel.
    Card,
    /// One line of text ([`Node::text`]).
    Text,
    /// One icon glyph ([`Node::text`], ASCII fallback in [`Node::alt`]), drawn
    /// centred in its rectangle.
    Icon,
    /// A scrolling list; its rows arrive in `list` messages.
    List,
    /// A kind this version does not know; laid out as a `column` of its children.
    #[serde(other)]
    Unknown,
}

/// A UI node. The server has already decided everything; a client lays it out and
/// draws it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // independent flags, as on the wire
pub struct Node {
    /// Stable id.
    pub id: String,
    /// What it is.
    pub kind: NodeKind,
    /// Width.
    pub width: Dim,
    /// Height.
    pub height: Dim,
    /// Child nodes, for containers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
    /// Style name, looked up in the theme's tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// `text`: the text; `icon`: the glyph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `icon`: an ASCII fallback for terminals that cannot show the glyph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    /// Containers: `[vertical, horizontal]` padding in layout units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub padding: Option<[u32; 2]>,
    /// Containers: units between visible children.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub gap: u32,
    /// The client sends presses on this node (`mouse` with its id).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clickable: bool,
    /// Surface: draw line numbers.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gutter: bool,
    /// Surface: mark the primary cursor's line.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub current_line: bool,
    /// Surface: tab stop; 0 means 4.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tab_width: u32,
    /// Containers: a one-cell line around the node, as six one-cell glyphs: top
    /// left, top right, bottom left, bottom right, horizontal, vertical. The line
    /// takes one cell on each side, inside the node's rectangle; padding is inside
    /// the line. A client draws nothing if the string is not six one-cell glyphs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border: Option<String>,
    /// Containers: the line's six glyphs in ASCII, for terminals that cannot show
    /// [`Node::border`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border_alt: Option<String>,
    /// Containers: style name for the line's glyphs (text colour and attributes;
    /// the node's own style stays under them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border_style: Option<String>,
    /// Text: repeat the text to fill the node's width.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub repeat: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde passes a reference
fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl Node {
    /// A node with `fill` sizes and every optional field empty.
    pub fn new(id: impl Into<String>, kind: NodeKind) -> Self {
        Self {
            id: id.into(),
            kind,
            width: Dim::FILL,
            height: Dim::FILL,
            children: Vec::new(),
            style: None,
            text: None,
            alt: None,
            padding: None,
            gap: 0,
            clickable: false,
            gutter: false,
            current_line: false,
            tab_width: 0,
            border: None,
            border_alt: None,
            border_style: None,
            repeat: false,
        }
    }
}

/// How a style looks. Unset fields keep what is under the styled area (a keyword on
/// the current-line band keeps the band).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)] // independent text attributes, as on the wire
pub struct Token {
    /// Text colour, `#rrggbb`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg: Option<String>,
    /// Background colour, `#rrggbb`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
    /// Bold.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
    /// Italic.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub italic: bool,
    /// Underlined.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub underline: bool,
    /// Dimmed.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dim: bool,
}

/// What a list row is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// An ordinary row (a file).
    #[default]
    Entry,
    /// A row with children (a folder); see [`Item::open`].
    Dir,
    /// "N more" in place of rows that were not read.
    More,
    /// A row that could not be read.
    Error,
    /// A kind this version does not know; drawn as an entry.
    #[serde(other)]
    Unknown,
}

/// One list row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// Stable id, sent back in `mouse.item`.
    pub id: String,
    /// The text.
    pub text: String,
    /// Icon glyph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glyph: Option<String>,
    /// ASCII fallback for the glyph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    /// Indent level; a cell client indents 2 cells per level.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub depth: u32,
    /// What the row is.
    #[serde(default)]
    pub kind: ItemKind,
    /// `dir`: expanded.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub open: bool,
    /// The document this client shows ([`tokens::LIST_ROW_CURRENT`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub current: bool,
}

/// The visible window of a list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListView {
    /// Index of `items[0]` in the whole list.
    pub offset: u32,
    /// Items in the whole list.
    pub total: u32,
    /// Index of the selected item in the whole list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<u32>,
    /// The visible items, in order.
    pub items: Vec<Item>,
}

/// Fixed style names the protocol itself uses; every theme should define them.
pub mod tokens {
    /// Line numbers.
    pub const GUTTER: &str = "gutter";
    /// The primary cursor line's number.
    pub const GUTTER_CURRENT: &str = "gutter.current";
    /// The primary cursor line's band.
    pub const LINE_CURRENT: &str = "line.current";
    /// Selected text.
    pub const SELECTION: &str = "selection";
    /// This client's other cursors.
    pub const CURSOR_SECONDARY: &str = "cursor.secondary";
    /// Other clients' cursors.
    pub const CURSOR_PEER: &str = "cursor.peer";
    /// A list row.
    pub const LIST_ROW: &str = "list.row";
    /// The selected row of a list outside the focused node.
    pub const LIST_ROW_SELECTED: &str = "list.row.selected";
    /// The row of the document the client shows (under a selection).
    pub const LIST_ROW_CURRENT: &str = "list.row.current";
    /// The selected row of a list inside the focused node.
    pub const LIST_ROW_SELECTED_FOCUSED: &str = "list.row.selected.focused";
    /// A `dir` row's text.
    pub const LIST_DIR: &str = "list.dir";
    /// A row's icon.
    pub const LIST_ICON: &str = "list.icon";
    /// A `more` row.
    pub const LIST_MORE: &str = "list.more";
    /// An `error` row.
    pub const LIST_ERROR: &str = "list.error";
    /// Highlight names a surface's spans may carry.
    pub const SYNTAX: [&str; 7] = [
        "syntax.keyword",
        "syntax.type",
        "syntax.function",
        "syntax.string",
        "syntax.number",
        "syntax.comment",
        "syntax.punctuation",
    ];
    /// Every fixed name.
    pub const ALL: [&str; 21] = [
        GUTTER,
        GUTTER_CURRENT,
        LINE_CURRENT,
        SELECTION,
        CURSOR_SECONDARY,
        CURSOR_PEER,
        LIST_ROW,
        LIST_ROW_SELECTED,
        LIST_ROW_CURRENT,
        LIST_ROW_SELECTED_FOCUSED,
        LIST_DIR,
        LIST_ICON,
        LIST_MORE,
        LIST_ERROR,
        SYNTAX[0],
        SYNTAX[1],
        SYNTAX[2],
        SYNTAX[3],
        SYNTAX[4],
        SYNTAX[5],
        SYNTAX[6],
    ];
}

/// A position in a document: line and char column, both from 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Pos {
    /// Line.
    pub line: u32,
    /// Char column.
    pub col: u32,
}

/// Whose cursor it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorKind {
    /// This client's main cursor.
    Primary,
    /// This client's other cursors.
    Secondary,
    /// Another client's cursor in the same document.
    Peer,
    /// A kind this version does not know; drawn as a peer cursor.
    #[serde(other)]
    Unknown,
}

/// A cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// Line.
    pub line: u32,
    /// Char column.
    pub col: u32,
    /// Whose.
    pub kind: CursorKind,
    /// For a peer cursor: that client's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<u64>,
}

/// A non-empty selection, start before end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sel {
    /// Start.
    pub start: Pos,
    /// End.
    pub end: Pos,
}

/// A run of text, with an optional semantic highlight name (never a colour).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// The text.
    pub text: String,
    /// Highlight name, e.g. `syntax.keyword`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hl: Option<String>,
}

/// One visible line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    /// Its text as spans.
    pub spans: Vec<Span>,
    /// The line was cut because it is very long.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// A run of visible rows: `count` rows copied from the previous frame starting at
/// row `from` (0 is that frame's first visible row), or, without `from`, the next
/// `count` entries of [`SurfaceView::lines`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rows {
    /// First row to copy from the previous frame; absent for new rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<u32>,
    /// Number of rows.
    pub count: u32,
}

/// [`SurfaceView::rows`] refers to rows or lines that are not there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadRows;

impl std::fmt::Display for BadRows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("surface rows do not match the previous frame")
    }
}

impl std::error::Error for BadRows {}

/// What a surface shows: the visible lines of one view, its cursors and status data.
/// Columns are in chars; the client handles tabs and wide chars.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceView {
    /// Document id.
    pub doc: u64,
    /// Text version shown; it rises with every edit of the document.
    pub version: u64,
    /// First line shown.
    pub top_line: u32,
    /// Lines in the document.
    pub line_count: u32,
    /// Visible lines: all of them when `rows` is empty, else only the rows `rows`
    /// marks as new, in order.
    pub lines: Vec<Line>,
    /// How to build the visible rows from the previous surface of this node and
    /// `lines`. Absent means `lines` holds every row.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<Rows>,
    /// This surface's number for the client, counting from 1.
    #[serde(default)]
    pub seq: u64,
    /// With `rows`: the `seq` of the surface the rows are copied from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<u64>,
    /// Cursors (possibly outside the visible lines).
    pub cursors: Vec<Cursor>,
    /// This client's non-empty selections.
    #[serde(default)]
    pub selections: Vec<Sel>,
    /// Absolute file path, if any. Clients shorten it for display.
    #[serde(default)]
    pub path: Option<String>,
    /// Unsaved changes.
    #[serde(default)]
    pub modified: bool,
    /// Mode name.
    #[serde(default)]
    pub mode: String,
}

impl SurfaceView {
    /// Turns a received surface into a full one: builds every visible row from
    /// `prev` (the last full surface of the same node, as resolved) and the new lines,
    /// and clears `rows`. A client calls this on every `surface` it receives, in order,
    /// even ones it will not draw.
    ///
    /// # Errors
    ///
    /// [`BadRows`] if the surface builds on another surface than `prev` (its `base`
    /// is not `prev`'s `seq`) or its runs do not fit; `self` is left as received. The
    /// client's copy is then lost: it sends `refresh` once, skips surfaces with `rows`
    /// until a full one arrives, and keeps acking every flush meanwhile.
    pub fn resolve(&mut self, prev: Option<&Self>) -> Result<(), BadRows> {
        if self.rows.is_empty() {
            return Ok(());
        }
        let prev = prev.filter(|p| Some(p.seq) == self.base).ok_or(BadRows)?;
        let mut new = self.lines.iter();
        let mut lines = Vec::new();
        for run in &self.rows {
            let count = run.count as usize;
            match run.from {
                Some(from) => {
                    let from = from as usize;
                    let end = from.checked_add(count).ok_or(BadRows)?;
                    lines.extend_from_slice(prev.lines.get(from..end).ok_or(BadRows)?);
                }
                None => {
                    for _ in 0..count {
                        lines.push(new.next().ok_or(BadRows)?.clone());
                    }
                }
            }
        }
        if new.next().is_some() {
            return Err(BadRows);
        }
        self.lines = lines;
        self.rows.clear();
        self.base = None;
        Ok(())
    }
}

/// Reads the protocol versions from a first message without decoding the rest, so a
/// `hello` from a future version still gets a readable refusal. `None` if `value` is
/// not a `hello` with a `protocols` list of numbers.
pub fn peek_hello(value: &Value) -> Option<Vec<u32>> {
    if value.get("type")?.as_str()? != "hello" {
        return None;
    }
    value
        .get("protocols")?
        .as_array()?
        .iter()
        .map(|v| v.as_u64().and_then(|n| u32::try_from(n).ok()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round_trip<T>(msg: &T) -> String
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let text = serde_json::to_string(msg).unwrap();
        assert_eq!(&serde_json::from_str::<T>(&text).unwrap(), msg, "{text}");
        text
    }

    fn surface() -> Node {
        Node::new("editor", NodeKind::Surface)
    }

    #[test]
    fn client_messages_have_stable_json() {
        let key = ClientMsg::Key {
            key: "<C-s>".into(),
        };
        assert_eq!(round_trip(&key), r#"{"type":"key","key":"<C-s>"}"#);
        let hello: ClientMsg =
            serde_json::from_str(r#"{"type":"hello","protocols":[0],"kind":"frontend"}"#).unwrap();
        assert_eq!(
            hello,
            ClientMsg::Hello {
                protocols: vec![0],
                kind: ClientKind::Frontend,
                name: String::new(),
                caps: Caps::default(),
                size: Size::default(),
            }
        );
        round_trip(&hello);
        for msg in [
            ClientMsg::Paste { text: "x".into() },
            ClientMsg::Mouse {
                node: "editor".into(),
                action: MouseAction::Press,
                line: 3,
                col: 1,
                lines: 0,
                item: None,
            },
            ClientMsg::Mouse {
                node: "files".into(),
                action: MouseAction::Press,
                line: 0,
                col: 0,
                lines: 0,
                item: Some("src/main.rs".into()),
            },
            ClientMsg::Resize {
                size: Size { cols: 80, rows: 24 },
            },
            ClientMsg::Layout {
                surfaces: vec![SurfaceSize {
                    node: "editor".into(),
                    rows: 23,
                    cols: 80,
                }],
            },
            ClientMsg::Command {
                req: 7,
                id: "buffer.save".into(),
                args: Value::Null,
            },
            ClientMsg::Subscribe {
                filters: vec!["buffer.*".into()],
            },
            ClientMsg::Ack { frame: 3 },
            ClientMsg::Ping { id: 1 },
            ClientMsg::Bye,
        ] {
            round_trip(&msg);
        }
    }

    #[test]
    fn server_messages_have_stable_json() {
        let flush = ServerMsg::Flush { frame: 3 };
        assert_eq!(round_trip(&flush), r#"{"type":"flush","frame":3}"#);
        let ok = ServerMsg::CommandResult {
            req: 7,
            reply: Reply::Ok(Value::Null),
        };
        assert_eq!(round_trip(&ok), r#"{"type":"result","req":7,"ok":null}"#);
        let err = ServerMsg::CommandResult {
            req: 8,
            reply: Reply::Error(WireError {
                code: "unknown_command".into(),
                message: "no such command".into(),
                data: None,
            }),
        };
        assert_eq!(
            round_trip(&err),
            r#"{"type":"result","req":8,"error":{"code":"unknown_command","message":"no such command"}}"#
        );
        let ui = ServerMsg::Ui {
            root: Node {
                height: Dim::Units { n: 3 },
                children: vec![surface()],
                ..Node::new("root", NodeKind::Column)
            },
            focus: Some("editor".into()),
        };
        let text = round_trip(&ui);
        assert!(text.contains(r#""kind":"column""#), "{text}");
        assert!(text.contains(r#"{"type":"units","n":3}"#), "{text}");
        let view = ServerMsg::Surface {
            node: "editor".into(),
            view: SurfaceView {
                doc: 1,
                version: 2,
                top_line: 0,
                line_count: 1,
                lines: vec![Line {
                    spans: vec![Span {
                        text: "hi".into(),
                        hl: None,
                    }],
                    truncated: false,
                }],
                cursors: vec![
                    Cursor {
                        line: 0,
                        col: 2,
                        kind: CursorKind::Primary,
                        peer: None,
                    },
                    Cursor {
                        line: 0,
                        col: 0,
                        kind: CursorKind::Peer,
                        peer: Some(2),
                    },
                ],
                path: Some("notes.txt".into()),
                modified: true,
                mode: "insert".into(),
                ..SurfaceView::default()
            },
        };
        let text = round_trip(&view);
        assert!(
            text.contains(r#"{"line":0,"col":0,"kind":"peer","peer":2}"#),
            "{text}"
        );
        assert!(text.contains(r#"{"spans":[{"text":"hi"}]}"#), "{text}");
        for msg in [
            ServerMsg::Welcome {
                protocol: 0,
                server: "0.0.0".into(),
                client: 1,
                session: "s".into(),
                features: vec![],
            },
            ServerMsg::Event {
                seq: 1,
                name: "buffer.changed".into(),
                actor: json!({"kind": "user"}),
                payload: json!({"doc": 1}),
            },
            ServerMsg::Lagged {
                missed: 2,
                first_missed_seq: 5,
            },
            ServerMsg::Error {
                message: "bad key".into(),
            },
            ServerMsg::Pong { id: 1, frame: 3 },
            ServerMsg::Bye {
                reason: "done".into(),
            },
        ] {
            round_trip(&msg);
        }
    }

    #[test]
    fn look_messages_have_stable_json() {
        let text = Node {
            text: Some("spur".into()),
            style: Some("topbar.brand".into()),
            width: Dim::Fit,
            height: Dim::Fit,
            ..Node::new("brand", NodeKind::Text)
        };
        let json = serde_json::to_string(&text).unwrap();
        assert_eq!(
            json,
            r#"{"id":"brand","kind":"text","width":{"type":"fit"},"height":{"type":"fit"},"style":"topbar.brand","text":"spur"}"#
        );
        let card = Node {
            padding: Some([1, 2]),
            gap: 1,
            clickable: true,
            children: vec![Node {
                gutter: true,
                current_line: true,
                tab_width: 4,
                ..surface()
            }],
            ..Node::new("main", NodeKind::Card)
        };
        round_trip(&ServerMsg::Ui {
            root: card,
            focus: Some("main".into()),
        });
        let theme = ServerMsg::Theme {
            name: "graphite".into(),
            dark: true,
            tokens: BTreeMap::from([(
                "app".to_string(),
                Token {
                    fg: Some("#a4a7b0".into()),
                    bg: Some("#111215".into()),
                    bold: true,
                    ..Token::default()
                },
            )]),
        };
        assert_eq!(
            round_trip(&theme),
            r##"{"type":"theme","name":"graphite","dark":true,"tokens":{"app":{"fg":"#a4a7b0","bg":"#111215","bold":true}}}"##
        );
        let list = ServerMsg::List {
            node: "files_list".into(),
            list: ListView {
                offset: 0,
                total: 2,
                selected: Some(1),
                items: vec![
                    Item {
                        id: "src".into(),
                        text: "src".into(),
                        glyph: Some("v".into()),
                        kind: ItemKind::Dir,
                        open: true,
                        ..Item::default()
                    },
                    Item {
                        id: "src/main.rs".into(),
                        text: "main.rs".into(),
                        depth: 1,
                        ..Item::default()
                    },
                ],
            },
        };
        let json = round_trip(&list);
        assert!(
            json.contains(r#"{"id":"src/main.rs","text":"main.rs","depth":1,"kind":"entry"}"#),
            "{json}"
        );
    }

    #[test]
    fn old_receivers_skip_look_additions() {
        // A slice 1 style node with only the base fields still decodes.
        let n: Node = serde_json::from_str(
            r#"{"id":"editor","kind":"surface","width":{"type":"fill","weight":1},"height":{"type":"fill","weight":1}}"#,
        )
        .unwrap();
        assert_eq!(n, surface());
        let i: Item = serde_json::from_str(r#"{"id":"x","text":"x","kind":"symlink"}"#).unwrap();
        assert_eq!(i.kind, ItemKind::Unknown);
        assert_eq!(tokens::ALL.len(), 21);
    }

    fn line(t: &str) -> Line {
        Line {
            spans: vec![Span {
                text: t.into(),
                hl: None,
            }],
            truncated: false,
        }
    }

    fn texts(v: &SurfaceView) -> Vec<&str> {
        v.lines.iter().map(|l| l.spans[0].text.as_str()).collect()
    }

    #[test]
    fn rows_copy_from_the_previous_frame() {
        let prev = SurfaceView {
            lines: vec![line("a"), line("b"), line("c")],
            seq: 1,
            ..SurfaceView::default()
        };
        let mut next: SurfaceView = serde_json::from_str(
            r#"{"doc":0,"version":1,"top_line":1,"line_count":4,"cursors":[],"mode":"insert",
               "lines":[{"spans":[{"text":"x"}]}],
               "rows":[{"from":1,"count":2},{"count":1}],"seq":2,"base":1}"#,
        )
        .unwrap();
        next.resolve(Some(&prev)).unwrap();
        assert_eq!(texts(&next), ["b", "c", "x"]);
        assert!(next.rows.is_empty());
        // A full frame needs no previous one.
        let mut full = prev.clone();
        full.resolve(None).unwrap();
        assert_eq!(full, prev);
        // Runs past either side are refused.
        for rows in [
            vec![Rows {
                from: Some(2),
                count: 2,
            }],
            vec![Rows {
                from: Some(u32::MAX),
                count: u32::MAX,
            }],
            vec![Rows {
                from: None,
                count: 2,
            }],
            vec![Rows {
                from: Some(0),
                count: 1,
            }],
        ] {
            let mut bad = SurfaceView {
                lines: vec![line("x")],
                rows,
                base: Some(1),
                ..SurfaceView::default()
            };
            let before = bad.clone();
            assert_eq!(bad.resolve(Some(&prev)), Err(BadRows), "{:?}", bad.rows);
            assert_eq!(bad, before);
        }
        // Runs that fit, but against another surface than the client holds.
        let mut stale = SurfaceView {
            rows: vec![Rows {
                from: Some(0),
                count: 1,
            }],
            base: Some(7),
            ..SurfaceView::default()
        };
        assert_eq!(stale.resolve(Some(&prev)), Err(BadRows));
        assert_eq!(stale.resolve(None), Err(BadRows));
    }

    #[test]
    fn newer_senders_do_not_break_decoding() {
        // Unknown fields are ignored.
        let m: ClientMsg =
            serde_json::from_str(r#"{"type":"ack","frame":1,"extra":true}"#).unwrap();
        assert_eq!(m, ClientMsg::Ack { frame: 1 });
        // Unknown message types decode to Unknown.
        let m: ServerMsg = serde_json::from_str(r#"{"type":"hologram","x":1}"#).unwrap();
        assert_eq!(m, ServerMsg::Unknown);
        let m: ClientMsg = serde_json::from_str(r#"{"type":"telepathy"}"#).unwrap();
        assert_eq!(m, ClientMsg::Unknown);
        // A command outcome added later decodes as Unknown.
        let m: ServerMsg =
            serde_json::from_str(r#"{"type":"result","req":4,"pending":{"eta":3}}"#).unwrap();
        assert_eq!(
            m,
            ServerMsg::CommandResult {
                req: 4,
                reply: Reply::Unknown
            }
        );
        // Unknown nested variants decode without failing the message, and an unknown
        // container keeps its children.
        let m: ServerMsg = serde_json::from_str(
            r#"{"type":"ui","root":{"id":"r","kind":"tabs","width":{"type":"golden"},
               "height":{"type":"fill","weight":1},"badge":"new",
               "children":[{"id":"editor","kind":"surface","width":{"type":"fit"},
               "height":{"type":"fit"}}]}}"#,
        )
        .unwrap();
        let ServerMsg::Ui { root, focus } = m else {
            panic!("not ui");
        };
        assert_eq!(focus, None);
        assert_eq!(root.kind, NodeKind::Unknown);
        assert_eq!(root.width, Dim::Unknown);
        assert_eq!(root.children[0].kind, NodeKind::Surface);
        let c: Cursor =
            serde_json::from_str(r#"{"line":1,"col":2,"kind":"ghost","peer":3}"#).unwrap();
        assert_eq!(c.kind, CursorKind::Unknown);
        let caps: Caps = serde_json::from_str(r#"{"layout":"voxels","haptics":true}"#).unwrap();
        assert_eq!(caps.layout, Layout::Unknown);
        let m: ClientMsg =
            serde_json::from_str(r#"{"type":"mouse","node":"editor","action":"hover","line":1}"#)
                .unwrap();
        assert!(matches!(
            m,
            ClientMsg::Mouse {
                action: MouseAction::Unknown,
                ..
            }
        ));
    }

    #[test]
    fn hello_versions_are_read_from_any_shape() {
        let future = json!({"type": "hello", "protocols": [3, 4], "who": {"nested": 1}});
        assert_eq!(peek_hello(&future), Some(vec![3, 4]));
        assert_eq!(peek_hello(&json!({"type": "key", "key": "a"})), None);
        assert_eq!(
            peek_hello(&json!({"type": "hello", "protocols": "0"})),
            None
        );
        assert_eq!(
            peek_hello(&json!({"type": "hello", "protocols": [-1]})),
            None
        );
        assert_eq!(peek_hello(&json!([1])), None);
    }
}
