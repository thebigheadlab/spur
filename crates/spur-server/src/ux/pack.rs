//! Loading and checking a UX pack: a folder of TOML files that holds a look (layout,
//! components, themes, key bindings, labels). Nothing here knows any one pack.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::Value;
use spur_core::editor::VIEW_KEYS;
use spur_core::{CommandId, Source, When, check_name};
use spur_keymap::{Layer, Step};
use spur_rpc::{Dim, NodeKind, Token, tokens};
use toml::Spanned;
use toml::de::{DeTable, DeValue, ValueDeserializer};
use unicode_width::UnicodeWidthStr;

use crate::lang;

/// Pack format version this server reads.
pub const FORMAT: i64 = 0;

/// Message ids the server uses, with the placeholders each may hold.
pub const MESSAGES: &[(&str, &[&str])] = &[
    ("doc.untitled", &[]),
    ("quit.unsaved", &["doc"]),
    ("command.failed", &["command", "reason"]),
    ("list.more", &["count"]),
    ("list.unreadable", &["reason"]),
];

/// Icons the Files provider uses.
pub const REQUIRED_ICONS: &[&str] = &["folder", "folder_open", "file"];

/// Context keys the server writes for every client, besides declared state, the view
/// keys and `cond.*`.
pub const BUILTIN_KEYS: &[&str] = &[
    "ui.focus",
    "ui.notice",
    "client.cols",
    "client.rows",
    "doc.name",
    "doc.language",
    "workspace.name",
];

/// Where a pack's files come from.
#[derive(Clone, Debug)]
pub enum PackSource {
    /// A folder on disk; watched for changes.
    Dir(PathBuf),
    /// Files built into the binary: (path relative to the pack, contents).
    Embedded(&'static [(&'static str, &'static str)]),
}

/// One problem in a pack, shown as `file:line: path: message`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackError {
    /// File, relative to the pack (`layout.toml`).
    pub file: String,
    /// 1-based line, when known.
    pub line: Option<u32>,
    /// Where in the file (`node.sidebar.width`); may be empty.
    pub path: String,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for PackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.file)?;
        if let Some(l) = self.line {
            write!(f, ":{l}")?;
        }
        if !self.path.is_empty() {
            write!(f, ": {}", self.path)?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for PackError {}

/// A state key's value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StateValue {
    /// A string.
    Str(String),
    /// A flag.
    Bool(bool),
    /// A number.
    Int(i64),
}

impl StateValue {
    /// Same type as `other`.
    pub fn same_type(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }

    /// From a JSON command argument.
    pub fn from_json(v: &Value) -> Option<Self> {
        match v {
            Value::String(s) => Some(Self::Str(s.clone())),
            Value::Bool(b) => Some(Self::Bool(*b)),
            Value::Number(n) => n.as_i64().map(Self::Int),
            _ => None,
        }
    }

    /// As text in a template.
    pub fn render(&self) -> String {
        match self {
            Self::Str(s) => s.clone(),
            Self::Bool(b) => b.to_string(),
            Self::Int(i) => i.to_string(),
        }
    }
}

/// A `{key}` template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template(Vec<Part>);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Part {
    Lit(String),
    Key(String),
}

impl Template {
    /// Parses `{key}` and `{{` / `}}`.
    ///
    /// # Errors
    ///
    /// An unclosed or stray brace, or an empty key.
    pub fn parse(src: &str) -> Result<Self, String> {
        let mut parts = Vec::new();
        let mut lit = String::new();
        let mut chars = src.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    lit.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    lit.push('}');
                }
                '{' => {
                    let mut key = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some(c) => key.push(c),
                            None => return Err("unclosed {".into()),
                        }
                    }
                    let key = key.trim().to_owned();
                    if key.is_empty() || key.contains('{') {
                        return Err(format!("bad placeholder {{{key}}}"));
                    }
                    if !lit.is_empty() {
                        parts.push(Part::Lit(std::mem::take(&mut lit)));
                    }
                    parts.push(Part::Key(key));
                }
                '}' => return Err("stray } (write }} for a brace)".into()),
                c => lit.push(c),
            }
        }
        if !lit.is_empty() {
            parts.push(Part::Lit(lit));
        }
        Ok(Self(parts))
    }

    /// The keys it reads.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.iter().filter_map(|p| match p {
            Part::Key(k) => Some(k.as_str()),
            Part::Lit(_) => None,
        })
    }

    /// Fills in the keys; an unset key is empty.
    pub fn render(&self, mut get: impl FnMut(&str) -> Option<String>) -> String {
        let mut out = String::new();
        for p in &self.0 {
            match p {
                Part::Lit(s) => out.push_str(s),
                Part::Key(k) => out.push_str(&get(k).unwrap_or_default()),
            }
        }
        out
    }
}

/// A focusable region.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    /// Its node.
    pub node: String,
    /// Key mode while it has focus; `None` for the editor (the view's own mode).
    pub mode: Option<String>,
}

/// An icon: a one-cell glyph and an ASCII fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Icon {
    /// The glyph.
    pub glyph: String,
    /// One ASCII char.
    pub ascii: String,
}

/// One `on_click` entry: steps run when `when` holds; the last holding entry wins.
#[derive(Clone, Debug)]
pub struct ClickEntry {
    /// Condition.
    pub when: Option<When>,
    /// What runs.
    pub steps: Vec<Step>,
}

/// A node as the pack defines it.
#[derive(Clone, Debug)]
pub struct NodeSpec {
    /// Kind.
    pub kind: NodeKind,
    /// Child ids.
    pub children: Vec<String>,
    /// Width.
    pub width: Dim,
    /// Height.
    pub height: Dim,
    /// Visibility.
    pub when: Option<When>,
    /// Style name.
    pub style: Option<String>,
    /// Style while `active` holds.
    pub style_active: Option<String>,
    /// Condition for `style_active`.
    pub active: Option<When>,
    /// Padding.
    pub padding: Option<[u32; 2]>,
    /// Gap.
    pub gap: u32,
    /// Text template.
    pub text: Option<Template>,
    /// Icon name.
    pub icon: Option<String>,
    /// Click actions.
    pub on_click: Vec<ClickEntry>,
    /// List data provider.
    pub source: Option<String>,
    /// Surface: line numbers.
    pub gutter: bool,
    /// Surface: current-line band.
    pub current_line: bool,
}

/// A theme with its tokens resolved to `#rrggbb`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    /// Dark.
    pub dark: bool,
    /// Defined tokens.
    pub tokens: BTreeMap<String, Token>,
}

impl Theme {
    /// The token for `name`: the first defined one on its dotted chain
    /// (`syntax.keyword.control` → `syntax.keyword` → `syntax`).
    pub fn resolve(&self, name: &str) -> Option<&Token> {
        let mut n = name;
        loop {
            if let Some(t) = self.tokens.get(n) {
                return Some(t);
            }
            n = &n[..n.rfind('.')?];
        }
    }
}

/// A loaded, checked pack.
#[derive(Clone, Debug)]
pub struct Pack {
    /// Pack id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Root node id.
    pub root: String,
    /// Default theme (file stem).
    pub theme: String,
    /// State keys and defaults.
    pub state: BTreeMap<String, StateValue>,
    /// Allowed values for string keys, in order.
    pub state_values: BTreeMap<String, Vec<String>>,
    /// Named conditions (`cond.<name>`).
    pub conditions: Vec<(String, When)>,
    /// Regions by name.
    pub regions: BTreeMap<String, Region>,
    /// Icons by name.
    pub icons: BTreeMap<String, Icon>,
    /// Language labels.
    pub languages: BTreeMap<String, String>,
    /// Message templates.
    pub messages: BTreeMap<String, Template>,
    /// Tab stop.
    pub tab_width: u32,
    /// Draw icons as ASCII.
    pub ascii_icons: bool,
    /// Nodes by id.
    pub nodes: BTreeMap<String, NodeSpec>,
    /// Parent of each node.
    pub parent: BTreeMap<String, String>,
    /// The key layer (`ux:<id>`).
    pub keys: Layer,
    /// Themes by name.
    pub themes: BTreeMap<String, Theme>,
    /// The one surface node (the editor region).
    pub surface: String,
}

impl Pack {
    /// Loads a pack from its source.
    ///
    /// # Errors
    ///
    /// Every problem found, or one if the folder cannot be read.
    pub fn from_source(
        src: &PackSource,
        known: &dyn Fn(&str) -> bool,
    ) -> Result<Self, Vec<PackError>> {
        let files = read_source(src).map_err(|e| vec![e])?;
        Self::load(&files, known)
    }

    /// Loads and checks a pack from its files (path relative to the pack, text).
    /// `known` says whether a command id exists.
    ///
    /// # Errors
    ///
    /// Every problem found.
    pub fn load(
        files: &[(String, String)],
        known: &dyn Fn(&str) -> bool,
    ) -> Result<Self, Vec<PackError>> {
        Loader::new(known).run(files)
    }

    /// Checks a pack against the commands of a default editor server (the editor
    /// commands and the `ui.*` table). For tests and tooling.
    ///
    /// # Errors
    ///
    /// Every problem found.
    pub fn check(files: &[(&str, &str)]) -> Result<(), Vec<PackError>> {
        let ids = default_commands();
        let files: Vec<(String, String)> = files
            .iter()
            .map(|(p, t)| ((*p).to_owned(), (*t).to_owned()))
            .collect();
        Self::load(&files, &|id| ids.contains(id)).map(|_| ())
    }

    /// The theme to use: `want` if given, else the pack's default.
    pub fn theme_named(&self, want: Option<&str>) -> Option<(&str, &Theme)> {
        let name = want.unwrap_or(&self.theme);
        self.themes
            .get_key_value(name)
            .map(|(k, v)| (k.as_str(), v))
    }

    /// True if `node` is `ancestor` or inside it.
    pub fn within(&self, node: &str, ancestor: &str) -> bool {
        let mut n = node;
        loop {
            if n == ancestor {
                return true;
            }
            match self.parent.get(n) {
                Some(p) => n = p,
                None => return false,
            }
        }
    }

    /// The region whose node contains `node`, if any.
    pub fn region_of(&self, node: &str) -> Option<&str> {
        self.regions
            .iter()
            .find(|(_, r)| self.within(node, &r.node))
            .map(|(k, _)| k.as_str())
    }

    /// A message with its placeholders filled.
    pub fn message(&self, id: &str, args: &[(&str, &str)]) -> String {
        self.messages.get(id).map_or_else(
            || id.to_owned(),
            |t| {
                t.render(|k| {
                    args.iter()
                        .find(|(a, _)| *a == k)
                        .map(|(_, v)| (*v).to_owned())
                })
            },
        )
    }
}

/// Command ids a default server knows: the editor commands and `ui.*`.
pub fn default_commands() -> BTreeSet<String> {
    let mut core = spur_core::Core::new(spur_core::editor::Editor::new());
    let _ = spur_core::editor::register_editor_commands(&mut core);
    let mut ids: BTreeSet<String> = core.commands().iter().map(|m| m.id.to_string()).collect();
    ids.extend(super::commands::UI_COMMANDS.iter().map(|s| (*s).to_owned()));
    ids
}

/// Reads a pack's files: `*.toml` at the top and in `components/` and `themes/`,
/// skipping dot-files and symlinks. Other `.toml` files are returned too, so the
/// loader can refuse them.
///
/// # Errors
///
/// A folder that cannot be read, or a file that is not UTF-8.
pub fn read_source(src: &PackSource) -> Result<Vec<(String, String)>, PackError> {
    match src {
        PackSource::Embedded(files) => Ok(files
            .iter()
            .map(|(p, t)| ((*p).to_owned(), (*t).to_owned()))
            .collect()),
        PackSource::Dir(dir) => {
            let mut out = Vec::new();
            read_dir(dir, "", 0, &mut out)?;
            out.sort();
            Ok(out)
        }
    }
}

fn read_dir(
    dir: &Path,
    prefix: &str,
    depth: usize,
    out: &mut Vec<(String, String)>,
) -> Result<(), PackError> {
    let fail = |m: String| PackError {
        file: if prefix.is_empty() {
            dir.display().to_string()
        } else {
            prefix.trim_end_matches('/').to_owned()
        },
        line: None,
        path: String::new(),
        message: m,
    };
    let entries = std::fs::read_dir(dir).map_err(|e| fail(format!("cannot read: {e}")))?;
    for entry in entries {
        let entry = entry.map_err(|e| fail(format!("cannot read: {e}")))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') {
            continue;
        }
        let ft = entry
            .file_type()
            .map_err(|e| fail(format!("cannot read: {e}")))?;
        let rel = format!("{prefix}{name}");
        if ft.is_dir() {
            // Deeper folders are read only to refuse their TOML files.
            if depth < 3 {
                read_dir(&entry.path(), &format!("{rel}/"), depth + 1, out)?;
            }
        } else if ft.is_file() && has_toml_ext(name) {
            let text = std::fs::read_to_string(entry.path()).map_err(|e| PackError {
                file: rel.clone(),
                line: None,
                path: String::new(),
                message: format!("cannot read: {e}"),
            })?;
            out.push((rel, text));
        }
    }
    Ok(())
}

/// One file being read, for spans → lines.
struct Src<'a> {
    file: &'a str,
    starts: Vec<usize>,
}

impl<'a> Src<'a> {
    fn new(file: &'a str, text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self { file, starts }
    }

    fn line(&self, offset: usize) -> u32 {
        let i = self.starts.partition_point(|&s| s <= offset);
        u32::try_from(i).unwrap_or(u32::MAX)
    }

    fn err(&self, span: Option<Range<usize>>, path: &str, msg: impl Into<String>) -> PackError {
        PackError {
            file: self.file.to_owned(),
            line: span.map(|s| self.line(s.start)),
            path: path.to_owned(),
            message: msg.into(),
        }
    }

    fn de<T: DeserializeOwned>(
        &self,
        v: &Spanned<DeValue<'_>>,
        path: &str,
    ) -> Result<T, PackError> {
        T::deserialize(ValueDeserializer::from(v.clone())).map_err(|e| {
            self.err(
                Some(e.span().unwrap_or_else(|| v.span())),
                path,
                e.message(),
            )
        })
    }
}

type Entries<'t, 'i> = Vec<(&'t str, &'t Spanned<DeValue<'i>>)>;

fn entries<'t, 'i>(t: &'t DeTable<'i>) -> Entries<'t, 'i> {
    t.iter().map(|(k, v)| (&**k.get_ref(), v)).collect()
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RegionToml {
    node: String,
    mode: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IconToml {
    glyph: String,
    ascii: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
struct TokenToml {
    fg: Option<String>,
    bg: Option<String>,
    #[serde(default)]
    bold: bool,
    #[serde(default)]
    italic: bool,
    #[serde(default)]
    underline: bool,
    #[serde(default)]
    dim: bool,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ClickToml {
    when: Option<String>,
    command: Option<String>,
    args: Option<Value>,
    #[serde(default)]
    run: Vec<StepToml>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StepToml {
    command: String,
    args: Option<Value>,
}

const SIZE_HELP: &str = "expected a number, \"fill\", \"fit\" or { fill = N }";

struct Loader<'k> {
    known: &'k dyn Fn(&str) -> bool,
    errors: Vec<PackError>,
    /// A node id was defined twice.
    duplicates: bool,
    /// A file did not parse.
    syntax_failed: bool,
}

/// Raw node, before cross-checks.
struct RawNode {
    file: String,
    line: u32,
    spec: NodeSpec,
    /// Text keys and click/when keys to check later: (path, key).
    reads: Vec<(String, String)>,
}

impl<'k> Loader<'k> {
    fn new(known: &'k dyn Fn(&str) -> bool) -> Self {
        Self {
            known,
            errors: Vec::new(),
            duplicates: false,
            syntax_failed: false,
        }
    }

    fn push(&mut self, e: PackError) {
        self.errors.push(e);
    }

    #[allow(clippy::too_many_lines)]
    fn run(mut self, files: &[(String, String)]) -> Result<Pack, Vec<PackError>> {
        let mut pack_toml = None;
        let mut settings = None;
        let mut node_files = Vec::new();
        let mut keys = None;
        let mut theme_files = Vec::new();
        for (path, text) in files {
            match path.as_str() {
                "pack.toml" => pack_toml = Some(text),
                "settings.toml" => settings = Some(text),
                "layout.toml" => node_files.push((path, text)),
                "keys.toml" => keys = Some(text),
                p if is_in(p, "components/") => node_files.push((path, text)),
                p if is_in(p, "themes/") => theme_files.push((path, text)),
                p if Path::new(p)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("toml")) =>
                {
                    self.push(PackError {
                        file: p.to_owned(),
                        line: None,
                        path: String::new(),
                        message: "not a pack file (pack.toml, settings.toml, layout.toml, keys.toml, components/*.toml, themes/*.toml)".into(),
                    });
                }
                _ => {}
            }
        }
        for (name, present) in [
            ("pack.toml", pack_toml.is_some()),
            (
                "layout.toml",
                node_files.iter().any(|(p, _)| *p == "layout.toml"),
            ),
            ("keys.toml", keys.is_some()),
            ("themes/*.toml", !theme_files.is_empty()),
        ] {
            if !present {
                self.push(PackError {
                    file: name.into(),
                    line: None,
                    path: String::new(),
                    message: "missing".into(),
                });
            }
        }
        let mut pack = Pack {
            id: String::new(),
            name: String::new(),
            root: String::new(),
            theme: String::new(),
            state: BTreeMap::new(),
            state_values: BTreeMap::new(),
            conditions: Vec::new(),
            regions: BTreeMap::new(),
            icons: BTreeMap::new(),
            languages: BTreeMap::new(),
            messages: BTreeMap::new(),
            tab_width: 4,
            ascii_icons: false,
            nodes: BTreeMap::new(),
            parent: BTreeMap::new(),
            keys: Layer::new("ux", Source::Core),
            themes: BTreeMap::new(),
            surface: String::new(),
        };
        let mut pack_lines = BTreeMap::new();
        if let Some(text) = pack_toml {
            self.pack_file(text, &mut pack, &mut pack_lines);
        }
        if let Some(text) = settings {
            self.settings_file(text, &mut pack);
        }
        for (path, text) in &theme_files {
            self.theme_file(path, text, &mut pack);
        }
        let mut raw = BTreeMap::new();
        for (path, text) in &node_files {
            self.node_file(path, text, &mut raw);
        }
        if self.syntax_failed {
            // Nodes from that file are missing; checking links now would only list
            // their absence.
            return Err(self.errors);
        }
        self.check_themes(&pack);
        let known_keys = self.check_nodes(&mut pack, raw, &pack_lines);
        if let Some(text) = keys {
            self.keys_file(text, &mut pack, &known_keys);
        }
        if self.errors.is_empty() {
            Ok(pack)
        } else {
            Err(self.errors)
        }
    }

    fn parse<'i>(&mut self, src: &Src<'_>, text: &'i str) -> Option<DeTable<'i>> {
        match DeTable::parse(text) {
            Ok(t) => Some(t.into_inner()),
            Err(e) => {
                self.syntax_failed = true;
                self.push(src.err(e.span(), "", e.message().trim().to_owned()));
                None
            }
        }
    }

    fn table<'t, 'i>(
        &mut self,
        src: &Src<'_>,
        v: &'t Spanned<DeValue<'i>>,
        path: &str,
    ) -> Option<&'t DeTable<'i>> {
        if let Some(t) = v.get_ref().as_table() {
            Some(t)
        } else {
            self.push(src.err(Some(v.span()), path, "expected a table"));
            None
        }
    }

    fn string(&mut self, src: &Src<'_>, v: &Spanned<DeValue<'_>>, path: &str) -> Option<String> {
        if let Some(s) = v.get_ref().as_str() {
            Some(s.to_owned())
        } else {
            self.push(src.err(Some(v.span()), path, "expected a string"));
            None
        }
    }

    fn de<T: DeserializeOwned>(
        &mut self,
        src: &Src<'_>,
        v: &Spanned<DeValue<'_>>,
        path: &str,
    ) -> Option<T> {
        src.de(v, path).map_err(|e| self.push(e)).ok()
    }

    fn when(&mut self, src: &Src<'_>, v: &Spanned<DeValue<'_>>, path: &str) -> Option<When> {
        let s = self.string(src, v, path)?;
        match When::parse(&s) {
            Ok(w) => Some(w),
            Err(e) => {
                self.push(src.err(Some(v.span()), path, format!("bad clause: {e}")));
                None
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn pack_file(&mut self, text: &str, pack: &mut Pack, lines: &mut BTreeMap<String, u32>) {
        let src = Src::new("pack.toml", text);
        let Some(doc) = self.parse(&src, text) else {
            return;
        };
        let mut seen = BTreeSet::new();
        for (key, v) in entries(&doc) {
            seen.insert(key);
            lines.insert(key.to_owned(), src.line(v.span().start));
            match key {
                "id" => {
                    if let Some(s) = self.string(&src, v, key) {
                        if is_ident(&s) {
                            pack.id = s;
                        } else {
                            self.push(src.err(Some(v.span()), key, "use [a-z][a-z0-9_]*"));
                        }
                    }
                }
                "name" => pack.name = self.string(&src, v, key).unwrap_or_default(),
                "root" => pack.root = self.string(&src, v, key).unwrap_or_default(),
                "theme" => pack.theme = self.string(&src, v, key).unwrap_or_default(),
                "format" => {
                    if let Some(n) = self.de::<i64>(&src, v, key)
                        && n != FORMAT
                    {
                        self.push(src.err(
                            Some(v.span()),
                            key,
                            format!("format {n} is not supported (this server reads {FORMAT})"),
                        ));
                    }
                }
                "state" => {
                    let Some(t) = self.table(&src, v, key) else {
                        continue;
                    };
                    for (k, v) in entries(t) {
                        let path = format!("state.{k}");
                        if !k.starts_with("ui.") || check_name(k, 2).is_err() {
                            self.push(src.err(
                                Some(v.span()),
                                &path,
                                "state keys are names starting with ui.",
                            ));
                            continue;
                        }
                        if k == "ui.focus" || k == "ui.notice" {
                            self.push(src.err(
                                Some(v.span()),
                                &path,
                                "built in; choose another name",
                            ));
                            continue;
                        }
                        let val = match v.get_ref() {
                            DeValue::String(s) => StateValue::Str(s.to_string()),
                            DeValue::Boolean(b) => StateValue::Bool(*b),
                            DeValue::Integer(_) => match self.de::<i64>(&src, v, &path) {
                                Some(i) => StateValue::Int(i),
                                None => continue,
                            },
                            _ => {
                                self.push(src.err(
                                    Some(v.span()),
                                    &path,
                                    "expected a string, boolean or integer",
                                ));
                                continue;
                            }
                        };
                        pack.state.insert(k.to_owned(), val);
                    }
                }
                "state_values" => {
                    let Some(t) = self.table(&src, v, key) else {
                        continue;
                    };
                    for (k, v) in entries(t) {
                        let path = format!("state_values.{k}");
                        if let Some(list) = self.de::<Vec<String>>(&src, v, &path) {
                            pack.state_values.insert(k.to_owned(), list);
                            lines.insert(path, src.line(v.span().start));
                        }
                    }
                }
                "conditions" => {
                    let Some(t) = self.table(&src, v, key) else {
                        continue;
                    };
                    for (k, v) in entries(t) {
                        let path = format!("conditions.{k}");
                        if !is_ident(k) {
                            self.push(src.err(Some(v.span()), &path, "use [a-z][a-z0-9_]*"));
                            continue;
                        }
                        if let Some(w) = self.when(&src, v, &path) {
                            lines.insert(path, src.line(v.span().start));
                            pack.conditions.push((k.to_owned(), w));
                        }
                    }
                }
                "regions" => {
                    let Some(t) = self.table(&src, v, key) else {
                        continue;
                    };
                    for (k, v) in entries(t) {
                        let path = format!("regions.{k}");
                        if !is_ident(k) {
                            self.push(src.err(Some(v.span()), &path, "use [a-z][a-z0-9_]*"));
                            continue;
                        }
                        if let Some(r) = self.de::<RegionToml>(&src, v, &path) {
                            lines.insert(path, src.line(v.span().start));
                            pack.regions.insert(
                                k.to_owned(),
                                Region {
                                    node: r.node,
                                    mode: r.mode,
                                },
                            );
                        }
                    }
                }
                "icons" => {
                    let Some(t) = self.table(&src, v, key) else {
                        continue;
                    };
                    for (k, v) in entries(t) {
                        let path = format!("icons.{k}");
                        let Some(i) = self.de::<IconToml>(&src, v, &path) else {
                            continue;
                        };
                        if i.glyph.width() != 1 || i.glyph.chars().any(char::is_control) {
                            self.push(src.err(
                                Some(v.span()),
                                &format!("{path}.glyph"),
                                "must be one cell wide",
                            ));
                        }
                        let mut a = i.ascii.chars();
                        if !matches!((a.next(), a.next()), (Some(c), None) if (' '..='~').contains(&c))
                        {
                            self.push(src.err(
                                Some(v.span()),
                                &format!("{path}.ascii"),
                                "must be one printable ASCII char",
                            ));
                        }
                        pack.icons.insert(
                            k.to_owned(),
                            Icon {
                                glyph: i.glyph,
                                ascii: i.ascii,
                            },
                        );
                    }
                }
                "languages" => {
                    let Some(t) = self.table(&src, v, key) else {
                        continue;
                    };
                    for (k, v) in entries(t) {
                        let path = format!("languages.{k}");
                        if !lang::IDS.contains(&k) {
                            self.push(src.err(
                                Some(v.span()),
                                &path,
                                format!("unknown language id (known: {})", lang::IDS.join(", ")),
                            ));
                            continue;
                        }
                        if let Some(s) = self.string(&src, v, &path) {
                            pack.languages.insert(k.to_owned(), s);
                        }
                    }
                }
                "messages" => {
                    let Some(t) = self.table(&src, v, key) else {
                        continue;
                    };
                    for (k, v) in entries(t) {
                        let path = format!("messages.{k}");
                        let Some((_, allowed)) = MESSAGES.iter().find(|(id, _)| *id == k) else {
                            self.push(src.err(Some(v.span()), &path, "unknown message id"));
                            continue;
                        };
                        let Some(s) = self.string(&src, v, &path) else {
                            continue;
                        };
                        match Template::parse(&s) {
                            Ok(t) => {
                                for key in t.keys() {
                                    if !allowed.contains(&key) {
                                        self.push(src.err(
                                            Some(v.span()),
                                            &path,
                                            format!(
                                                "unknown placeholder {{{key}}} (allowed: {})",
                                                if allowed.is_empty() {
                                                    "none".to_owned()
                                                } else {
                                                    allowed.join(", ")
                                                }
                                            ),
                                        ));
                                    }
                                }
                                pack.messages.insert(k.to_owned(), t);
                            }
                            Err(e) => self.push(src.err(Some(v.span()), &path, e)),
                        }
                    }
                }
                other => self.push(src.err(Some(v.span()), other, "unknown key")),
            }
        }
        for req in ["id", "format", "root", "theme"] {
            if !seen.contains(req) {
                self.push(src.err(None, req, "missing"));
            }
        }
        for (id, _) in MESSAGES {
            if !pack.messages.contains_key(*id) && seen.contains("messages") {
                self.push(src.err(None, &format!("messages.{id}"), "missing"));
            }
        }
        if !seen.contains("messages") {
            self.push(src.err(None, "messages", "missing"));
        }
        if !seen.contains("regions") {
            self.push(src.err(None, "regions", "missing"));
        }
        for i in REQUIRED_ICONS {
            if !pack.icons.contains_key(*i) {
                self.push(src.err(
                    None,
                    &format!("icons.{i}"),
                    "missing (the Files list uses it)",
                ));
            }
        }
        for (k, values) in &pack.state_values {
            let path = format!("state_values.{k}");
            let line = lines.get(&path).copied();
            let e = |m: &str| PackError {
                file: "pack.toml".into(),
                line,
                path: path.clone(),
                message: m.into(),
            };
            match pack.state.get(k) {
                Some(StateValue::Str(d)) => {
                    if values.is_empty() {
                        self.push(e("must not be empty"));
                    } else if !values.contains(d) {
                        self.push(e(&format!("the default {d:?} is not in the list")));
                    }
                    let unique: BTreeSet<_> = values.iter().collect();
                    if unique.len() != values.len() {
                        self.push(e("values repeat"));
                    }
                }
                Some(_) => self.push(e("only string state keys take a list of values")),
                None => self.push(e("not a declared state key")),
            }
        }
    }

    fn settings_file(&mut self, text: &str, pack: &mut Pack) {
        let src = Src::new("settings.toml", text);
        let Some(doc) = self.parse(&src, text) else {
            return;
        };
        for (key, v) in entries(&doc) {
            match key {
                "tab_width" => {
                    if let Some(n) = self.de::<u32>(&src, v, key) {
                        if (1..=16).contains(&n) {
                            pack.tab_width = n;
                        } else {
                            self.push(src.err(Some(v.span()), key, "must be 1 to 16"));
                        }
                    }
                }
                "icons" => match self.string(&src, v, key).as_deref() {
                    Some("unicode") => pack.ascii_icons = false,
                    Some("ascii") => pack.ascii_icons = true,
                    Some(_) => {
                        self.push(src.err(
                            Some(v.span()),
                            key,
                            "expected \"unicode\" or \"ascii\"",
                        ));
                    }
                    None => {}
                },
                other => self.push(src.err(Some(v.span()), other, "unknown key")),
            }
        }
    }

    fn theme_file(&mut self, path: &str, text: &str, pack: &mut Pack) {
        let src = Src::new(path, text);
        let stem = path
            .strip_prefix("themes/")
            .and_then(|p| p.strip_suffix(".toml"))
            .unwrap_or_default();
        if !is_ident(stem) {
            self.push(src.err(None, "", "theme file names use [a-z][a-z0-9_]*"));
            return;
        }
        let Some(doc) = self.parse(&src, text) else {
            return;
        };
        let mut dark = false;
        let mut palette = BTreeMap::new();
        let mut raw_tokens = Vec::new();
        for (key, v) in entries(&doc) {
            match key {
                "dark" => dark = self.de::<bool>(&src, v, key).unwrap_or(false),
                "palette" => {
                    let Some(t) = self.table(&src, v, key) else {
                        continue;
                    };
                    for (k, v) in entries(t) {
                        let path = format!("palette.{k}");
                        if let Some(s) = self.string(&src, v, &path) {
                            if is_hex(&s) {
                                palette.insert(k.to_owned(), s.to_ascii_lowercase());
                            } else {
                                self.push(src.err(Some(v.span()), &path, "expected #rrggbb"));
                            }
                        }
                    }
                }
                "tokens" => {
                    let Some(t) = self.table(&src, v, key) else {
                        continue;
                    };
                    for (k, v) in entries(t) {
                        raw_tokens.push((k.to_owned(), v.clone()));
                    }
                }
                other => self.push(src.err(Some(v.span()), other, "unknown key")),
            }
        }
        let mut tokens = BTreeMap::new();
        for (name, v) in raw_tokens {
            let path = format!("tokens.{name}");
            if check_name(&name, 1).is_err() {
                self.push(src.err(Some(v.span()), &path, "bad token name"));
                continue;
            }
            let Some(t) = self.de::<TokenToml>(&src, &v, &path) else {
                continue;
            };
            let mut colour = |c: Option<String>, field: &str| -> Option<String> {
                let c = c?;
                if is_hex(&c) {
                    return Some(c.to_ascii_lowercase());
                }
                if let Some(p) = palette.get(&c) {
                    return Some(p.clone());
                }
                self.errors.push(src.err(
                    Some(v.span()),
                    &format!("{path}.{field}"),
                    format!("{c:?} is not #rrggbb or a palette name"),
                ));
                None
            };
            let fg = colour(t.fg, "fg");
            let bg = colour(t.bg, "bg");
            tokens.insert(
                name,
                Token {
                    fg,
                    bg,
                    bold: t.bold,
                    italic: t.italic,
                    underline: t.underline,
                    dim: t.dim,
                },
            );
        }
        pack.themes.insert(stem.to_owned(), Theme { dark, tokens });
    }

    fn check_themes(&mut self, pack: &Pack) {
        if !pack.theme.is_empty() && !pack.themes.contains_key(&pack.theme) {
            self.push(PackError {
                file: "pack.toml".into(),
                line: None,
                path: "theme".into(),
                message: format!("no theme file themes/{}.toml", pack.theme),
            });
        }
        let Some((first, base)) = pack.themes.iter().next() else {
            return;
        };
        for (name, theme) in &pack.themes {
            let file = format!("themes/{name}.toml");
            for fixed in tokens::ALL {
                if theme.resolve(fixed).is_none() {
                    self.push(PackError {
                        file: file.clone(),
                        line: None,
                        path: format!("tokens.{fixed}"),
                        message: "missing (the protocol uses it)".into(),
                    });
                }
            }
            let a: BTreeSet<_> = theme.tokens.keys().collect();
            let b: BTreeSet<_> = base.tokens.keys().collect();
            for missing in b.difference(&a) {
                self.push(PackError {
                    file: file.clone(),
                    line: None,
                    path: format!("tokens.{missing}"),
                    message: format!("missing (themes/{first}.toml defines it)"),
                });
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn node_file(&mut self, path: &str, text: &str, raw: &mut BTreeMap<String, RawNode>) {
        let src = Src::new(path, text);
        let Some(doc) = self.parse(&src, text) else {
            return;
        };
        for (key, v) in entries(&doc) {
            if key != "node" {
                self.push(src.err(Some(v.span()), key, "unknown key (nodes go in [node.<id>])"));
                continue;
            }
            let Some(t) = self.table(&src, v, "node") else {
                continue;
            };
            for (id, v) in entries(t) {
                let npath = format!("node.{id}");
                if !is_ident(id) {
                    self.push(src.err(Some(v.span()), &npath, "node ids use [a-z][a-z0-9_]*"));
                    continue;
                }
                if let Some(prev) = raw.get(id) {
                    self.duplicates = true;
                    self.push(src.err(
                        Some(v.span()),
                        &npath,
                        format!("already defined in {}:{}", prev.file, prev.line),
                    ));
                    continue;
                }
                let Some(fields) = self.table(&src, v, &npath) else {
                    continue;
                };
                if let Some(node) = self.node(&src, &npath, v.span(), fields) {
                    raw.insert(id.to_owned(), node);
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn node(
        &mut self,
        src: &Src<'_>,
        npath: &str,
        span: Range<usize>,
        fields: &DeTable<'_>,
    ) -> Option<RawNode> {
        let f = entries(fields);
        let get = |k: &str| f.iter().find(|(n, _)| *n == k).map(|(_, v)| *v);
        let Some(kv) = get("kind") else {
            self.push(src.err(Some(span), &format!("{npath}.kind"), "missing"));
            return None;
        };
        let kind = match self.string(src, kv, &format!("{npath}.kind"))?.as_str() {
            "row" => NodeKind::Row,
            "column" => NodeKind::Column,
            "card" => NodeKind::Card,
            "spacer" => NodeKind::Spacer,
            "text" => NodeKind::Text,
            "icon" => NodeKind::Icon,
            "list" => NodeKind::List,
            "surface" => NodeKind::Surface,
            other => {
                self.push(src.err(
                    Some(kv.span()),
                    &format!("{npath}.kind"),
                    format!("unknown kind {other:?} (row, column, card, spacer, text, icon, list, surface)"),
                ));
                return None;
            }
        };
        let container = matches!(kind, NodeKind::Row | NodeKind::Column | NodeKind::Card);
        let fit_default = matches!(kind, NodeKind::Text | NodeKind::Icon);
        let mut spec = NodeSpec {
            kind,
            children: Vec::new(),
            width: if fit_default { Dim::Fit } else { Dim::FILL },
            height: if fit_default { Dim::Fit } else { Dim::FILL },
            when: None,
            style: None,
            style_active: None,
            active: None,
            padding: None,
            gap: 0,
            text: None,
            icon: None,
            on_click: Vec::new(),
            source: None,
            gutter: false,
            current_line: false,
        };
        let mut reads = Vec::new();
        for (k, v) in &f {
            let path = format!("{npath}.{k}");
            let allowed = match *k {
                "kind" | "width" | "height" | "when" | "style" | "style_active" | "active" => true,
                "children" | "padding" | "gap" => container,
                "text" => kind == NodeKind::Text,
                "icon" => kind == NodeKind::Icon,
                "source" => kind == NodeKind::List,
                "gutter" | "current_line" => kind == NodeKind::Surface,
                "on_click" => !matches!(kind, NodeKind::List | NodeKind::Surface),
                _ => false,
            };
            if !allowed {
                self.push(src.err(Some(v.span()), &path, "unknown key for this kind"));
                continue;
            }
            match *k {
                "width" | "height" => {
                    let Some(d) = self.size(src, v, &path) else {
                        continue;
                    };
                    if d == Dim::Fit && matches!(kind, NodeKind::List | NodeKind::Surface) {
                        self.push(src.err(
                            Some(v.span()),
                            &path,
                            "a list or surface cannot be \"fit\"",
                        ));
                        continue;
                    }
                    if *k == "width" {
                        spec.width = d;
                    } else {
                        spec.height = d;
                    }
                }
                "when" => {
                    spec.when = self.when(src, v, &path);
                    if let Some(w) = &spec.when {
                        reads.extend(
                            w.keys()
                                .into_iter()
                                .map(|key| (path.clone(), key.to_owned())),
                        );
                    }
                }
                "active" => {
                    spec.active = self.when(src, v, &path);
                    if let Some(w) = &spec.active {
                        reads.extend(
                            w.keys()
                                .into_iter()
                                .map(|key| (path.clone(), key.to_owned())),
                        );
                    }
                }
                "style" => spec.style = self.string(src, v, &path),
                "style_active" => spec.style_active = self.string(src, v, &path),
                "children" => spec.children = self.de(src, v, &path).unwrap_or_default(),
                "padding" => spec.padding = self.de(src, v, &path),
                "gap" => spec.gap = self.de(src, v, &path).unwrap_or(0),
                "text" => {
                    let s = self.string(src, v, &path)?;
                    match Template::parse(&s) {
                        Ok(t) => {
                            reads.extend(t.keys().map(|key| (path.clone(), key.to_owned())));
                            spec.text = Some(t);
                        }
                        Err(e) => self.push(src.err(Some(v.span()), &path, e)),
                    }
                }
                "icon" => spec.icon = self.string(src, v, &path),
                "source" => spec.source = self.string(src, v, &path),
                "gutter" => spec.gutter = self.de(src, v, &path).unwrap_or(false),
                "current_line" => spec.current_line = self.de(src, v, &path).unwrap_or(false),
                "on_click" => spec.on_click = self.on_click(src, v, &path, &mut reads),
                _ => {}
            }
        }
        if spec.style_active.is_some() != spec.active.is_some() {
            self.push(src.err(
                Some(span.clone()),
                npath,
                "style_active and active go together",
            ));
        }
        if kind == NodeKind::Text && spec.text.is_none() {
            self.push(src.err(Some(span.clone()), &format!("{npath}.text"), "missing"));
        }
        if kind == NodeKind::Icon && spec.icon.is_none() {
            self.push(src.err(Some(span.clone()), &format!("{npath}.icon"), "missing"));
        }
        if kind == NodeKind::List && spec.source.is_none() {
            self.push(src.err(Some(span.clone()), &format!("{npath}.source"), "missing"));
        }
        Some(RawNode {
            file: src.file.to_owned(),
            line: src.line(span.start),
            spec,
            reads,
        })
    }

    fn size(&mut self, src: &Src<'_>, v: &Spanned<DeValue<'_>>, path: &str) -> Option<Dim> {
        let d = match v.get_ref() {
            DeValue::Integer(_) => src.de::<u32>(v, path).ok().map(|n| Dim::Units { n }),
            DeValue::String(s) if s == "fill" => Some(Dim::FILL),
            DeValue::String(s) if s == "fit" => Some(Dim::Fit),
            DeValue::Table(t) if t.len() == 1 => t
                .iter()
                .next()
                .filter(|(k, _)| k.get_ref() == "fill")
                .and_then(|(_, w)| src.de::<u32>(w, path).ok())
                .filter(|w| *w > 0)
                .map(|weight| Dim::Fill { weight }),
            _ => None,
        };
        if d.is_none() {
            self.push(src.err(Some(v.span()), path, SIZE_HELP));
        }
        d
    }

    fn on_click(
        &mut self,
        src: &Src<'_>,
        v: &Spanned<DeValue<'_>>,
        path: &str,
        reads: &mut Vec<(String, String)>,
    ) -> Vec<ClickEntry> {
        let Some(arr) = v.get_ref().as_array() else {
            self.push(src.err(
                Some(v.span()),
                path,
                "expected a list of { when, command, args } or { when, run }",
            ));
            return Vec::new();
        };
        let mut out = Vec::new();
        for (i, e) in arr.iter().enumerate() {
            let epath = format!("{path}[{i}]");
            let Some(entry) = self.de::<ClickToml>(src, e, &epath) else {
                continue;
            };
            let when = match entry.when {
                Some(w) => match When::parse(&w) {
                    Ok(w) => {
                        reads.extend(w.keys().into_iter().map(|k| (epath.clone(), k.to_owned())));
                        Some(w)
                    }
                    Err(err) => {
                        self.push(src.err(
                            Some(e.span()),
                            &format!("{epath}.when"),
                            format!("bad clause: {err}"),
                        ));
                        continue;
                    }
                },
                None => None,
            };
            let raw: Vec<(String, Option<Value>)> = match (entry.command, entry.run.is_empty()) {
                (Some(c), true) => vec![(c, entry.args)],
                (None, false) if entry.args.is_none() => {
                    entry.run.into_iter().map(|s| (s.command, s.args)).collect()
                }
                _ => {
                    self.push(src.err(
                        Some(e.span()),
                        &epath,
                        "give either command (with optional args) or run",
                    ));
                    continue;
                }
            };
            let mut steps = Vec::new();
            for (c, args) in raw {
                match CommandId::new(&c) {
                    Ok(id) if (self.known)(&c) => steps.push(Step {
                        command: id,
                        args: Arc::new(args.unwrap_or(Value::Null)),
                    }),
                    _ => {
                        self.push(src.err(
                            Some(e.span()),
                            &epath,
                            format!("unknown command {c:?}"),
                        ));
                    }
                }
            }
            out.push(ClickEntry { when, steps });
        }
        out
    }

    /// Checks nodes, regions and conditions against each other; fills `pack.nodes`.
    /// Returns the context keys clauses and templates may read.
    #[allow(clippy::too_many_lines)]
    fn check_nodes(
        &mut self,
        pack: &mut Pack,
        raw: BTreeMap<String, RawNode>,
        lines: &BTreeMap<String, u32>,
    ) -> BTreeSet<String> {
        let at = |key: &str| lines.get(key).copied();
        let perr = |path: &str, line: Option<u32>, m: String| PackError {
            file: "pack.toml".into(),
            line,
            path: path.to_owned(),
            message: m,
        };
        // Keys clauses may read.
        let mut keys: BTreeSet<String> = BUILTIN_KEYS.iter().map(|s| (*s).to_owned()).collect();
        keys.extend(VIEW_KEYS.iter().map(|s| (*s).to_owned()));
        keys.extend(pack.state.keys().cloned());
        for (name, w) in &pack.conditions {
            let path = format!("conditions.{name}");
            for k in w.keys() {
                if k.starts_with("cond.") {
                    self.push(perr(
                        &path,
                        at(&path),
                        format!("{k}: a condition cannot read another condition"),
                    ));
                } else if !keys.contains(k) {
                    self.push(perr(&path, at(&path), format!("unknown key {k}")));
                }
            }
        }
        keys.extend(pack.conditions.iter().map(|(n, _)| format!("cond.{n}")));
        // Parents, reachability. Broken links would make most nodes look unreachable,
        // so reachability is checked only when every link is good.
        let before = self.errors.len();
        for (id, n) in &raw {
            for c in &n.spec.children {
                let e = |m: String| PackError {
                    file: n.file.clone(),
                    line: Some(n.line),
                    path: format!("node.{id}.children"),
                    message: m,
                };
                if !raw.contains_key(c) {
                    self.push(e(format!("no node {c:?}")));
                } else if let Some(p) = pack.parent.get(c) {
                    self.push(e(format!("{c:?} is already a child of {p:?}")));
                } else {
                    pack.parent.insert(c.clone(), id.clone());
                }
            }
        }
        let links_ok = self.errors.len() == before && !self.duplicates;
        if !pack.root.is_empty() {
            if !raw.contains_key(&pack.root) {
                self.push(perr("root", at("root"), format!("no node {:?}", pack.root)));
            } else if pack.parent.contains_key(&pack.root) {
                self.push(perr(
                    "root",
                    at("root"),
                    "the root cannot be a child".into(),
                ));
            } else {
                let mut seen = BTreeSet::new();
                let mut stack = vec![pack.root.clone()];
                while let Some(n) = stack.pop() {
                    if !seen.insert(n.clone()) {
                        continue;
                    }
                    if let Some(r) = raw.get(&n) {
                        stack.extend(r.spec.children.iter().cloned());
                    }
                }
                for (id, n) in raw.iter().filter(|_| links_ok) {
                    if !seen.contains(id) {
                        self.push(PackError {
                            file: n.file.clone(),
                            line: Some(n.line),
                            path: format!("node.{id}"),
                            message: "not reachable from the root (or part of a cycle)".into(),
                        });
                    }
                }
            }
        }
        // Per node: styles, icons, sources, keys read.
        let style_names: BTreeSet<&String> = pack
            .themes
            .get(&pack.theme)
            .or_else(|| pack.themes.values().next())
            .map(|t| t.tokens.keys().collect())
            .unwrap_or_default();
        for (id, n) in &raw {
            let e = |path: String, m: String| PackError {
                file: n.file.clone(),
                line: Some(n.line),
                path,
                message: m,
            };
            for (f, s) in [
                ("style", &n.spec.style),
                ("style_active", &n.spec.style_active),
            ] {
                if let Some(s) = s
                    && !pack.themes.is_empty()
                    && !style_names.contains(s)
                {
                    self.push(e(
                        format!("node.{id}.{f}"),
                        format!("no token {s:?} in the themes"),
                    ));
                }
            }
            if let Some(i) = &n.spec.icon
                && !pack.icons.contains_key(i)
            {
                self.push(e(
                    format!("node.{id}.icon"),
                    format!("no icon {i:?} in pack.toml"),
                ));
            }
            if let Some(s) = &n.spec.source
                && s != "workspace.files"
            {
                self.push(e(
                    format!("node.{id}.source"),
                    format!("unknown source {s:?} (workspace.files)"),
                ));
            }
            for (path, k) in &n.reads {
                if !keys.contains(k) {
                    self.push(e(path.clone(), format!("unknown key {k}")));
                }
            }
        }
        // Surface and regions.
        let surfaces: Vec<&String> = raw
            .iter()
            .filter(|(_, n)| n.spec.kind == NodeKind::Surface)
            .map(|(k, _)| k)
            .collect();
        if surfaces.len() != 1 {
            self.push(perr(
                "",
                None,
                format!(
                    "a pack has exactly one surface node; found {}",
                    surfaces.len()
                ),
            ));
        }
        match pack.regions.get("editor") {
            None => {
                if lines.contains_key("regions") {
                    self.push(perr("regions.editor", at("regions"), "missing".into()));
                }
            }
            Some(r) => {
                let path = "regions.editor";
                if raw.get(&r.node).map(|n| n.spec.kind) != Some(NodeKind::Surface) {
                    self.push(perr(
                        path,
                        at(path),
                        format!("{:?} is not the surface node", r.node),
                    ));
                }
                if r.mode.is_some() {
                    self.push(perr(
                        path,
                        at(path),
                        "the editor uses the view's own mode; remove mode".into(),
                    ));
                }
                pack.surface.clone_from(&r.node);
                // The surface and its ancestors are always shown.
                let mut n = Some(r.node.clone());
                while let Some(id) = n {
                    if let Some(node) = raw.get(&id)
                        && node.spec.when.is_some()
                    {
                        self.push(PackError {
                            file: node.file.clone(),
                            line: Some(node.line),
                            path: format!("node.{id}.when"),
                            message: "the surface and the nodes around it are always shown".into(),
                        });
                    }
                    n = pack.parent.get(&id).cloned();
                }
            }
        }
        pack.nodes = raw.into_iter().map(|(k, n)| (k, n.spec)).collect();
        for (name, r) in &pack.regions {
            let path = format!("regions.{name}");
            if !pack.nodes.contains_key(&r.node) {
                self.push(perr(&path, at(&path), format!("no node {:?}", r.node)));
                continue;
            }
            if name != "editor" && r.mode.as_deref().is_none_or(str::is_empty) {
                self.push(perr(
                    &path,
                    at(&path),
                    "give the key mode used while it has focus".into(),
                ));
            }
            for (other, o) in &pack.regions {
                if other != name && o.node != r.node && pack.within(&o.node, &r.node) {
                    self.push(perr(&path, at(&path), format!("contains region {other}")));
                }
                if other < name && o.node == r.node {
                    self.push(perr(
                        &path,
                        at(&path),
                        format!("same node as region {other}"),
                    ));
                }
            }
        }
        keys
    }

    fn keys_file(&mut self, text: &str, pack: &mut Pack, keys: &BTreeSet<String>) {
        let src = Src::new("keys.toml", text);
        let name = format!("ux:{}", pack.id);
        match Layer::from_toml(
            &name,
            Source::Extension {
                id: pack.id.as_str().into(),
            },
            text,
        ) {
            Ok(layer) => pack.keys = layer,
            Err(e) => {
                self.push(src.err(None, "", e.to_string()));
                return;
            }
        }
        // Commands and keys, with lines, from the file itself.
        let Some(doc) = self.parse(&src, text) else {
            return;
        };
        let check_cmd = |me: &mut Self, c: &str, span: Range<usize>, path: &str| {
            if !(me.known)(c) {
                me.push(src.err(Some(span), path, format!("unknown command {c:?}")));
            }
        };
        for (key, v) in entries(&doc) {
            match key {
                "bind" => {
                    let Some(arr) = v.get_ref().as_array() else {
                        continue;
                    };
                    for (i, b) in arr.iter().enumerate() {
                        let Some(t) = b.get_ref().as_table() else {
                            continue;
                        };
                        let f = entries(t);
                        let k = f
                            .iter()
                            .find(|(n, _)| *n == "keys")
                            .and_then(|(_, v)| v.get_ref().as_str())
                            .unwrap_or("?")
                            .to_owned();
                        let path = format!("bind[{i}] ({k})");
                        for (n, v) in &f {
                            match *n {
                                "command" => {
                                    if let Some(c) = v.get_ref().as_str() {
                                        check_cmd(self, c, v.span(), &path);
                                    }
                                }
                                "run" => {
                                    for s in v.get_ref().as_array().into_iter().flatten() {
                                        if let Some(c) = s
                                            .get_ref()
                                            .as_table()
                                            .and_then(|t| t.get("command"))
                                            .and_then(|c| c.get_ref().as_str())
                                        {
                                            check_cmd(self, c, s.span(), &path);
                                        }
                                    }
                                }
                                "when" => {
                                    if let Some(w) =
                                        v.get_ref().as_str().and_then(|s| When::parse(s).ok())
                                    {
                                        for key in w.keys() {
                                            if !keys.contains(key) {
                                                self.push(src.err(
                                                    Some(v.span()),
                                                    &path,
                                                    format!("unknown key {key}"),
                                                ));
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                "mode" => {
                    for (m, opts) in v.get_ref().as_table().map(entries).unwrap_or_default() {
                        if let Some(c) = opts
                            .get_ref()
                            .as_table()
                            .and_then(|t| t.get("fallback"))
                            .and_then(|c| c.get_ref().as_str())
                        {
                            check_cmd(self, c, opts.span(), &format!("mode.{m}.fallback"));
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn is_in(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir)
        .is_some_and(|rest| !rest.contains('/') && has_toml_ext(rest))
}

fn has_toml_ext(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
}

fn is_ident(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_lowercase())
        && c.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn is_hex(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
#[path = "pack_tests.rs"]
mod tests;
