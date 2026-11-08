//! Hot-path costs for the keymap budget (#23: dispatch < 50 µs with 5k bindings).

use std::hint::black_box;

use criterion::Criterion;
use serde_json::Value;
use spur_core::{Actor, CommandId, CommandMeta, Context, Core, Invocation, When};

fn main() {
    let mut c = Criterion::default().configure_from_args();

    let mut cx = Context::new();
    cx.set("editor.mode", "normal").unwrap();
    cx.set("editor.focus", true).unwrap();
    cx.set("buffer.filetype", "rust").unwrap();
    let when = When::parse(
        "editor.focus && editor.mode == normal && !editor.reviewing && buffer.filetype =~ /^(rust|toml)$/",
    )
    .unwrap();
    assert!(when.eval(&cx));
    c.bench_function("when/eval_4_clauses", |b| {
        b.iter(|| black_box(&when).eval(&cx));
    });

    let mut core = Core::new(0u64);
    let id = CommandId::new("editor.move_down").unwrap();
    core.register_fn(
        CommandMeta::new(id.clone(), "down").repeatable(),
        |cx, _| {
            *cx.state += 1;
            Ok(Value::Null)
        },
    )
    .unwrap();
    let handle = core.resolve(&id).unwrap();
    c.bench_function("execute/user_null_args", |b| {
        b.iter(|| core.execute(Invocation::new(handle, Actor::User).count(1)));
    });

    c.final_summary();
}
