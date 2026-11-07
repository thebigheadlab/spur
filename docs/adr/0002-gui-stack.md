# 2. GUI renderer and text stack: winit + wgpu + cosmic-text (via glyphon)

- Status: proposed
- Date: 2026-11-07
- Issues: [#28](https://github.com/thebigheadlab/spur/issues/28) (this decision),
  [#7](https://github.com/thebigheadlab/spur/issues/7) (GUI spec),
  [#12](https://github.com/thebigheadlab/spur/issues/12) (budgets)

## Context

The desktop frontend must be native, fast on macOS, Windows and Linux, and lightweight. The
owner approved wgpu as the GPU layer in [#28](https://github.com/thebigheadlab/spur/issues/28).
This record answers two narrower questions with a prototype (`spikes/gui-renderer`):

1. Which windowing, rendering and text crates the GUI frontend is built on.
2. Whether that stack plausibly fits the #12 GUI budgets.

**Out of scope (later ADRs):** the widget, layout and focus model (menus, tabs, panels, agent
UI, diff and markdown views), and whether it is shared with the TUI; accessibility tree design;
the CPU-only fallback policy. This record decides the drawing and text layer underneath them.

## Decision

| Concern | Choice |
|---|---|
| Windowing, input, IME | `winit` |
| GPU rendering | `wgpu` (Metal, DX12, Vulkan). The GL backend is best-effort, not a tested target. |
| Editor text shaping and layout | `cosmic-text` (HarfRust shaping, fontdb, swash raster). **Provisional**, see below. |
| Glyph atlas and text drawing | `glyphon` |
| Other primitives | Our own instanced-quad pipeline (rects, carets, selections, popups) |
| Accessibility | `accesskit` + `accesskit_winit` (planned; not prototyped) |

Exact versions live in `Cargo.toml`, not here.

Principles that come with it:

1. **Shape only what is visible**, and cache shaped lines by content, styling and font config,
   so highlighting or theme changes reshape a line even when its text did not change.
   *Not prototyped:* the spike caches by line index and compares the line's text. Inserting or
   deleting a line therefore reshapes every visible line below it, and a theme change
   invalidates nothing. The "structural" numbers below are a full-viewport reshape.
2. **No system font scan at startup.** Embed one monospace font. Fallback fonts (CJK, emoji)
   load lazily. Scanning cost 78 ms and about 25 MB on Linux in the prototype.
3. **Render on demand.** No continuous redraw loop; skip frames while occluded. Idle CPU 0.
4. **Renderer internals stay private to `spur-gui`.** Extensions and agents never draw quads
   or touch text-stack types. They contribute UI through the core command and render-snapshot
   model, which the GUI draws and the TUI degrades. This keeps extension UI working on both
   frontends and over remote sessions.

### Alternatives considered

| Option | Why not (for now) |
|---|---|
| gpui-ce (Zed's framework) | Most complete, but a large framework with its own app model and platform layer; harder to keep the core frontend-agnostic and the binary small. Fork risk. |
| iced, Slint, Makepad | Opinionated retained or reactive UI models that would own our event loop and layout; editor-surface performance is ours to tune either way. |
| Xilem / Masonry on Vello | Vello needs compute shaders, which suit the GL and software paths poorly; Masonry is still young. |
| Skia (skia-safe), femtovg | Skia is a large C++ build and binary cost. femtovg is GL-centric. Neither helps text layout. |
| Native toolkits (AppKit, WinUI, GTK) | Three UI codebases; no shared rendering with predictable latency. |
| parley instead of cosmic-text | Stronger for rich text (styled paragraphs, inline boxes) but no ready wgpu atlas. Not prototyped. |

Cost we take on by not adopting a framework: our own layout, focus, hit testing, scrolling
containers, menus and accessibility tree. The widget ADR must size this (consider `taffy` for
layout, `muda` for native menus and `rfd` for file dialogs).

### Text stack is provisional

cosmic-text is chosen for the **editor surface** because it is built for editors (per-line
buffers, hit testing, cursor positions) and glyphon gives it a wgpu atlas. The spike only
measured throughput, not the #7 selection criteria. Before M1 exits, cosmic-text must pass:

- CJK and emoji fallback quality, using lazily loaded system fonts;
- per-range ligature control (ligatures off under the cursor);
- IME preedit styling (underline, selected range, hidden cursor);
- shaping throughput in lines/ms on a 10k-line file.

If it fails, re-evaluate against parley. Rich UI text in panels (markdown, agent panel, hover
docs) is **not** decided here; the widget ADR decides whether it shares this stack, to avoid two
text stacks by accident. We may also outgrow glyphon's atlas (#7 asks for subpixel AA and
images); if so we own the atlas and keep cosmic-text.

## Prototype results

Setup: 10k-line synthetic file, toy syntax highlighting, release build, 1280x800 logical
window, vsync off. CI-class machines only: **none has a real GPU**.

**What is timed:** from applying an input action to the frame being presented and the GPU idle
(`device.poll(Wait)`). This is an internal metric. It excludes OS input delivery, compositor
and display scanout, and the vsync wait, so it is **not** the #12 key-to-photon budget.

**How:** `bench.sh 5` runs five times; every run counts; `results/summary.txt` reports medians
across runs (per-phase p99 is computed inside each run, nearest-rank). Memory is process RSS
right after the first frame, which on macOS includes shared framework pages and on Linux
llvmpipe's code, so it overstates our own footprint.

The table below predates that aggregation: its numbers were read from individual CI runs, so
treat them as indicative. The next real-hardware run with `bench.sh` replaces them.

| Metric | Linux (llvmpipe, Vulkan) | macOS (paravirtual Metal) | Windows (WARP, DX12) | #12 budget |
|---|---|---|---|---|
| Spawn to first frame (warm) | 116 ms | 210 ms | 237 ms ¹ | cold start < 150 ms (different metric) |
| Input applied to presented, typing p99 | 5.9 ms | 4.0 ms | 50 ms | key-to-photon < 8 ms: **not measured** |
| Input applied to presented, scroll p99 | 5.8 ms | 5.4 ms | 46 ms | |
| RSS after first frame | 135 MB | 82 MB | 50 MB | idle < 80 MB (metric to define) |
| RSS before GPU init | 9.6 MB | 71 MB | 21 MB | |
| Idle CPU | 0 (Linux, 8 s sample) | not measured | not measured | ~0% |
| Binary (stripped) | 11 MB | 6.8 MB | not recorded | < 50 MB |

¹ Spawn on Windows is timed from Git Bash, which adds process-spawn overhead.

What the numbers say:

- **Our per-frame CPU work is small**: average layout and shaping 0.1-0.3 ms, glyphon prepare
  (including rasterising atlas misses) 0.2-0.4 ms, a full viewport reshape 1-3 ms. The rest is
  the GPU or its software stand-in.
- **No-GPU machines are slow**: WARP spends about 40 ms of each frame rasterising on the CPU.
  Windows VMs and RDP sessions look like this; the fallback policy is an open question below.
- **Startup is dominated by OS and driver setup** (window creation, GPU init), not our code.
- **Memory is mostly platform and driver RSS.** Whether real drivers do better is not shown.
  RSS also grows by about 14 MB over a 3k-step bench run (117 to 131 MB on Linux) although the
  line cache is evicted. This is likely glyph-atlas growth (`atlas.trim()` frees only unused
  glyphs) or allocator retention; it needs explaining before an idle memory budget is set.
- **Known spike gaps:** the IME cursor area is computed from the previous frame's layout, so it
  lags one frame (the risk below); the scale factor is read once, and `ScaleFactorChanged` is not
  handled, so moving to a monitor with a different DPI gives wrong metrics.

**Budget status: open.** The stack is accepted for building on. Typing, startup and memory
budgets are unverified until there is a key-to-photon measurement and a run on real hardware.

## Risks and how we handle them

| Risk | Handling |
|---|---|
| Key-to-photon over budget with vsync and a compositor | Split #12 into internal and end-to-end budgets; build a Typometer-style harness in M1. |
| No usable GPU (VMs, RDP, old GPUs) | Detect a CPU adapter and tell the user. Decide in M1 between a CPU path (tiny-skia + softbuffer) and pointing users to the TUI. |
| IME (unproven on all three OSes; no IME in CI) | Manual test with real input methods (Japanese, Chinese, Korean) per OS in M1. Set the IME cursor area from the new layout, not the previous frame. |
| Accessibility of a custom-drawn 10k-line editor | Virtualised AccessKit tree; prototype with VoiceOver, NVDA and Orca in M1. |
| Wayland (only X11 via Xvfb was run) | Test client-side decorations, fractional scaling and text-input-v3 in M1. |
| HiDPI and mixed DPI | Handle scale-factor changes (font metrics, atlas). Not prototyped. |
| Frame pacing and battery | Production present mode: Mailbox, else Fifo. Low-power adapter by default. 120 Hz pacing is unproven with winit. |
| GPU device or surface loss | Recreate the surface on loss; recover the device on loss rather than panic. |
| glyphon lagging wgpu releases | Vendor it (about 1.5k lines) or own the atlas. |
| cosmic-text fails the criteria above | Swap behind the renderer boundary (principle 4) to parley. |

**What would reverse this decision:** IME or accessibility unworkable on winit for a major OS,
or real-hardware memory and startup far over budget even after the follow-ups below. Fallbacks
are a gpui-ce fork or platform text rasterisers (CoreText, DirectWrite).

## Consequences

- One drawing path on all platforms, and we own the draw order.
- Startup follow-ups (one tracking issue): overlap window creation, GPU init and file load;
  use wgpu's pipeline cache where the backend supports it (Vulkan today).
- Real-hardware runs (macOS, Windows, Linux with a discrete or integrated GPU) are the gate
  before #12 GUI budgets are accepted or adjusted.
