//! Key-to-frame latency of the editor server over Unix sockets.
//!
//! For each key, the client writes `key` and `ping` in one batch and waits for the
//! `pong`, which the server sends after the frame the key caused. Server time is
//! measured on the session thread, from taking the first message to handing the
//! batch to the writer. Four scenarios: 80x24 and 300x100, one and two clients.
//!
//! `cargo run --release -p spur-server --example latency [-- --keys N]`

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
    use spur_server::{Session, SessionConfig};

    use crate::common::{Client, KEYS};

    /// Keys not counted while caches and allocations warm up.
    const WARMUP: usize = 200;

    pub fn run() {
        let keys = std::env::args()
            .skip_while(|a| a != "--keys")
            .nth(1)
            .and_then(|n| n.parse().ok())
            .unwrap_or(10_000);
        let mut small = String::new();
        for i in 0..80 {
            let _ = writeln!(small, "line {i}: some text");
        }
        let mut big = String::new();
        for i in 0..200 {
            let _ = writeln!(big, "{i:>3} {}", "naïve café, 東京 ✓ text ".repeat(13));
        }
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{keys} keys per scenario, budget 0.20 ms\n");
        let _ = writeln!(
            out,
            "{:<20} {:>9} {:>9} {:>12} {:>12}",
            "scenario", "p50 ms", "p99 ms", "server p50", "server p99"
        );
        for (name, size, text, clients) in [
            ("80x24, 1 client", (80, 24), &small, 1),
            ("80x24, 2 clients", (80, 24), &small, 2),
            ("300x100, 1 client", (300, 100), &big, 1),
            ("300x100, 2 clients", (300, 100), &big, 2),
        ] {
            let (round, server) = scenario(size, text, clients, keys);
            let _ = writeln!(
                out,
                "{name:<20} {:>9.3} {:>9.3} {:>12.3} {:>12.3}",
                ms(pct(&round, 50)),
                ms(pct(&round, 99)),
                ms(pct(&server, 50)),
                ms(pct(&server, 99)),
            );
        }
    }

    fn scenario(
        (cols, rows): (u32, u32),
        text: &String,
        clients: usize,
        keys: usize,
    ) -> (Vec<Duration>, Vec<Duration>) {
        let dir = std::env::temp_dir().join(format!("spur-latency-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("bench.txt");
        std::fs::write(&path, text).expect("temp file");
        let session = Session::spawn(SessionConfig {
            name: "latency".into(),
            open: vec![path],
            keymap: vec![("example".into(), KEYS.into())],
            ..SessionConfig::default()
        })
        .expect("session");
        let size = Size { cols, rows };
        let mut c = Client::connect(&session, "bench", size);
        let others: Vec<_> = (1..clients)
            .map(|_| {
                let o = Client::connect(&session, "peer", size);
                std::thread::spawn(move || o.serve())
            })
            .collect();
        // Start in the middle of the file so frames are full.
        c.keys(&"<Down>".repeat(40));
        let start = session.stats().len();
        let mut round = Vec::with_capacity(keys);
        for i in 0..keys + WARMUP {
            // Mostly typing, with a new line now and then so lines stay short.
            let key = match i % 40 {
                39 => "<CR>",
                n => ["a", "b", "c", "d", "e", "f", "g", "h"][n % 8],
            };
            let t = Instant::now();
            c.keys(key);
            if i >= WARMUP {
                round.push(t.elapsed());
            }
        }
        let server = session.stats().split_off(start + WARMUP);
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
