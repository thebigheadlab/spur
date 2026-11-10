//! Message types. Every type is plain data, so any language can implement a client.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Protocol version. Client and server must match exactly in v0.
pub const PROTOCOL_VERSION: u32 = 0;

/// What kind of client is attaching.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    /// Draws the UI for a person.
    Frontend,
    /// A script or agent: sends commands, draws nothing.
    Tool,
}

/// What a frontend can draw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caps {
    /// Lays out on a character grid (a terminal) instead of pixels.
    pub cells: bool,
    /// 24-bit colour.
    pub truecolor: bool,
    /// Sends mouse clicks and scrolls.
    pub mouse: bool,
}

/// A size in layout units (terminal cells for cell-grid clients).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
}

/// Client to server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// First message. The server answers with [`ServerMsg::Welcome`] or [`ServerMsg::Bye`].
    Hello {
        /// [`PROTOCOL_VERSION`] the client speaks.
        protocol: u32,
        /// Kind of client.
        kind: ClientKind,
        /// Display name, e.g. `tui`.
        name: String,
        /// What it can draw.
        caps: Caps,
        /// Its drawing area.
        size: Size,
    },
    /// A key press in key notation: `a`, `<C-s>`, `<S-Left>`, `<Space>`, `<lt>`.
    Key {
        /// The key.
        key: String,
    },
    /// Text that is not key presses (a paste, IME output).
    Text {
        /// The text.
        text: String,
    },
    /// The drawing area changed.
    Resize {
        /// New size.
        size: Size,
    },
    /// The size the client's layout gave the editor surface. The server scrolls with it.
    SurfaceSize {
        /// Text rows.
        rows: u16,
        /// Text columns.
        cols: u16,
    },
    /// A click inside a node. For a surface, `row` is the document line and `col` the
    /// char column; for a list, `row` is the row index.
    Click {
        /// Node id.
        node: String,
        /// Row.
        row: u32,
        /// Column.
        col: u32,
    },
    /// A scroll inside a node, in lines (positive = down).
    Scroll {
        /// Node id.
        node: String,
        /// Lines.
        lines: i32,
    },
    /// Run a command by id, as this client.
    Command {
        /// Command id.
        id: String,
        /// Args (`null` = none).
        #[serde(default)]
        args: Value,
    },
    /// The client drew everything up to this [`ServerMsg::Flush`].
    Ack {
        /// Frame id.
        frame: u64,
    },
    /// Asks for a [`ServerMsg::Pong`] once everything sent before it has been handled
    /// and drawn. Used by tests, scripts and latency probes.
    Ping {
        /// Echoed back.
        id: u64,
    },
    /// The client is leaving.
    Bye,
}

/// Server to client.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// The server accepted the client.
    Welcome {
        /// Protocol version.
        protocol: u32,
        /// Server version.
        server: String,
        /// Id of this client.
        client: u64,
        /// Session name.
        session: String,
    },
    /// Theme tokens: semantic name to style. Nodes and spans refer to these names.
    Theme {
        /// The tokens.
        tokens: BTreeMap<String, Style>,
    },
    /// The whole UI tree for this client.
    Ui {
        /// The tree.
        ui: Ui,
    },
    /// The visible part of the editor surface.
    Surface {
        /// The frame.
        frame: SurfaceFrame,
    },
    /// Everything since the last flush is one frame: draw it now.
    Flush {
        /// Frame id.
        frame: u64,
    },
    /// Answer to [`ClientMsg::Ping`].
    Pong {
        /// The ping's id.
        id: u64,
    },
    /// The server is closing this connection.
    Bye {
        /// Why.
        reason: String,
    },
}

/// An RGB colour, written `#rrggbb`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color(pub u8, pub u8, pub u8);

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

impl std::str::FromStr for Color {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let hex = s.strip_prefix('#').unwrap_or(s);
        if hex.len() != 6 || !hex.is_ascii() {
            return Err(format!("colour {s:?} is not #rrggbb"));
        }
        let p = |i: usize| {
            u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| format!("colour {s:?} is not hex"))
        };
        Ok(Self(p(0)?, p(2)?, p(4)?))
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// How to draw something. Unset fields inherit from what is underneath.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Style {
    /// Text colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<Color>,
    /// Background colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<Color>,
    /// Bold.
    #[serde(default, skip_serializing_if = "is_false")]
    pub bold: bool,
    /// Italic.
    #[serde(default, skip_serializing_if = "is_false")]
    pub italic: bool,
    /// Underline.
    #[serde(default, skip_serializing_if = "is_false")]
    pub underline: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde calls it with a reference
fn is_false(b: &bool) -> bool {
    !*b
}

/// A run of text with a theme token name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// The text.
    pub text: String,
    /// Theme token name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
}

impl Span {
    /// A span with a style.
    pub fn new(text: impl Into<String>, style: &str) -> Self {
        Self {
            text: text.into(),
            style: Some(style.to_owned()),
        }
    }

    /// A span without a style.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: None,
        }
    }
}

/// One dimension of a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dim {
    /// As large as the content.
    Fit,
    /// A share of the space left over, by weight.
    Fill(u16),
    /// Exactly this many units.
    Cells(u16),
}

/// Padding inside a node, in units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pad {
    /// Left.
    pub left: u16,
    /// Top.
    pub top: u16,
    /// Right.
    pub right: u16,
    /// Bottom.
    pub bottom: u16,
}

/// Horizontal text alignment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    /// Left.
    #[default]
    Start,
    /// Right.
    End,
}

/// A UI node. The server has already evaluated bindings and conditions; a frontend only
/// lays it out and draws it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// Stable id (the node's name in the experience pack).
    pub id: String,
    /// Theme token for the node's area.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// Width.
    pub width: Dim,
    /// Height.
    pub height: Dim,
    /// Padding.
    #[serde(default)]
    pub pad: Pad,
    /// What it is.
    #[serde(flatten)]
    pub kind: NodeKind,
}

/// The node vocabulary of protocol v0.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeKind {
    /// Children side by side.
    Row {
        /// The children.
        children: Vec<Node>,
    },
    /// Children stacked.
    Column {
        /// The children.
        children: Vec<Node>,
    },
    /// One line of styled text.
    Text {
        /// The text.
        spans: Vec<Span>,
        /// Alignment.
        #[serde(default)]
        align: Align,
    },
    /// Empty space.
    Spacer,
    /// The editor surface; content arrives as [`SurfaceFrame`]s.
    Surface,
    /// A list with at most one selected row. The server owns the selection and filtering.
    List {
        /// The rows.
        rows: Vec<ListRow>,
        /// Index of the selected row.
        selected: Option<u32>,
        /// Whether the list has keyboard focus.
        focused: bool,
    },
    /// A one-line text input. The server owns the value.
    Input {
        /// Shown before the value.
        prompt: String,
        /// The value.
        value: String,
        /// Cursor position in chars.
        cursor: u32,
        /// Shown while the value is empty.
        placeholder: String,
    },
}

/// What a list row is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    /// A selectable row.
    #[default]
    Item,
    /// A section title.
    Header,
}

/// A list row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListRow {
    /// Item or header.
    #[serde(default)]
    pub kind: RowKind,
    /// Nesting level.
    #[serde(default)]
    pub indent: u16,
    /// A short glyph before the label (may be empty).
    #[serde(default)]
    pub icon: String,
    /// The label; spans with the `match` token mark matched chars.
    pub spans: Vec<Span>,
    /// Secondary text after the label.
    #[serde(default)]
    pub detail: String,
    /// Right-aligned hint, usually a key.
    #[serde(default)]
    pub hint: String,
}

/// Where a float sits in the client's area.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    /// Centred.
    Center,
    /// Centred horizontally, near the top.
    Top,
    /// Top right corner.
    TopRight,
    /// Bottom right corner.
    BottomRight,
}

/// A node drawn above the main tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Float {
    /// The content.
    pub node: Node,
    /// Position.
    pub anchor: Anchor,
    /// Width in units (clamped to the client's area).
    pub width: u16,
    /// Largest height in units; the float shrinks to fit its content.
    pub height: u16,
    /// Dim everything underneath.
    #[serde(default)]
    pub scrim: bool,
}

/// What has keyboard focus, so a frontend knows where to put the text cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Selected {
    /// The editor surface.
    #[default]
    Surface,
    /// The node with this id.
    Node(String),
}

/// The UI for one client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ui {
    /// The main tree.
    pub root: Node,
    /// Floats, bottom first.
    #[serde(default)]
    pub floats: Vec<Float>,
    /// Keyboard focus.
    #[serde(default)]
    pub focus: Selected,
}

/// A position in a document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Pos {
    /// Line, from 0.
    pub line: u32,
    /// Char column, from 0.
    pub col: u32,
}

/// Whose cursor it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorKind {
    /// This client's main cursor.
    Primary,
    /// This client's extra cursors.
    Secondary,
    /// Another client's cursor in the same document.
    Peer,
}

/// A cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// Where.
    pub pos: Pos,
    /// Whose.
    pub kind: CursorKind,
}

/// The visible lines of the editor surface. Full frames in v0 (no diffs).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceFrame {
    /// First line shown, from 0.
    pub top_line: u32,
    /// Lines in the document.
    pub line_count: u32,
    /// Visible lines as spans with `syntax.*` token names.
    pub lines: Vec<Vec<Span>>,
    /// Cursors (possibly outside the visible lines).
    pub cursors: Vec<Cursor>,
    /// This client's non-empty selections, start..end.
    pub selections: Vec<(Pos, Pos)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_as_json() {
        let msg = ServerMsg::Ui {
            ui: Ui {
                root: Node {
                    id: "root".into(),
                    style: Some("base".into()),
                    width: Dim::Fill(1),
                    height: Dim::Cells(3),
                    pad: Pad::default(),
                    kind: NodeKind::Row {
                        children: vec![Node {
                            id: "t".into(),
                            style: None,
                            width: Dim::Fit,
                            height: Dim::Cells(1),
                            pad: Pad::default(),
                            kind: NodeKind::Text {
                                spans: vec![Span::new("hi", "text")],
                                align: Align::End,
                            },
                        }],
                    },
                },
                floats: vec![],
                focus: Selected::Node("ask_input".into()),
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"ui""#), "{json}");
        assert!(json.contains(r#""kind":"row""#), "{json}");
        assert_eq!(serde_json::from_str::<ServerMsg>(&json).unwrap(), msg);

        let key = ClientMsg::Key {
            key: "<C-s>".into(),
        };
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(json, r#"{"type":"key","key":"<C-s>"}"#);
        assert_eq!(serde_json::from_str::<ClientMsg>(&json).unwrap(), key);
    }

    #[test]
    fn colours_are_hex_strings() {
        let s = Style {
            fg: Some(Color(0x8a, 0x93, 0xf8)),
            ..Style::default()
        };
        assert_eq!(serde_json::to_string(&s).unwrap(), r##"{"fg":"#8a93f8"}"##);
        assert!("#12345".parse::<Color>().is_err());
        assert!("#12345g".parse::<Color>().is_err());
        assert!(serde_json::from_str::<Style>(r#"{"colour":"x"}"#).is_err());
    }
}
