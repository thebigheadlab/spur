//! Key-to-frame latency of the editor server over Unix sockets.
//!
//! For each key, the client writes `key` and `ping` in one batch and waits for the
//! `pong`, which the server sends after the frame the key caused. Server time is
//! measured on the session thread, from taking the first message to handing the
//! batch to the writer. Scenarios: 80x24 and 300x100, one and two clients, with
//! the bare tree and with the `ux_fresh` pack (the shell: tree, lists, notices).
//! Then the palette over generated workspaces of 10,000 and 100,000 files.
//!
//! `cargo run --release -p spur-server --example latency [-- --keys N] [--only TEXT]`
//! (`--only` runs the scenarios whose name contains the text).

#[cfg(unix)]
mod common;

#[cfg(unix)]
fn main() {
    bench::run();
}

#[cfg(not(unix))]
fn main() {
    use std::io::Write as _;
    let _ = writeln!(std::io::stdout().lock(), "latency needs Unix sockets.");
}

#[cfg(unix)]
mod bench {
    use std::fmt::Write as _;
    use std::io::Write as _;
    use std::time::{Duration, Instant};

    use spur_rpc::Size;
    use spur_server::{PackSource, Session, SessionConfig};

    use crate::common::{Client, KEYS};

    /// Keys not counted while caches and allocations warm up.
    const WARMUP: usize = 200;

    /// What a scenario measures.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Kind {
        /// Typing with the bare tree (slice 1).
        Bare,
        /// Typing with the pack.
        Typing,
        /// Down and Up in an expanded 5,000-entry folder.
        Tree,
        /// ctrl+s after each typed key (the modified dot changes); only saves count.
        Save,
        /// The same without the pack: the save's own cost (it syncs the file).
        BareSave,
    }

    pub fn run() {
        let keys = std::env::args()
            .skip_while(|a| a != "--keys")
            .nth(1)
            .and_then(|n| n.parse().ok())
            .unwrap_or(10_000);
        let only = std::env::args()
            .skip_while(|a| a != "--only")
            .nth(1)
            .unwrap_or_default();
        let mut small = String::new();
        for i in 0..80 {
            let _ = writeln!(small, "line {i}: some text");
        }
        let mut big = String::new();
        for i in 0..200 {
            let _ = writeln!(big, "{i:>3} {}", "naïve café, 東京 ✓ text ".repeat(13));
        }
        let mut out = std::io::stdout().lock();
        let _ = writeln!(
            out,
            "{keys} keys per scenario; p50 budgets: typing 0.20 ms, tree and save 0.30 ms\n"
        );
        let _ = writeln!(
            out,
            "{:<26} {:>9} {:>9} {:>12} {:>12}",
            "scenario", "p50 ms", "p99 ms", "server p50", "server p99"
        );
        for (name, size, text, clients, kind) in [
            ("bare 80x24, 1 client", (80, 24), &small, 1, Kind::Bare),
            ("bare 80x24, 2 clients", (80, 24), &small, 2, Kind::Bare),
            ("bare 300x100, 1 client", (300, 100), &big, 1, Kind::Bare),
            ("bare 300x100, 2 clients", (300, 100), &big, 2, Kind::Bare),
            ("typing 80x24, 1 client", (80, 24), &small, 1, Kind::Typing),
            (
                "typing 300x100, 1 client",
                (300, 100),
                &big,
                1,
                Kind::Typing,
            ),
            (
                "typing 300x100, 2 clients",
                (300, 100),
                &big,
                2,
                Kind::Typing,
            ),
            ("tree 160x45, 1 client", (160, 45), &small, 1, Kind::Tree),
            (
                "bare save 160x45, 1 client",
                (160, 45),
                &small,
                1,
                Kind::BareSave,
            ),
            ("save 160x45, 1 client", (160, 45), &small, 1, Kind::Save),
        ] {
            if !name.contains(only.as_str()) {
                continue;
            }
            let n = if matches!(kind, Kind::Save | Kind::BareSave) {
                keys / 10
            } else {
                keys
            };
            let (round, server) = scenario(size, text, clients, n, kind);
            let _ = writeln!(
                out,
                "{name:<26} {:>9.3} {:>9.3} {:>12.3} {:>12.3}",
                ms(pct(&round, 50)),
                ms(pct(&round, 99)),
                ms(pct(&server, 50)),
                ms(pct(&server, 99)),
            );
        }
        let _ = writeln!(out);
        for files in [10_000, 100_000] {
            if format!("palette {files}").contains(only.as_str()) {
                palette(&mut out, files);
            }
        }
    }

    /// A workspace of `n` files: 6 folders deep, names of 6 to 20 ASCII characters.
    fn workspace(dir: &std::path::Path, n: usize) {
        const PARTS: [&str; 16] = [
            "src", "main", "lib", "mod", "util", "core", "view", "test", "data", "item", "node",
            "pack", "list", "key", "sync", "text",
        ];
        let mut seed = 0x2545_f491_u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let name = |r: u64, ext: &str| {
            let mut s = String::new();
            let mut r = r;
            while s.len() < 6 || (s.len() < 14 && !r.is_multiple_of(3)) {
                let i = usize::try_from(r % 16).unwrap_or(0);
                if !s.is_empty() {
                    s.push('_');
                }
                s.push_str(PARTS[i]);
                r /= 16;
            }
            s.truncate(20 - ext.len());
            s.push_str(ext);
            s
        };
        for i in 0..n {
            let mut p = dir.to_path_buf();
            let mut r = next();
            for _ in 0..6 {
                // 8 folders a level: a wide, deep tree.
                p.push(format!(
                    "d{}_{}",
                    r % 8,
                    PARTS[usize::try_from(r % 16).unwrap_or(0)]
                ));
                r /= 8;
            }
            std::fs::create_dir_all(&p).expect("temp dir");
            let ext = [".rs", ".toml", ".md", ".txt"][i % 4];
            p.push(format!("{}{i}", name(next(), ext)));
            std::fs::write(&p, "").expect("temp file");
        }
    }

    /// Palette rows: opens, keystrokes, and keystroke to settled rows.
    fn palette(out: &mut impl std::io::Write, files: usize) {
        let dir = std::env::temp_dir().join(format!("spur-latency-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        workspace(&dir, files);
        let file = dir.join("bench.rs");
        std::fs::write(&file, "fn main() {}\n").expect("temp file");
        let pack = concat!(env!("CARGO_MANIFEST_DIR"), "/../../extensions/ux_fresh");
        let session = Session::spawn(SessionConfig {
            name: "latency".into(),
            open: vec![file],
            pack: Some(PackSource::Dir(pack.into())),
            root: dir.clone(),
            ..SessionConfig::default()
        })
        .expect("session");
        let size = Size {
            cols: 160,
            rows: 45,
        };
        let layout = [("code", 40, 120), ("palette_list", 18, 68)];
        let quiet = Duration::from_millis(40);
        // A first client walks the index and waits for it to finish.
        {
            let mut c = Client::connect(&session, "walk", size);
            c.layout(&layout);
            c.keys("<C-p>");
            let t = Instant::now();
            let mut last = (0, Instant::now());
            while t.elapsed() < Duration::from_mins(2) {
                std::thread::sleep(Duration::from_millis(100));
                c.sync();
                let total = c.lists.get("palette_list").map_or(0, |l| l.total);
                if total != last.0 {
                    last = (total, Instant::now());
                } else if total > 0 && last.1.elapsed() > Duration::from_millis(500) {
                    break;
                }
            }
        }
        let mut c = Client::connect(&session, "bench", size);
        c.layout(&layout);
        // First open for this client: its matcher is made and filled.
        let (first, _) = c.keys_settle("<C-p>", "palette_list", quiet);
        let mut opens = Vec::new();
        for _ in 0..50 {
            c.keys("<Esc>");
            opens.push(c.keys_settle("<C-p>", "palette_list", quiet).0);
        }
        let mut frame = Vec::new();
        let mut settle = Vec::new();
        let mut key_server = Vec::new();
        for _ in 0..5 {
            for q in ["src", "hlt", "mainrs", "zzzz"] {
                for ch in q
                    .chars()
                    .map(String::from)
                    .chain(q.chars().map(|_| "<BS>".into()))
                {
                    let before = session.stats().len();
                    let (f, s) = c.keys_settle(&ch, "palette_list", quiet);
                    let stats = session.stats();
                    if let Some(k) = stats.get(before) {
                        key_server.push(*k);
                    }
                    frame.push(f);
                    if let Some(s) = s {
                        settle.push(s);
                    }
                }
            }
        }
        let _ = writeln!(out, "palette, {files} files (p50 ms)");
        let row = |out: &mut dyn std::io::Write, name: &str, v: &[Duration]| {
            let _ = writeln!(
                out,
                "  {name:<34} {:>8.3} (p99 {:.3}, n {})",
                ms(pct(v, 50)),
                ms(pct(v, 99)),
                v.len()
            );
        };
        row(out, "first open, round trip", &[first]);
        row(out, "later opens, round trip", &opens);
        row(out, "keystroke, server", &key_server);
        row(out, "keystroke, round trip", &frame);
        row(out, "keystroke to settled rows", &settle);
        let _ = writeln!(out);
        drop(c);
        session.join();
        let _ = std::fs::remove_dir_all(dir);
    }

    fn scenario(
        (cols, rows): (u32, u32),
        text: &String,
        clients: usize,
        keys: usize,
        kind: Kind,
    ) -> (Vec<Duration>, Vec<Duration>) {
        let dir = std::env::temp_dir().join(format!("spur-latency-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let mut path = dir.join("bench.rs");
        std::fs::write(&path, text).expect("temp file");
        if kind == Kind::Tree {
            let folder = dir.join("big");
            std::fs::create_dir_all(&folder).expect("temp dir");
            for i in 0..5000 {
                std::fs::write(folder.join(format!("f{i:04}.txt")), "x\n").expect("temp file");
            }
            // Opening a file in it reveals the folder, expanded.
            path = folder.join("f2500.txt");
        }
        let pack = concat!(env!("CARGO_MANIFEST_DIR"), "/../../extensions/ux_fresh");
        let bare = matches!(kind, Kind::Bare | Kind::BareSave);
        let session = Session::spawn(if bare {
            SessionConfig {
                name: "latency".into(),
                open: vec![path],
                keymap: vec![("example".into(), KEYS.into())],
                ..SessionConfig::default()
            }
        } else {
            // The pack brings its own keys.
            SessionConfig {
                name: "latency".into(),
                open: vec![path],
                pack: Some(PackSource::Dir(pack.into())),
                root: dir.clone(),
                ..SessionConfig::default()
            }
        })
        .expect("session");
        let size = Size { cols, rows };
        // Roughly what the terminal client lays out: the code surface under its
        // header, the Files list under its label.
        let layout = [
            ("code", rows.saturating_sub(5), cols.saturating_sub(40)),
            ("files_list", rows.saturating_sub(8), 24),
        ];
        let connect = |name: &str| {
            let mut c = Client::connect(&session, name, size);
            if !bare {
                c.layout(&layout);
            }
            c
        };
        let mut c = connect("bench");
        let others: Vec<_> = (1..clients)
            .map(|_| {
                let o = connect("peer");
                std::thread::spawn(move || o.serve())
            })
            .collect();
        match kind {
            // Start in the middle of the file so frames are full.
            Kind::Bare | Kind::Typing => c.keys(&"<Down>".repeat(40)),
            Kind::Tree => c.keys("<C-S-e>"),
            Kind::Save | Kind::BareSave => {}
        }
        let start = session.stats().len();
        let mut round = Vec::with_capacity(keys);
        let mut server_keys = Vec::with_capacity(keys);
        for i in 0..keys + WARMUP {
            let key = match kind {
                // Mostly typing, with a new line now and then so lines stay short.
                Kind::Bare | Kind::Typing => match i % 40 {
                    39 => "<CR>",
                    n => ["a", "b", "c", "d", "e", "f", "g", "h"][n % 8],
                },
                Kind::Tree if i % 80 < 40 => "<Down>",
                Kind::Tree => "<Up>",
                Kind::Save | Kind::BareSave => {
                    c.keys("a");
                    "<C-s>"
                }
            };
            let t = Instant::now();
            let before = session.stats().len();
            c.keys(key);
            if i >= WARMUP {
                round.push(t.elapsed());
                server_keys.push(before);
            }
        }
        let stats = session.stats();
        let server: Vec<Duration> = if matches!(kind, Kind::Save | Kind::BareSave) {
            // Only the samples of the saves.
            server_keys
                .iter()
                .filter_map(|&n| stats.get(n).copied())
                .collect()
        } else {
            stats
                .get(start + WARMUP..)
                .map(<[_]>::to_vec)
                .unwrap_or_default()
        };
        drop(c);
        session.join();
        for o in others {
            let _ = o.join();
        }
        let _ = std::fs::remove_dir_all(dir);
        (round, server)
    }

    fn pct(v: &[Duration], p: usize) -> Duration {
        let mut v = v.to_vec();
        v.sort_unstable();
        v.get((v.len().saturating_sub(1)) * p / 100)
            .copied()
            .unwrap_or_default()
    }

    fn ms(d: Duration) -> f64 {
        d.as_secs_f64() * 1000.0
    }
}
