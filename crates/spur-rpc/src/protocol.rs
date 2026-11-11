//! Message types. Every type is plain data with a JSON form, so a client in any
//! language can implement the protocol.
//!
//! Compatibility: receivers ignore unknown fields, and every enum has an `Unknown`
//! catch-all, so a newer sender never fails an older receiver's decode. Container
//! nodes keep their `children` whatever their kind, so an unknown container still
//! shows what is inside it.

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
    /// In pixels (a window). Sizes are logical pixels.
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

/// A size in layout units (cells or logical pixels, see [`Layout`]).
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

/// The space a client's layout gave one surface node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceSize {
    /// The surface node's id.
    pub node: String,
    /// Text rows.
    pub rows: u32,
    /// Text columns.
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
    },
    /// The client's area changed.
    Resize {
        /// New size.
        size: Size,
    },
    /// The space the client's layout gave each surface.
    Layout {
        /// One entry per surface node.
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
    /// Exactly this many layout units.
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
    /// A kind this version does not know; laid out as a `column` of its children.
    #[serde(other)]
    Unknown,
}

/// A UI node. The server has already decided everything; a client lays it out and
/// draws it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Visible lines.
    pub lines: Vec<Line>,
    /// Cursors (possibly outside the visible lines).
    pub cursors: Vec<Cursor>,
    /// This client's non-empty selections.
    #[serde(default)]
    pub selections: Vec<Sel>,
    /// File path as it was opened, if any. Clients shorten it for display.
    #[serde(default)]
    pub path: Option<String>,
    /// Unsaved changes.
    #[serde(default)]
    pub modified: bool,
    /// Mode name.
    #[serde(default)]
    pub mode: String,
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
        Node {
            id: "editor".into(),
            kind: NodeKind::Surface,
            width: Dim::FILL,
            height: Dim::FILL,
            children: vec![],
        }
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
                id: "root".into(),
                kind: NodeKind::Column,
                width: Dim::FILL,
                height: Dim::Units { n: 3 },
                children: vec![surface()],
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
                selections: vec![],
                path: Some("notes.txt".into()),
                modified: true,
                mode: "insert".into(),
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
