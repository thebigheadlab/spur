//! The whole stack without a terminal: a session, the bundled pack, the protocol and the
//! terminal renderer drawing into a grid.
#![cfg(feature = "tui")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use spur_rpc::{
    Caps, ClientEnd, ClientKind, ClientMsg, PROTOCOL_VERSION, ServerMsg, Size, Style, SurfaceFrame,
    Ui, in_process,
};
use spur_server::{Pack, Session, SessionConfig};
use spur_tui::{Frame, Target, ViewState, render};

const FRESH: &str = include_str!("../../../extensions/ux_fresh/pack.toml");
const FALLBACK: &str = include_str!("../../../extensions/fallback.toml");

struct Tui {
    end: ClientEnd,
    theme: BTreeMap<String, Style>,
    ui: Option<Ui>,
    surface: SurfaceFrame,
    size: (u16, u16),
    state: ViewState,
    pings: u64,
}

impl Tui {
    fn start(root: &Path, open: &str, pack: &str, size: (u16, u16)) -> (Session, Self) {
        let session = Session::spawn(SessionConfig {
            name: "e2e".into(),
            root: root.to_path_buf(),
            pack: Pack::parse(pack).unwrap(),
            open: Some(open.into()),
            exit_when_empty: true,
            notice: None,
        })
        .unwrap();
        let (end, server) = in_process();
        session.attach(server);
        end.send(ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            kind: ClientKind::Frontend,
            name: "tui".into(),
            caps: Caps {
                cells: true,
                truecolor: true,
                mouse: true,
            },
            size: Size {
                cols: size.0,
                rows: size.1,
            },
        });
        let mut tui = Self {
            end,
            theme: BTreeMap::new(),
            ui: None,
            surface: SurfaceFrame::default(),
            size,
            state: ViewState::default(),
            pings: 0,
        };
        // Like the real client: draw, report the surface size, draw again.
        let first = tui.frame();
        let (rows, cols) = first.surface_size.unwrap();
        tui.end.send(ClientMsg::SurfaceSize { rows, cols });
        (session, tui)
    }

    fn frame(&mut self) -> Frame {
        self.pings += 1;
        self.end.send(ClientMsg::Ping { id: self.pings });
        loop {
            match self
                .end
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap()
            {
                ServerMsg::Pong { id } if id == self.pings => break,
                ServerMsg::Theme { tokens } => self.theme = tokens,
                ServerMsg::Ui { ui } => self.ui = Some(ui),
                ServerMsg::Surface { frame } => self.surface = frame,
                _ => {}
            }
        }
        render(
            self.ui.as_ref().unwrap(),
            &self.surface,
            &self.theme,
            self.size,
            &mut self.state,
        )
    }

    fn keys(&mut self, keys: &[&str]) -> Frame {
        for k in keys {
            self.end.send(ClientMsg::Key { key: (*k).into() });
        }
        self.frame()
    }
}

fn workspace(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spur-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/lib.rs"),
        "pub fn add(a: u8) -> u8 {\n\ta + 1 // 日本\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("README.md"), "# Demo\n").unwrap();
    dir
}

#[test]
fn the_fresh_pack_draws_a_workspace() {
    let root = workspace("fresh");
    let (_s, mut tui) = Tui::start(&root, "src/lib.rs", FRESH, (80, 12));
    let f = tui.frame();
    let name = root.file_name().unwrap().to_string_lossy();
    let expect = format!(
        "  {name}  /  src  /  lib.rs

  Files                          1  pub fn add(a: u8) -> u8 {{
                                 2      a + 1 // 日本
  ▾ src                          3  }}
      lib.rs                     4
    README.md




  ctrl space Ask    ctrl e Files    ctrl s Save    ctrl z Undo    ctrl q Quit   Ln 1, Col 1    Rust"
    );
    // The footer is wider than 80 columns here; compare what fits.
    let got = f.grid.text();
    for (g, e) in got.lines().zip(expect.lines()) {
        let e: String = e.chars().take(g.chars().count().max(40)).collect();
        assert!(
            g.starts_with(e.trim_end()) || e.starts_with(g),
            "\n got: {g:?}\nwant: {e:?}\n{got}"
        );
    }
    assert_eq!(
        f.cursor,
        Some((36, 2)),
        "the cursor sits at line 1, column 1 of the text"
    );

    // Tokens became colours; the server never sent one with the text.
    let kw = f.grid.cell(36, 2).unwrap();
    assert_eq!(kw.style.fg, tui.theme["syntax.keyword"].fg);
    assert_eq!(kw.style.bg, tui.theme["line.current"].bg);
    let panel = f.grid.cell(1, 7).unwrap();
    assert_eq!(panel.style.bg, tui.theme["panel"].bg);

    // Tabs expand, wide chars take two cells, and the cursor follows.
    let f = tui.keys(&["<Down>", "<End>"]);
    assert_eq!(f.cursor, Some((36 + 4 + 13, 3)));

    // A click maps back to a document position through the same geometry.
    let hit = f.hit(36 + 4, 3).unwrap();
    assert_eq!(
        (hit.node.as_str(), &hit.target),
        ("editor", &Target::Surface)
    );
    assert_eq!(spur_tui::char_col("\ta + 1 // 日本", 4), 1);
    let hit = f.hit(6, 6).unwrap();
    let Target::List { rows } = &hit.target else {
        panic!("{hit:?}")
    };
    assert_eq!(
        rows[usize::from(6 - hit.rect.y)],
        Some(2),
        "README.md is row 2 of the list"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn ask_is_a_modal_card_that_fits_its_rows() {
    let root = workspace("ask");
    let (_s, mut tui) = Tui::start(&root, "README.md", FRESH, (100, 30));
    let before = tui.frame();
    let f = tui.keys(&["<C-Space>", "l", "i", "b"]);
    let text = f.grid.text();
    assert!(text.contains("› lib"), "{text}");
    assert!(text.contains("▌ · src/lib.rs"), "{text}");
    assert!(text.contains("enter Run"), "{text}");
    // Behind the card everything is dimmed, and clicks do not reach it.
    let (was, now) = (before.grid.cell(3, 0).unwrap(), f.grid.cell(3, 0).unwrap());
    assert_eq!(was.ch, now.ch);
    assert_ne!(was.style.fg, now.style.fg);
    assert_eq!(f.hit(3, 6).unwrap().target, Target::Nothing);
    assert!(
        f.cursor.is_some_and(|(_, y)| y < 8),
        "the cursor is in the input"
    );
    // One header, one file, input, hints and padding: the card is short.
    let card_rows = (0..30)
        .filter(|y| f.grid.cell(12, *y).unwrap().style.bg == tui.theme["float"].bg)
        .count();
    assert_eq!(card_rows, 8, "{text}");

    let f = tui.keys(&["<CR>"]);
    assert!(f.grid.text().contains("pub fn add"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn another_pack_is_another_editor() {
    let root = workspace("fallback");
    let (_s, mut tui) = Tui::start(&root, "README.md", FALLBACK, (40, 5));
    let f = tui.keys(&["x"]);
    assert_eq!(
        f.grid.text(),
        "  1  x# Demo\n  2\n\n\n README.md  Ln 1, Col 2"
    );
    // A narrow screen and a long line: the view scrolls sideways with the cursor.
    let long = "y".repeat(50);
    tui.end.send(ClientMsg::Text { text: long });
    let f = tui.frame();
    assert!(f.hscroll > 0);
    assert_eq!(f.cursor.map(|c| c.0), Some(39));
    std::fs::remove_dir_all(root).unwrap();
}

/// The server never draws: the crates below it must not pull in a terminal, window or
/// web library, and the protocol crate must stand alone so any client can use it.
#[test]
fn server_crates_do_not_depend_on_a_ui_library() {
    const BANNED: &[&str] = &[
        "crossterm",
        "ratatui",
        "termion",
        "winit",
        "wgpu",
        "glyphon",
        "web-sys",
    ];
    let manifests = [
        ("spur-server", include_str!("../../spur-server/Cargo.toml")),
        ("spur-core", include_str!("../../spur-core/Cargo.toml")),
        ("spur-keymap", include_str!("../../spur-keymap/Cargo.toml")),
        ("spur-text", include_str!("../../spur-text/Cargo.toml")),
        ("spur-rpc", include_str!("../../spur-rpc/Cargo.toml")),
    ];
    for (name, manifest) in manifests {
        for banned in BANNED {
            assert!(
                !manifest.lines().any(|l| l.trim_start().starts_with(banned)),
                "{name} must not depend on {banned}"
            );
        }
        assert!(
            !manifest.contains("spur-tui") && !manifest.contains("spur-gui"),
            "{name}"
        );
    }
    let rpc = include_str!("../../spur-rpc/Cargo.toml");
    assert!(
        !rpc.contains("spur-core") && !rpc.contains("workspace = true }"),
        "the protocol crate depends on no other spur crate"
    );
    let tui = include_str!("../../spur-tui/Cargo.toml");
    assert!(
        !tui.contains("spur-core") && !tui.contains("spur-server") && !tui.contains("spur-keymap"),
        "a frontend speaks the protocol only"
    );
}
