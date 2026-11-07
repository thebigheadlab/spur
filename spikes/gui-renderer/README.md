# GUI renderer spike (#28)

Throwaway prototype backing [ADR 0002](../../docs/adr/0002-gui-stack.md). Standalone crate
(own `[workspace]`), not part of the main build.

Stack: winit 0.30 · wgpu 30 · glyphon 0.12 (cosmic-text 0.19 shaping + swash raster + wgpu glyph atlas) · one instanced-rect pipeline.

What it does: 10k-line synthetic Rust file with toy highlighting, smooth scroll (wheel/PageUp/PageDown),
cursor, selection (shift+arrows, mouse drag), IME preedit/commit, and a completion popup (Ctrl+Space).
Only visible lines are shaped; shaped lines are cached per line and checked against their content.

```sh
cargo run --release                   # interactive
cargo run --release -- --bench        # scripted scroll/typing/popup run, prints JSON
xvfb-run -a ./bench.sh 5              # 5 runs, medians in results/summary.txt, idle CPU (Linux)
```

Flags: `--bench`, `--vsync`, `--system-fonts` (scan system fonts at startup), `--lines N`, `--idle-secs N`.
`WGPU_BACKEND=vulkan|metal|dx12|gl` picks the backend.

Bench timings are measured from applying an input action to frame presented and GPU idle
(`device.poll(Wait)`), with vsync off. They exclude OS input delivery and compositor/display latency.

Font: DejaVu Sans Mono in `assets/`, under the Bitstream Vera / DejaVu license. Its notice is in
[`assets/LICENSE-DejaVu`](assets/LICENSE-DejaVu) and must ship with the font wherever it is copied
(for example into `spur-gui`).
