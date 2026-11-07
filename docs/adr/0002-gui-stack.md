# 2. GUI stack: winit + wgpu + cosmic-text/swash (via glyphon) + AccessKit

- Status: proposed
- Date: 2026-11-07
- Issues: #28 (this decision), #7 (GUI spec), #12 (budgets)

## Context

The desktop frontend must be native, fast on macOS/Windows/Linux, lightweight, and simple
enough to extend (plugins and agents can add UI). The owner approved wgpu on 2026-11-07, so
the open questions were the text stack (parley vs cosmic-text) and whether the stack can meet
the #12 GUI budgets. A prototype in `spikes/gui-renderer` answers both.

## Decision

| Concern | Choice |
|---|---|
| Windowing, input, IME | `winit` 0.30 (move to 0.31 once stable) |
| Rendering | `wgpu` 30: Metal / DX12 / Vulkan, GL fallback |
| Shaping + layout | `cosmic-text` (harfrust shaping, fontdb fallback, swash raster) |
| Glyph atlas + text draw | `glyphon` (cosmic-text to wgpu atlas) |
| Everything else | Our own instanced-quad pipeline (rects, carets, selections, popups) |
| Accessibility | `accesskit` + `accesskit_winit` (not in the prototype) |

Rules that come with it:

1. **Shape only what is visible.** One cached shaped buffer per visible line, keyed by line and
   checked against content; lines entering the viewport are shaped, the rest are evicted.
2. **Embed one monospace font; load system fonts lazily.** Scanning system fonts at startup cost
   78 ms and ~25 MB in the prototype. Fallback fonts (CJK, emoji) load on first miss, off the
   main thread.
3. **Event-driven only.** `ControlFlow::Wait`, redraw on change. Idle CPU measured at 0.
4. **Text behind a small trait in `spur-gui`.** cosmic-text and glyphon types stay inside the
   renderer, so we can swap to parley or our own atlas without touching editor code.

### Why cosmic-text over parley

- cosmic-text is built for editors: per-line buffers, hit testing, cursor positions, and font
  fallback out of the box. glyphon gives a ready wgpu atlas for it.
- parley is stronger for rich UI text (inline boxes, styled paragraphs) but has no
  off-the-shelf wgpu atlas; we would write the atlas ourselves.
- With rule 4, switching later is contained. Revisit if we need rich UI text in panels.

## Prototype results

10k-line file, 1280x800 window, toy syntax highlighting on, release build, vsync off. Times are
from applying an input to the frame being presented and the GPU idle. Median of 5 runs.

**Linux, Mesa llvmpipe (software Vulkan, no GPU) under Xvfb:**

| Metric | Result | #12 budget |
|---|---|---|
| Spawn to first frame | 116 ms (gpu init 67, first frame 26) | < 150 ms |
| Keystroke to render, p50 / p99 | 3.9 / 5.7 ms | < 8 ms p99 |
| Smooth scroll frame, p50 / p99 | 3.9 / 5.8 ms | (16 ms at 60 Hz) |
| Page jump (full viewport reshape), p99 | 7.6 ms | |
| Idle memory | 135 MB (9.6 MB before GPU init) | < 80 MB |
| Idle CPU | 0 ms over 8 s | ~0% |
| Binary size (stripped, thin LTO) | 11 MB | < 50 MB |

Our CPU work per frame is small: layout and shaping 0.2 ms, glyph/rect upload 0.3 ms. A full
viewport reshape (about 40 lines, advanced shaping) is about 1.9 ms. Most of each frame is the
software rasterizer and present copy, which a real GPU removes.

Memory is the one miss, and it comes from the llvmpipe driver (about 125 MB after GPU init);
the editor itself is under 10 MB. Real-GPU numbers on macOS and Windows come from the
`spike-gui-renderer` CI workflow and a run on owner hardware; they are recorded on #28.

## Consequences

- One rendering path on all platforms; we own the draw order, so popups, overlays and agent UI
  are just more quads and text areas.
- We depend on glyphon tracking wgpu releases (it has so far, in lockstep). If it lags, we vendor
  it (about 1.5k lines).
- GPU init is the largest startup cost. Follow-ups: create the window first and paint the
  background, init the device in parallel with loading the buffer, and use wgpu's pipeline
  cache where available.
- Idle memory must be checked on real drivers before we accept the budget. If it is over, try
  the GL backend and smaller swapchain/atlas sizes.
- Still to prove in M1: AccessKit tree, IME on macOS and Windows with a real input method,
  ligatures, and wide-glyph (CJK) cursor placement.
