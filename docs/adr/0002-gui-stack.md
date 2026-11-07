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

10k-line file, toy syntax highlighting on, release build, vsync off. Times are from applying an
input to the frame being presented and the GPU idle. Medians of 5 runs, warm (first run after
boot excluded). None of these machines has a real GPU; all ran on CI-class VMs or containers.

| Metric | Linux (llvmpipe, Vulkan) | macOS (paravirtual Metal) | Windows (WARP, DX12) | #12 budget |
|---|---|---|---|---|
| Spawn to first frame | 116 ms | 210 ms | 237 ms | < 150 ms |
| Keystroke to render p99 | 5.9 ms | 4.0 ms | 50 ms | < 8 ms |
| Scroll frame p99 | 5.8 ms | 5.4 ms | 46 ms | |
| Idle memory | 135 MB | 82 MB | 50 MB | < 80 MB |
| Memory before GPU init | 9.6 MB | 71 MB | 21 MB | |
| Idle CPU | 0 | (not measured) | (not measured) | ~0% |
| Binary (stripped) | 11 MB | 6.8 MB | | < 50 MB |

What the numbers say:

- **Our own work is small everywhere.** Layout and shaping take 0.1-0.3 ms per frame, glyph
  and rect upload 0.2-0.4 ms, and a full viewport reshape 1-3 ms. The rest is the GPU (or the
  software stand-in for one).
- **Windows WARP is a CPU rasterizer**: 40 ms of each frame is waiting for it. That says nothing
  about real hardware, but it is a useful worst case for machines without a usable GPU.
- **Startup is OS and driver setup**, not our code: on macOS, event loop + window creation is
  ~115 ms in the VM; GPU init is 70-80 ms on macOS/Windows and 45-200 ms on llvmpipe. The first
  frame itself is 4 ms on Metal.
- **Memory is mostly the platform.** The editor's own data is under 10 MB. macOS starts at 71 MB
  before any GPU work (AppKit), and llvmpipe adds ~125 MB.

Budget status: typing latency passes wherever there is a GPU-like path. Cold start and idle
memory need a run on real hardware before we accept or adjust the #12 GUI budgets.

## Consequences

- One rendering path on all platforms; we own the draw order, so popups, overlays and agent UI
  are just more quads and text areas.
- We depend on glyphon tracking wgpu releases (it has so far, in lockstep). If it lags, we vendor
  it (about 1.5k lines).
- GPU init is the largest startup cost. Follow-ups: create the window first and paint the
  background, init the device in parallel with loading the buffer, and use wgpu's pipeline
  cache where available.
- Idle memory and cold start must be checked on real hardware before we accept the budgets. If
  memory is over, try the GL backend and smaller swapchain/atlas sizes; for startup, overlap
  window creation, GPU init and file load.
- Still to prove in M1: AccessKit tree, IME on macOS and Windows with a real input method,
  ligatures, and wide-glyph (CJK) cursor placement.
