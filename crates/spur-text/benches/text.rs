//! Text engine benches against the ADR 0003 thresholds. Fixtures are generated from a
//! fixed seed at run time; nothing is committed.

use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{BatchSize, Criterion};
use spur_text::{AuthorId, Bias, Buffer, Change, ChangeSet, Transaction};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % (n as u64).max(1)).unwrap_or(0)
    }
}

/// ASCII text with lines of 20-100 bytes and some CRLF endings.
fn fixture(bytes: usize) -> String {
    let mut rng = Rng(42);
    let mut s = String::with_capacity(bytes + 128);
    while s.len() < bytes {
        let n = 20 + rng.below(80);
        for _ in 0..n {
            s.push(char::from(b'a' + u8::try_from(rng.below(26)).unwrap_or(0)));
        }
        s.push_str(if rng.below(7) == 0 { "\r\n" } else { "\n" });
    }
    s
}

fn insert(buf: &Buffer, author: AuthorId, at: usize, text: &str) -> Transaction {
    Transaction::new(
        author,
        buf.version(),
        ChangeSet::insert(buf.len(), at, text).unwrap(),
    )
}

fn snapshot_clone(c: &mut Criterion) {
    let buf = Buffer::new(&fixture(100 << 20));
    c.bench_function("snapshot_clone_100mb", |b| {
        b.iter(|| black_box(buf.snapshot()));
    });
}

fn apply_insert(c: &mut Criterion) {
    let mut buf = Buffer::new(&fixture(100 << 20));
    let mut rng = Rng(7);
    for _ in 0..10_000 {
        let at = rng.below(buf.len());
        buf.track(at, Bias::Left);
    }
    c.bench_function("apply_insert_100mb_10k_tracked", |b| {
        b.iter(|| {
            let at = rng.below(buf.len());
            buf.apply(insert(&buf, AuthorId::User, at, "x")).unwrap()
        });
    });
}

fn rebase_agent_changeset(c: &mut Criterion) {
    let text = fixture(1 << 20);
    c.bench_function("rebase_500_hunks_over_50_edits", |b| {
        b.iter_batched(
            || {
                let mut buf = Buffer::new(&text);
                let base = buf.version();
                let stride = buf.len() / 500;
                let agent = ChangeSet::new(
                    buf.len(),
                    (0..500).map(|i| Change::new(i * stride + 1..i * stride + 3, "AG")),
                )
                .unwrap();
                // 50 user edits that land between agent hunks.
                for i in 0..50 {
                    let at = i * stride * 10 + stride / 2 + i;
                    buf.apply(insert(&buf, AuthorId::User, at, "u")).unwrap();
                }
                (
                    buf,
                    Transaction::new(AuthorId::Agent("bench".into()), base, agent),
                )
            },
            |(mut buf, txn)| buf.apply(txn).unwrap(),
            BatchSize::LargeInput,
        );
    });
}

fn resolve_tracked(c: &mut Criterion) {
    let mut buf = Buffer::new(&fixture(1 << 20));
    let mut rng = Rng(9);
    let ids: Vec<_> = (0..10_000)
        .map(|_| {
            let at = rng.below(buf.len());
            buf.track(at, Bias::Right)
        })
        .collect();
    for _ in 0..10_000 {
        let at = rng.below(buf.len());
        buf.apply(insert(&buf, AuthorId::User, at, "x")).unwrap();
    }
    c.bench_function("resolve_10k_tracked_after_10k_edits", |b| {
        b.iter(|| {
            ids.iter()
                .map(|&id| buf.tracked(id).unwrap_or(0))
                .sum::<usize>()
        });
    });
}

/// Per-transaction latency percentiles, which criterion does not report.
fn insert_latency() {
    let mut buf = Buffer::new(&fixture(100 << 20));
    let mut rng = Rng(11);
    for _ in 0..10_000 {
        let at = rng.below(buf.len());
        buf.track(at, Bias::Left);
    }
    let mut lat: Vec<Duration> = (0..100_000)
        .map(|_| {
            let at = rng.below(buf.len());
            let txn = insert(&buf, AuthorId::User, at, "x");
            let start = Instant::now();
            black_box(buf.apply(txn).unwrap());
            start.elapsed()
        })
        .collect();
    lat.sort();
    eprintln!(
        "insert latency, 100 MB, 10k tracked anchors, undo on: p50 {:?} p99 {:?} max {:?}",
        lat[lat.len() / 2],
        lat[lat.len() * 99 / 100],
        lat[lat.len() - 1]
    );
}

fn main() {
    insert_latency();
    let mut c = Criterion::default().configure_from_args();
    snapshot_clone(&mut c);
    apply_insert(&mut c);
    rebase_agent_changeset(&mut c);
    resolve_tracked(&mut c);
    c.final_summary();
}
