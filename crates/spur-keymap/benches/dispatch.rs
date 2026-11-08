//! Dispatch cost with 5k bindings (#23 budget: < 50 µs including `when`).

use std::fmt::Write as _;
use std::time::Instant;

use criterion::Criterion;
use spur_core::{Context, Source};
use spur_keymap::{Keymap, Layer, parse_keys};

fn main() {
    let mut toml = String::new();
    let chars: Vec<char> = ('a'..='z').collect();
    let mut n = 0;
    'outer: for a in &chars {
        for b in &chars {
            for c in &chars {
                let _ = writeln!(
                    toml,
                    "[[bind]]\nmode = \"normal\"\nkeys = \"<Space>{a}{b}{c}\"\ncommand = \"bench.cmd_{n}\"\nwhen = \"editor.focus && editor.mode == normal && !editor.reviewing\"\n"
                );
                n += 1;
                if n == 5000 {
                    break 'outer;
                }
            }
        }
    }
    let mut k = Keymap::new();
    k.push_layer(Layer::from_toml("bench", Source::Core, &toml).unwrap());
    let mut cx = Context::new();
    cx.set("editor.focus", true).unwrap();
    cx.set("editor.mode", "normal").unwrap();
    let keys = parse_keys("<Space>gzz").unwrap();

    let mut c = Criterion::default().configure_from_args();
    c.bench_function("dispatch/4_keys_5k_bindings", |b| {
        b.iter(|| {
            let now = Instant::now();
            let mut ran = 0;
            for key in &keys {
                ran += k.feed(*key, "normal", &cx, now).run.len();
            }
            assert_eq!(ran, 1);
        });
    });
    c.final_summary();
}
