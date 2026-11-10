//! Experience packs: what the editor is like, as data. A pack is one TOML file with
//! theme tokens, a keymap layer, and a UI made of named nodes. The server evaluates it;
//! frontends only draw the result.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;
use spur_core::{CommandId, Source, When};
use spur_keymap::{Layer, LayerSpec};
use spur_rpc::{Align, Anchor, Dim, Pad, Style};

/// Deepest allowed nesting of nodes.
const MAX_DEPTH: usize = 32;

/// Why a pack did not load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackError(pub String);

impl fmt::Display for PackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PackError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackFile {
    id: String,
    name: String,
    #[serde(default)]
    #[allow(dead_code)] // documentation for people reading the file
    description: String,
    #[serde(default)]
    theme: BTreeMap<String, Style>,
    #[serde(default)]
    keymap: LayerSpec,
    layout: LayoutSpec,
    node: BTreeMap<String, NodeSpec>,
    #[serde(default)]
    float: Vec<FloatSpec>,
    #[serde(default)]
    ask: AskSpec,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AskSpec {
    /// Commands listed first while the query is empty.
    #[serde(default)]
    featured: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutSpec {
    root: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum KindSpec {
    Row,
    Column,
    Text,
    Spacer,
    Surface,
    List,
    Input,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum DimSpec {
    Cells(u16),
    Word(String),
}

impl DimSpec {
    fn dim(&self, node: &str) -> Result<Dim, PackError> {
        match self {
            Self::Cells(n) => Ok(Dim::Cells(*n)),
            Self::Word(w) if w == "fit" => Ok(Dim::Fit),
            Self::Word(w) if w == "fill" => Ok(Dim::Fill(1)),
            Self::Word(w) => Err(PackError(format!(
                "node {node}: size {w:?} must be a number, \"fit\" or \"fill\""
            ))),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeSpec {
    kind: KindSpec,
    style: Option<String>,
    width: Option<DimSpec>,
    height: Option<DimSpec>,
    /// Left, top, right, bottom.
    pad: Option<[u16; 4]>,
    when: Option<String>,
    #[serde(default)]
    children: Vec<String>,
    text: Option<String>,
    #[serde(default)]
    spans: Vec<SpanSpec>,
    #[serde(default)]
    align: Align,
    source: Option<String>,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    placeholder: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpanSpec {
    text: Option<String>,
    style: Option<String>,
    when: Option<String>,
    key: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FloatSpec {
    node: String,
    anchor: Anchor,
    width: u16,
    height: u16,
    when: Option<String>,
    #[serde(default)]
    scrim: bool,
}

/// Text with `{context.key}` placeholders. `{{` is a literal brace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Template(Vec<Piece>);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Piece {
    Lit(String),
    Key(String),
}

impl Template {
    fn parse(src: &str) -> Result<Self, String> {
        let mut out = Vec::new();
        let mut lit = String::new();
        let mut rest = src;
        while let Some(c) = rest.chars().next() {
            if let Some(r) = rest.strip_prefix("{{") {
                lit.push('{');
                rest = r;
            } else if let Some(r) = rest.strip_prefix("}}") {
                lit.push('}');
                rest = r;
            } else if c == '{' {
                let end = rest
                    .find('}')
                    .ok_or_else(|| format!("unclosed {{ in {src:?}"))?;
                let key = rest[1..end].trim();
                if key.is_empty() || !key.contains('.') {
                    return Err(format!("{{{key}}} in {src:?} is not a context key"));
                }
                if !lit.is_empty() {
                    out.push(Piece::Lit(std::mem::take(&mut lit)));
                }
                out.push(Piece::Key(key.to_owned()));
                rest = &rest[end + 1..];
            } else {
                lit.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
        if !lit.is_empty() {
            out.push(Piece::Lit(lit));
        }
        Ok(Self(out))
    }

    /// Fills the placeholders with `lookup`; a missing key renders as nothing.
    pub(crate) fn render(&self, lookup: impl Fn(&str) -> Option<String>) -> String {
        let mut s = String::new();
        for p in &self.0 {
            match p {
                Piece::Lit(l) => s.push_str(l),
                Piece::Key(k) => s.push_str(&lookup(k).unwrap_or_default()),
            }
        }
        s
    }
}

/// One span of a text node.
#[derive(Clone, Debug)]
pub(crate) struct SpanDef {
    pub text: Template,
    pub style: Option<String>,
    pub when: Option<When>,
    /// Show the key bound to this command before the text; hide the span if unbound.
    pub key: Option<CommandId>,
}

/// What a node is.
#[derive(Clone, Debug)]
pub(crate) enum KindDef {
    Row(Vec<String>),
    Column(Vec<String>),
    Text {
        spans: Vec<SpanDef>,
        align: Align,
    },
    Spacer,
    Surface,
    List {
        source: String,
    },
    Input {
        source: String,
        prompt: String,
        placeholder: String,
    },
}

/// A named node.
#[derive(Clone, Debug)]
pub(crate) struct NodeDef {
    pub style: Option<String>,
    pub width: Dim,
    pub height: Dim,
    pub pad: Pad,
    pub when: Option<When>,
    pub kind: KindDef,
}

/// A float.
#[derive(Clone, Debug)]
pub(crate) struct FloatDef {
    pub node: String,
    pub anchor: Anchor,
    pub width: u16,
    pub height: u16,
    pub when: Option<When>,
    pub scrim: bool,
}

/// A loaded, validated experience pack.
#[derive(Clone, Debug)]
pub struct Pack {
    /// Pack id, e.g. `ux_fresh`.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Theme tokens.
    pub theme: BTreeMap<String, Style>,
    pub(crate) layer: Layer,
    pub(crate) root: String,
    pub(crate) nodes: BTreeMap<String, NodeDef>,
    pub(crate) floats: Vec<FloatDef>,
    /// Commands Ask lists first while the query is empty.
    pub(crate) featured: Vec<CommandId>,
}

/// List and input sources the server provides.
const SOURCES: &[&str] = &["files", "ask"];

fn when(node: &str, src: Option<&String>) -> Result<Option<When>, PackError> {
    src.map(|s| When::parse(s).map_err(|e| PackError(format!("node {node}: when {s:?}: {e}"))))
        .transpose()
}

impl Pack {
    /// Parses and validates a pack.
    ///
    /// # Errors
    ///
    /// Returns [`PackError`] naming the first problem: bad TOML, an unknown field, a
    /// child that does not exist, a cycle, a bad `when` clause or a bad key binding.
    pub fn parse(toml_src: &str) -> Result<Self, PackError> {
        let file: PackFile = toml::from_str(toml_src).map_err(|e| PackError(e.to_string()))?;
        spur_core::CommandId::new(&format!("{}.x", file.id))
            .map_err(|_| PackError(format!("pack id {:?} must be [a-z][a-z0-9_]*", file.id)))?;
        let layer = Layer::from_spec(
            &file.id,
            Source::Extension {
                id: file.id.as_str().into(),
            },
            file.keymap,
        )
        .map_err(|e| PackError(format!("keymap: {e}")))?;

        let mut nodes = BTreeMap::new();
        for (name, spec) in &file.node {
            nodes.insert(name.clone(), Self::node(name, spec)?);
        }
        let pack = Self {
            id: file.id,
            name: file.name,
            theme: file.theme,
            layer,
            root: file.layout.root,
            nodes,
            featured: file
                .ask
                .featured
                .iter()
                .map(|c| CommandId::new(c).map_err(|e| PackError(format!("ask.featured: {e}"))))
                .collect::<Result<_, _>>()?,
            floats: file
                .float
                .iter()
                .map(|f| {
                    Ok(FloatDef {
                        node: f.node.clone(),
                        anchor: f.anchor,
                        width: f.width,
                        height: f.height,
                        when: when(&f.node, f.when.as_ref())?,
                        scrim: f.scrim,
                    })
                })
                .collect::<Result<_, PackError>>()?,
        };
        pack.check_tree(&pack.root, &mut Vec::new())?;
        for f in &pack.floats {
            pack.check_tree(&f.node, &mut Vec::new())?;
        }
        Ok(pack)
    }

    fn node(name: &str, spec: &NodeSpec) -> Result<NodeDef, PackError> {
        let err = |m: String| PackError(format!("node {name}: {m}"));
        let source = || {
            let s = spec
                .source
                .clone()
                .ok_or_else(|| err("needs a source".into()))?;
            if !SOURCES.contains(&s.as_str()) {
                return Err(err(format!("unknown source {s:?} (known: {SOURCES:?})")));
            }
            Ok(s)
        };
        let container = matches!(spec.kind, KindSpec::Row | KindSpec::Column);
        if !container && !spec.children.is_empty() {
            return Err(err("only rows and columns have children".into()));
        }
        let (kind, width, height) = match spec.kind {
            KindSpec::Row => (
                KindDef::Row(spec.children.clone()),
                Dim::Fill(1),
                Dim::Fill(1),
            ),
            KindSpec::Column => (
                KindDef::Column(spec.children.clone()),
                Dim::Fill(1),
                Dim::Fill(1),
            ),
            KindSpec::Spacer => (KindDef::Spacer, Dim::Fill(1), Dim::Fill(1)),
            KindSpec::Surface => (KindDef::Surface, Dim::Fill(1), Dim::Fill(1)),
            KindSpec::List => (
                KindDef::List { source: source()? },
                Dim::Fill(1),
                Dim::Fill(1),
            ),
            KindSpec::Input => (
                KindDef::Input {
                    source: source()?,
                    prompt: spec.prompt.clone(),
                    placeholder: spec.placeholder.clone(),
                },
                Dim::Fill(1),
                Dim::Cells(1),
            ),
            KindSpec::Text => {
                let mut spans = Vec::new();
                if let Some(t) = &spec.text {
                    spans.push(SpanDef {
                        text: Template::parse(t).map_err(&err)?,
                        style: None,
                        when: None,
                        key: None,
                    });
                }
                for s in &spec.spans {
                    spans.push(SpanDef {
                        text: Template::parse(s.text.as_deref().unwrap_or_default())
                            .map_err(&err)?,
                        style: s.style.clone(),
                        when: when(name, s.when.as_ref())?,
                        key: s
                            .key
                            .as_deref()
                            .map(|k| CommandId::new(k).map_err(|e| err(e.to_string())))
                            .transpose()?,
                    });
                }
                (
                    KindDef::Text {
                        spans,
                        align: spec.align,
                    },
                    Dim::Fit,
                    Dim::Cells(1),
                )
            }
        };
        let [left, top, right, bottom] = spec.pad.unwrap_or_default();
        Ok(NodeDef {
            style: spec.style.clone(),
            width: spec.width.as_ref().map_or(Ok(width), |d| d.dim(name))?,
            height: spec.height.as_ref().map_or(Ok(height), |d| d.dim(name))?,
            pad: Pad {
                left,
                top,
                right,
                bottom,
            },
            when: when(name, spec.when.as_ref())?,
            kind,
        })
    }

    fn check_tree<'a>(&'a self, name: &'a str, path: &mut Vec<&'a str>) -> Result<(), PackError> {
        let def = self
            .nodes
            .get(name)
            .ok_or_else(|| PackError(format!("node {name:?} is used but not defined")))?;
        if path.contains(&name) {
            return Err(PackError(format!("node {name:?} contains itself")));
        }
        if path.len() >= MAX_DEPTH {
            return Err(PackError(format!("nodes nest deeper than {MAX_DEPTH}")));
        }
        if let KindDef::Row(children) | KindDef::Column(children) = &def.kind {
            path.push(name);
            for c in children {
                self.check_tree(c, path)?;
            }
            path.pop();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: &str = r#"
id = "t"
name = "T"
[layout]
root = "app"
[node.app]
kind = "column"
children = ["line"]
[node.line]
kind = "text"
text = "{buffer.name} {{x}}"
"#;

    #[test]
    fn parses_a_minimal_pack() {
        let p = Pack::parse(MIN).unwrap();
        assert_eq!(p.id, "t");
        let KindDef::Text { spans, .. } = &p.nodes["line"].kind else {
            panic!()
        };
        let out = spans[0]
            .text
            .render(|k| (k == "buffer.name").then(|| "a.rs".to_owned()));
        assert_eq!(out, "a.rs {x}");
        assert_eq!(p.nodes["line"].height, Dim::Cells(1));
    }

    #[test]
    fn rejects_broken_packs_with_a_reason() {
        let bad = |extra: &str, needle: &str| {
            let e = Pack::parse(&format!("{MIN}{extra}")).unwrap_err().0;
            assert!(e.contains(needle), "{e:?} should mention {needle:?}");
        };
        bad(
            "[node.a]\nkind = \"row\"\nchildren = [\"nope\"]\n[[float]]\nnode = \"a\"\nanchor = \"top\"\nwidth = 1\nheight = 1",
            "nope",
        );
        bad("[node.a]\nkind = \"blink\"", "blink");
        bad("[node.a]\nkind = \"text\"\nwhen = \"a ==\"", "when");
        bad("[node.a]\nkind = \"text\"\ntext = \"{oops\"", "unclosed");
        bad(
            "[node.a]\nkind = \"list\"\nsource = \"db\"",
            "unknown source",
        );
        bad("[node.a]\nkind = \"text\"\ncolour = 1", "colour");
        bad(
            "[[keymap.bind]]\nmode = \"insert\"\nkeys = \"<Nope>\"\ncommand = \"a.b\"",
            "keymap",
        );
        bad("[node.a]\nkind = \"text\"\nwidth = \"wide\"", "wide");
        let cyc = MIN.replace("children = [\"line\"]", "children = [\"app\"]");
        assert!(Pack::parse(&cyc).unwrap_err().0.contains("contains itself"));
        assert!(Pack::parse(&MIN.replace("id = \"t\"", "id = \"Bad-Id\"")).is_err());
    }
}
