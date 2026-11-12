//! Two clients edit one file through one editor server, over Unix sockets.
//!
//! Draws both clients' latest frames side by side. This is a protocol debug view in
//! plain ANSI, not the Spur look: inverse marks a client's own cursor, underline a
//! peer's cursor.
//!
//! `cargo run --release -p spur-server --example two_clients [-- --fast]`

#[cfg(unix)]
mod common;

#[cfg(unix)]
fn main() {
    demo::run();
}

#[cfg(not(unix))]
fn main() {
    use std::io::Write as _;
    let _ = writeln!(std::io::stdout().lock(), "two_clients needs Unix sockets.");
}

#[cfg(unix)]
mod demo {
    use std::fmt::Write as _;
    use std::io::Write as _;
    use std::time::Duration;

    use spur_rpc::{CursorKind, Size, SurfaceView};
    use spur_server::{Session, SessionConfig};

    use crate::common::{Client, KEYS};

    const WIDTH: usize = 38;
    const ROWS: u32 = 10;

    pub fn run() {
        let fast = std::env::args().any(|a| a == "--fast");
        let dir = std::env::temp_dir().join(format!("spur-two-clients-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("notes.txt");
        std::fs::write(&path, "# notes\n\n\n\n\n\n\n").expect("temp file");

        let session = Session::spawn(SessionConfig {
            name: "demo".into(),
            open: vec![path],
            keymap: vec![("example".into(), KEYS.into())],
            ..SessionConfig::default()
        })
        .expect("session");
        let size = Size {
            cols: 38,
            rows: ROWS,
        };
        let mut a = Client::connect(&session, "A", size);
        let mut b = Client::connect(&session, "B", size);
        let mut show = Screen { fast, step: 0 };

        show.frame(&mut a, &mut b, "A and B attach; both start on line 1");
        a.keys("<Down>");
        b.keys("<Down><Down><Down>");
        show.frame(&mut a, &mut b, "A goes to line 2, B to line 4");

        let (ta, tb) = ("Hello from A.", "B was here.");
        let mut ia = ta.chars();
        let mut ib = tb.chars();
        loop {
            let (ca, cb) = (ia.next(), ib.next());
            if ca.is_none() && cb.is_none() {
                break;
            }
            if let Some(ch) = cb {
                b.keys(&ch.to_string());
            }
            if let Some(ch) = ca {
                a.keys(&ch.to_string());
            }
            show.frame(&mut a, &mut b, "both type at once");
        }
        b.keys("<Up><Up><Home>");
        show.frame(&mut a, &mut b, "B moves to the start of A's line");
        b.keys("Hi A! ");
        show.frame(&mut a, &mut b, "B types on A's line; A's cursor shifts");
        a.keys("<C-z>");
        show.frame(
            &mut a,
            &mut b,
            "A undoes: A's last typing goes, B's newer edit stays",
        );
        b.keys("<C-z>");
        show.frame(&mut a, &mut b, "B undoes: B's \"Hi A! \" goes");
        a.keys("<C-y>");
        show.frame(&mut a, &mut b, "A redoes its typing");
        show.pause(Duration::from_secs(2));
        drop((a, b));
        session.join();
        let _ = std::fs::remove_dir_all(dir);
    }

    struct Screen {
        fast: bool,
        step: u32,
    }

    impl Screen {
        fn pause(&self, d: Duration) {
            if !self.fast {
                std::thread::sleep(d);
            }
        }

        fn frame(&mut self, a: &mut Client, b: &mut Client, caption: &str) {
            a.sync();
            b.sync();
            self.step += 1;
            let mut s = String::from("\x1b[2J\x1b[H");
            s.push_str("spur server: protocol debug view (not the Spur look)\n\n");
            let left = panel("A", a);
            let right = panel("B", b);
            for (l, r) in left.iter().zip(&right) {
                let _ = writeln!(s, "{l}  \u{2502}  {r}");
            }
            let _ = writeln!(s, "\nstep {:>2}: {caption}", self.step);
            s.push_str("inverse = own cursor, underline = the other client's cursor\n");
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(s.as_bytes());
            let _ = out.flush();
            self.pause(Duration::from_millis(if caption.starts_with("both") {
                250
            } else {
                2200
            }));
        }
    }

    /// Rows of one client's panel, each exactly `WIDTH` visible columns.
    fn panel(name: &str, c: &Client) -> Vec<String> {
        let mut rows = Vec::new();
        let Some(v) = &c.view else {
            return vec![pad(String::new(), 0); ROWS as usize + 2];
        };
        let head = format!("client {name} (#{})", c.id);
        rows.push(pad(head.clone(), head.chars().count()));
        for (i, line) in v.lines.iter().enumerate() {
            let n = v.top_line as usize + i;
            let text: String = line.spans.iter().map(|s| s.text.as_str()).collect();
            rows.push(draw_line(n, &text, v));
        }
        while rows.len() < ROWS as usize + 1 {
            rows.push(pad(String::from("~"), 1));
        }
        // The server sends full paths; a client shortens them for people.
        let name = v
            .path
            .as_deref()
            .and_then(|p| p.rsplit(['/', '\\']).next())
            .unwrap_or("[scratch]");
        let status = format!(
            "{} {} {} v{}",
            name,
            if v.modified { "[+]" } else { "" },
            v.mode,
            v.version
        );
        let status: String = status.chars().take(WIDTH).collect();
        let len = status.chars().count();
        rows.push(pad(format!("\x1b[2m{status}\x1b[0m"), len));
        rows
    }

    fn draw_line(n: usize, text: &str, v: &SurfaceView) -> String {
        let mark = |col: usize| {
            v.cursors
                .iter()
                .filter(|c| c.line as usize == n && c.col as usize == col)
                .map(|c| c.kind)
                .min_by_key(|k| matches!(k, CursorKind::Peer))
        };
        let mut s = format!("{:>2} ", n + 1);
        let mut used = 3;
        let chars: Vec<char> = text.chars().collect();
        for col in 0..=chars.len() {
            if used >= WIDTH {
                break;
            }
            let ch = chars.get(col).copied();
            if ch.is_none() && mark(col).is_none() {
                break;
            }
            let ch = ch.unwrap_or(' ');
            match mark(col) {
                Some(CursorKind::Peer) => {
                    let _ = write!(s, "\x1b[4m{ch}\x1b[0m");
                }
                Some(_) => {
                    let _ = write!(s, "\x1b[7m{ch}\x1b[0m");
                }
                None => s.push(ch),
            }
            used += 1;
        }
        pad(s, used)
    }

    /// Pads `s`, whose visible width is `len`, to `WIDTH` columns.
    fn pad(mut s: String, len: usize) -> String {
        s.extend(std::iter::repeat_n(' ', WIDTH.saturating_sub(len)));
        s
    }
}
