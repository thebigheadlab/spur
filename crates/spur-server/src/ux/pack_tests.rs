use super::*;

fn fresh() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/ux_fresh");
    read_source(&PackSource::Dir(dir)).unwrap()
}

fn load(files: &[(String, String)]) -> Result<Pack, Vec<PackError>> {
    let ids = default_commands();
    Pack::load(files, &|id| ids.contains(id))
}

/// `ux_fresh` with `file` edited by `f`; the errors as strings.
fn broken(file: &str, f: impl FnOnce(&str) -> String) -> Vec<String> {
    let mut files = fresh();
    let entry = files.iter_mut().find(|(p, _)| p == file).unwrap();
    entry.1 = f(&entry.1);
    load(&files)
        .map(|_| panic!("{file}: the broken pack loaded"))
        .unwrap_err()
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn has(errors: &[String], want: &str) {
    assert!(
        errors.iter().any(|e| e.contains(want)),
        "no error containing {want:?} in {errors:#?}"
    );
}

#[test]
fn ux_fresh_loads() {
    let pack = load(&fresh()).unwrap_or_else(|e| panic!("{e:#?}"));
    assert_eq!(pack.id, "ux_fresh");
    assert_eq!(pack.surface, "code");
    assert_eq!(pack.themes.len(), 4);
    assert_eq!(pack.regions.len(), 3);
    assert_eq!(pack.conditions.len(), 4);
    assert!(pack.within("files_list", "sidebar"));
    assert_eq!(pack.region_of("files_list"), Some("sidebar"));
    assert_eq!(pack.region_of("rail_files"), None);
    assert_eq!(&*pack.keys.name, "ux:ux_fresh");
    let g = &pack.themes["graphite"];
    assert_eq!(g.resolve("app").unwrap().bg.as_deref(), Some("#111215"));
    assert_eq!(
        g.resolve("syntax.keyword.control").unwrap(),
        g.resolve("syntax.keyword").unwrap()
    );
    assert_eq!(
        pack.message("quit.unsaved", &[("doc", "a.rs")]),
        "Unsaved changes in a.rs. Press ctrl+q again to quit."
    );
}

#[test]
fn check_uses_the_default_commands() {
    let files = fresh();
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    Pack::check(&refs).unwrap();
}

#[test]
fn errors_name_file_line_and_field() {
    let e = broken("components/sidebar.toml", |t| {
        t.replace("width = 28", "width = \"wide\"")
    });
    assert_eq!(
        e,
        [
            "components/sidebar.toml:8: node.sidebar.width: expected a number, \"fill\", \"fit\" or { fill = N }"
        ]
    );
    let e = broken("layout.toml", |t| {
        t.replace("[node.body]\nkind", "[node.body]\nkidn = 1\nkind")
    });
    has(
        &e,
        "layout.toml:11: node.body.kidn: unknown key for this kind",
    );
    let e = broken("pack.toml", |t| format!("colour = 1\n{t}"));
    has(&e, "pack.toml:");
    has(&e, "pack.toml:1: colour: unknown key");
    let e = broken("pack.toml", |t| t.replace("format = 0", "format = 1"));
    has(&e, "format 1 is not supported");
    let e = broken("components/topbar.toml", |t| {
        t.replace(
            "[node.brand]\nkind = \"text\"\ntext = \"spur\"",
            "[node.brand]\nkind = \"text\"\ntext = \"spur {\"",
        )
    });
    has(&e, "node.brand.text: unclosed {");
}

#[test]
fn syntax_errors_stop_one_file() {
    let e = broken("components/code.toml", |t| format!("{t}\n[node.x\n"));
    assert!(e[0].starts_with("components/code.toml:"), "{e:?}");
}

#[test]
fn references_are_checked() {
    let e = broken("components/code.toml", |t| {
        t.replace("style = \"title\"", "style = \"titel\"")
    });
    has(
        &e,
        "node.code_title.style: no token \"titel\" in the themes",
    );
    let e = broken("components/code.toml", |t| {
        t.replace("icon = \"modified\"", "icon = \"dot\"")
    });
    has(&e, "node.code_modified.icon: no icon \"dot\"");
    let e = broken("components/topbar.toml", |t| {
        t.replace("{workspace.name}", "{workspace.nmae}")
    });
    has(&e, "node.workspace.text: unknown key workspace.nmae");
    let e = broken("components/code.toml", |t| {
        t.replace("when = \"buffer.modified\"", "when = \"buffer.modifed\"")
    });
    has(&e, "node.code_modified.when: unknown key buffer.modifed");
    let e = broken("components/rail.toml", |t| {
        t.replacen("ui.focus\"", "ui.fcous\"", 1)
    });
    has(
        &e,
        "node.rail_files.on_click[0]: unknown command \"ui.fcous\"",
    );
    let e = broken("components/rail.toml", |t| {
        t.replacen(
            "{ when = \"ui.sidebar == files && cond.sidebar_shown\", command",
            "{ when = \"x\", run = [], command",
            1,
        )
    });
    has(&e, "unknown key x");
    let e = broken("keys.toml", |t| {
        t.replace("command = \"buffer.save\"", "command = \"buffer.sav\"")
    });
    has(&e, "keys.toml:");
    has(&e, "(<C-s>): unknown command \"buffer.sav\"");
    let e = broken("keys.toml", |t| {
        t.replace(
            "when = \"cond.sidebar_shown\"",
            "when = \"cond.sidebar_show\"",
        )
    });
    has(&e, "(<C-b>): unknown key cond.sidebar_show");
    let e = broken("keys.toml", |t| {
        t.replace(
            "fallback = \"editor.insert_text\"",
            "fallback = \"editor.type\"",
        )
    });
    has(&e, "mode.insert.fallback: unknown command");
}

#[test]
fn structure_is_checked() {
    let e = broken("layout.toml", |t| {
        t.replace(
            "children = [\"topbar\", \"body\"]",
            "children = [\"topbar\", \"body\", \"nope\"]",
        )
    });
    has(&e, "node.root.children: no node \"nope\"");
    let e = broken("layout.toml", |t| {
        t.replace(
            "children = [\"topbar\", \"body\"]",
            "children = [\"topbar\", \"body\", \"topbar\"]",
        )
    });
    has(&e, "\"topbar\" is already a child");
    let e = broken("layout.toml", |t| {
        format!("{t}\n[node.lost]\nkind = \"spacer\"\n")
    });
    has(&e, "node.lost: not reachable from the root");
    let e = broken("components/topbar.toml", |t| {
        format!("{t}\n[node.body]\nkind = \"row\"\n")
    });
    assert_eq!(e.len(), 1, "{e:?}");
    has(
        &e,
        "layout.toml:10: node.body: already defined in components/topbar.toml:",
    );
    let e = broken("components/code.toml", |t| {
        t.replace(
            "[node.code]\nkind = \"surface\"",
            "[node.code]\nkind = \"surface\"\nwhen = \"ui.agent_open\"",
        )
    });
    has(
        &e,
        "node.code.when: the surface and the nodes around it are always shown",
    );
    let e = broken("components/sidebar.toml", |t| {
        t.replace(
            "source = \"workspace.files\"",
            "source = \"workspace.files\"\nheight = \"fit\"",
        )
    });
    has(&e, "a list or surface cannot be \"fit\"");
    let e = broken("pack.toml", |t| {
        t.replace(
            "agent = { node = \"agent\", mode = \"agent\" }",
            "agent = { node = \"agent\" }",
        )
    });
    has(&e, "regions.agent: give the key mode");
    let e = broken("pack.toml", |t| {
        t.replace(
            "agent = { node = \"agent\", mode = \"agent\" }",
            "agent = { node = \"body\", mode = \"agent\" }",
        )
    });
    has(&e, "regions.agent: contains region");
}

#[test]
fn pack_tables_are_checked() {
    let e = broken("pack.toml", |t| {
        t.replace("\"ui.agent_tab\" = \"chat\"", "\"ui.agent_tab\" = \"plan\"")
    });
    has(
        &e,
        "state_values.ui.agent_tab: the default \"plan\" is not in the list",
    );
    let e = broken("pack.toml", |t| {
        t.replace(
            "sidebar_shown = \"",
            "sidebar_shown = \"cond.agent_shown || ",
        )
    });
    has(
        &e,
        "conditions.sidebar_shown: cond.agent_shown: a condition cannot read another condition",
    );
    let e = broken("pack.toml", |t| {
        t.replace(
            "\"command.failed\" = \"{command}: {reason}\"",
            "\"command.failed\" = \"{cmd}\"",
        )
    });
    has(&e, "messages.command.failed: unknown placeholder {cmd}");
    let e = broken("pack.toml", |t| {
        t.replace("\"list.more\" = \"{count} more\"\n", "")
    });
    has(&e, "messages.list.more: missing");
    let e = broken("pack.toml", |t| {
        t.replace("files = { glyph = \"≡\"", "files = { glyph = \"文\"")
    });
    has(&e, "icons.files.glyph: must be one cell wide");
    let e = broken("pack.toml", |t| {
        t.replace("folder = { glyph = \"▸\", ascii = \"+\" }\n", "")
    });
    has(&e, "icons.folder: missing");
    let e = broken("pack.toml", |t| {
        t.replace("rust = \"Rust\"", "rust = \"Rust\"\nlua = \"Lua\"")
    });
    has(&e, "languages.lua: unknown language id");
    let e = broken("pack.toml", |t| {
        t.replace("theme = \"graphite\"", "theme = \"neon\"")
    });
    has(&e, "no theme file themes/neon.toml");
    let e = broken("pack.toml", |t| {
        t.replace(
            "\"ui.sidebar\" = \"files\"",
            "\"ui.focus\" = \"x\"\n\"ui.sidebar\" = \"files\"",
        )
    });
    has(&e, "state.ui.focus: built in");
}

#[test]
fn themes_are_checked() {
    let e = broken("themes/paper.toml", |t| {
        t.replace("\"gutter\" = { fg = \"t3\" }\n", "")
    });
    has(&e, "themes/paper.toml: tokens.gutter: missing");
    let e = broken("themes/sand.toml", |t| {
        t.replace(
            "\"app\" = { fg = \"t2\", bg = \"bg\" }",
            "\"app\" = { fg = \"t9\", bg = \"bg\" }",
        )
    });
    has(&e, "tokens.app.fg: \"t9\" is not #rrggbb or a palette name");
    let e = broken("themes/sand.toml", |t| {
        t.replace("accent = \"#c2602b\"", "accent = \"orange\"")
    });
    has(&e, "palette.accent: expected #rrggbb");
}

#[test]
fn files_outside_the_set_are_refused() {
    let mut files = fresh();
    files.push(("colours.toml".into(), String::new()));
    files.push(("components/extra/x.toml".into(), String::new()));
    let e: Vec<String> = load(&files)
        .unwrap_err()
        .iter()
        .map(ToString::to_string)
        .collect();
    has(&e, "colours.toml: not a pack file");
    has(&e, "components/extra/x.toml: not a pack file");
}

#[test]
fn templates_render() {
    let t = Template::parse("{{a}} {x} and {y}!").unwrap();
    assert_eq!(t.keys().collect::<Vec<_>>(), ["x", "y"]);
    assert_eq!(t.render(|k| (k == "x").then(|| "1".into())), "{a} 1 and !");
    assert!(Template::parse("{x").is_err());
    assert!(Template::parse("x}").is_err());
    assert!(Template::parse("{}").is_err());
}

fn sidebar(pack: &Pack, ctx: &spur_core::Context) -> spur_rpc::Node {
    fn walk(n: &spur_rpc::Node) -> Option<&spur_rpc::Node> {
        if n.id == "sidebar" {
            return Some(n);
        }
        n.children.iter().find_map(walk)
    }
    walk(&super::super::eval::tree(pack, ctx).root)
        .unwrap()
        .clone()
}

#[test]
fn borders_load_and_reach_the_tree() {
    let pack = load(&fresh()).unwrap_or_else(|e| panic!("{e:#?}"));
    let mut ctx = spur_core::Context::new();
    ctx.set("cond.sidebar_shown", true).unwrap();
    let side = sidebar(&pack, &ctx);
    let round = &pack.borders["round"];
    assert_eq!(side.border.as_ref(), Some(&round.glyphs));
    assert_eq!(side.border_alt.as_ref(), Some(&round.ascii));
    assert_eq!(side.border_style.as_deref(), Some("edge"));
    assert_eq!(
        side.style.as_deref(),
        Some("card"),
        "style stays without style_active"
    );
    ctx.set("ui.focus", "sidebar").unwrap();
    let side = sidebar(&pack, &ctx);
    assert_eq!(side.border_style.as_deref(), Some("edge.focus"));
    assert_eq!(side.style.as_deref(), Some("card"));
}

#[test]
fn borders_are_checked() {
    let border = "round = { glyphs = \"\u{256d}\u{256e}\u{2570}\u{256f}\u{2500}\u{2502}\", ascii = \"++++-|\" }";
    let e = broken("pack.toml", |t| {
        t.replace(border, "round = { glyphs = \"+-\", ascii = \"++++-\" }")
    });
    has(&e, "borders.round.glyphs: must be six one-cell glyphs");
    has(&e, "borders.round.ascii: must be six printable ASCII chars");
    let e = broken("pack.toml", |t| {
        t.replace(border, &border.replace('\u{256d}', "\u{6771}"))
    });
    has(&e, "borders.round.glyphs: must be six one-cell glyphs");
    let e = broken("components/sidebar.toml", |t| {
        t.replacen("border = \"round\"", "border = \"double\"", 1)
    });
    has(&e, "node.sidebar.border: no border \"double\" in pack.toml");
    let e = broken("components/sidebar.toml", |t| {
        t.replacen("border = \"round\"\n", "", 1)
    });
    has(
        &e,
        "node.sidebar.border: missing (border_style needs a border)",
    );
    let e = broken("components/sidebar.toml", |t| {
        t.replacen("border_style_active = \"edge.focus\"\n", "", 1)
    });
    has(&e, "active goes with style_active or border_style_active");
    let e = broken("components/topbar.toml", |t| {
        t.replacen("kind = \"text\"", "kind = \"text\"\nborder = \"round\"", 1)
    });
    has(&e, "unknown key for this kind");
}

/// Key hints are typed by hand (ADR 0007 debt): each one must be in this table,
/// and each key it names must be bound in `keys.toml`.
const HINTS: &[(&str, &[&str])] = &[
    ("ctrl+s", &["<C-s>"]),
    ("ctrl+k e", &["<C-k>e"]),
    ("ctrl+k a", &["<C-k>a"]),
    ("ctrl+q", &["<C-q>"]),
    ("up/down", &["<Up>", "<Down>"]),
    ("left/right", &["<Left>", "<Right>"]),
    ("enter", &["<CR>"]),
    ("esc", &["<Esc>"]),
];

#[test]
fn key_hints_name_bound_keys() {
    let pack = load(&fresh()).unwrap();
    let mut seen = 0;
    for (id, n) in &pack.nodes {
        if n.style.as_deref() != Some("key") {
            continue;
        }
        let text = n
            .text
            .as_ref()
            .map(|t| t.render(|_| None))
            .unwrap_or_default();
        let hint = text.trim();
        let Some((_, keys)) = HINTS.iter().find(|(h, _)| *h == hint) else {
            panic!("node.{id}: hint {hint:?} is not in HINTS; add it with its keys");
        };
        for k in *keys {
            let want = spur_keymap::parse_keys(k).unwrap();
            assert!(
                pack.keys.bindings.iter().any(|b| b.keys == want),
                "node.{id}: {hint:?} names {k}, which keys.toml does not bind"
            );
        }
        seen += 1;
    }
    assert!(seen >= 5, "found only {seen} hints");
}

#[test]
fn every_theme_token_is_used() {
    let pack = load(&fresh()).unwrap();
    let mut used: BTreeSet<&str> = spur_rpc::tokens::ALL.into_iter().collect();
    for n in pack.nodes.values() {
        for s in [
            &n.style,
            &n.style_active,
            &n.border_style,
            &n.border_style_active,
        ]
        .into_iter()
        .flatten()
        {
            used.insert(s);
        }
    }
    for (name, theme) in &pack.themes {
        for token in theme.tokens.keys() {
            assert!(
                used.contains(token.as_str()),
                "themes/{name}.toml: token {token:?} is not used by any node"
            );
        }
    }
}
