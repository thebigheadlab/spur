//! Spur GUI renderer spike (#28). Interactive by default; `--bench` runs a scripted
//! scroll/typing/popup session through the same input path and prints JSON results.
//!
//! Flags: --bench  --vsync  --system-fonts  --lines N  --idle-secs N (interactive: exit after N s)

mod editor;
mod render;

use editor::{Action, Dir, Editor};
use render::{FrameTiming, Renderer};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

struct Opts {
    bench: bool,
    vsync: bool,
    system_fonts: bool,
    lines: usize,
    idle_secs: Option<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Scroll,
    Jump,
    Typing,
    Nav,
    Structural,
    Popup,
}

struct Bench {
    steps: Vec<(Phase, Action)>,
    next: usize,
    pending: Option<(Phase, Instant)>,
    samples: Vec<(Phase, f64, FrameTiming)>,
    warmup: usize,
}

struct App {
    opts: Opts,
    t_main: Instant,
    editor: Editor,
    r: Option<Renderer>,
    mods: ModifiersState,
    mouse: PhysicalPosition<f64>,
    dragging: bool,
    bench: Option<Bench>,
    marks: Vec<(&'static str, f64)>,
    first_frame: bool,
    idle_mem: Option<u64>,
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn mem_mb() -> f64 {
    memory_stats::memory_stats().map_or(0.0, |m| m.physical_mem as f64 / 1048576.0)
}

impl App {
    fn act(&mut self, a: Action) {
        let Some(r) = &self.r else { return };
        let (lh, vh) = (r.line_h(), r.view_h());
        self.editor.apply(a, lh, vh);
        let (x, y, h) = r.ime_area(&self.editor);
        r.window
            .set_ime_cursor_area(PhysicalPosition::new(x, y), PhysicalSize::new(1.0, h));
        r.window.request_redraw();
    }

    fn key(&mut self, ev: winit::event::KeyEvent) {
        if ev.state != ElementState::Pressed || self.editor.preedit.is_some() {
            return;
        }
        let shift = self.mods.shift_key();
        let ctrl = self.mods.control_key() || self.mods.super_key();
        let a = match &ev.logical_key {
            Key::Named(n) => match n {
                NamedKey::ArrowLeft => Action::Move(Dir::Left, shift),
                NamedKey::ArrowRight => Action::Move(Dir::Right, shift),
                NamedKey::ArrowUp => Action::Move(Dir::Up, shift),
                NamedKey::ArrowDown => Action::Move(Dir::Down, shift),
                NamedKey::Home if ctrl => Action::DocStart,
                NamedKey::End if ctrl => Action::DocEnd,
                NamedKey::Home => Action::Move(Dir::Home, shift),
                NamedKey::End => Action::Move(Dir::End, shift),
                NamedKey::PageUp => Action::Page(-1),
                NamedKey::PageDown => Action::Page(1),
                NamedKey::Backspace => Action::Backspace,
                NamedKey::Delete => Action::Delete,
                NamedKey::Escape => Action::Escape,
                NamedKey::Enter | NamedKey::Tab if self.editor.popup.is_some() => {
                    Action::PopupAccept
                }
                NamedKey::Enter => Action::Newline,
                NamedKey::Tab => Action::Insert("    ".into()),
                NamedKey::Space if ctrl => Action::TogglePopup,
                NamedKey::Space => Action::Insert(" ".into()),
                _ => return,
            },
            Key::Character(c) if ctrl => match c.as_str() {
                "a" => Action::SelectAll,
                " " => Action::TogglePopup,
                _ => return,
            },
            _ => match ev.text.as_deref() {
                Some(t) if !t.chars().any(char::is_control) => Action::Insert(t.to_owned()),
                _ => return,
            },
        };
        self.act(a);
    }

    fn bench_steps(&self) -> Vec<(Phase, Action)> {
        let mut s = Vec::new();
        // Smooth scroll: ~0.68 line per frame, so a new line enters most frames.
        for _ in 0..1200 {
            s.push((Phase::Scroll, Action::ScrollBy(13.7)));
        }
        for _ in 0..30 {
            s.push((Phase::Jump, Action::Page(1)));
        }
        s.push((Phase::Jump, Action::DocEnd));
        s.push((Phase::Jump, Action::DocStart));
        s.push((Phase::Jump, Action::GotoLine(self.editor.lines.len() / 2)));
        s.push((Phase::Nav, Action::Move(Dir::Home, false)));
        for (i, ch) in "    let typed_value = compute(alpha, beta) + 42; // spike "
            .chars()
            .cycle()
            .take(400)
            .enumerate()
        {
            s.push((Phase::Typing, Action::Insert(ch.to_string())));
            // No wrap or horizontal scroll in the spike: move to the next line
            // every 40 chars so typed text stays inside the viewport.
            if i % 40 == 39 {
                s.push((Phase::Nav, Action::Move(Dir::Down, false)));
                s.push((Phase::Nav, Action::Move(Dir::Home, false)));
            }
            if i % 37 == 36 {
                for _ in 0..3 {
                    s.push((Phase::Typing, Action::Backspace));
                }
            }
        }
        for _ in 0..40 {
            s.push((Phase::Structural, Action::Newline));
            s.push((Phase::Structural, Action::Insert("x".into())));
        }
        for _ in 0..40 {
            s.push((Phase::Structural, Action::Backspace));
            s.push((Phase::Structural, Action::Backspace));
        }
        for _ in 0..20 {
            s.push((Phase::Popup, Action::TogglePopup));
            s.push((Phase::Popup, Action::PopupMove(1)));
            s.push((Phase::Popup, Action::PopupMove(1)));
            s.push((Phase::Popup, Action::PopupAccept));
        }
        s
    }

    fn bench_tick(&mut self, ft: FrameTiming, el: &ActiveEventLoop) {
        let Some(b) = self.bench.as_mut() else { return };
        if let Some((p, t)) = b.pending.take() {
            if b.warmup > 0 {
                b.warmup -= 1;
            } else {
                b.samples.push((p, ms(t), ft));
            }
        }
        if b.next >= b.steps.len() {
            self.report();
            el.exit();
            return;
        }
        let (p, a) = b.steps[b.next].clone();
        b.next += 1;
        b.pending = Some((p, Instant::now()));
        self.act(a);
    }

    fn report(&self) {
        let b = self.bench.as_ref().unwrap();
        let r = self.r.as_ref().unwrap();
        let mut phases = String::new();
        for p in [
            Phase::Scroll,
            Phase::Jump,
            Phase::Typing,
            Phase::Nav,
            Phase::Structural,
            Phase::Popup,
        ] {
            let mut v: Vec<f64> = b.samples.iter().filter(|s| s.0 == p).map(|s| s.1).collect();
            if v.is_empty() {
                continue;
            }
            let n = v.len();
            let shaped: usize = b
                .samples
                .iter()
                .filter(|s| s.0 == p)
                .map(|s| s.2.shaped_lines)
                .sum();
            let avg = |f: fn(&FrameTiming) -> f64| {
                b.samples
                    .iter()
                    .filter(|s| s.0 == p)
                    .map(|s| f(&s.2))
                    .sum::<f64>()
                    / n as f64
            };
            let (lay, prep, sub, gpu) = (
                avg(|t| t.layout_ms),
                avg(|t| t.prepare_ms),
                avg(|t| t.submit_ms),
                avg(|t| t.gpu_wait_ms),
            );
            v.sort_by(f64::total_cmp);
            let q = |x: f64| v[((n as f64 - 1.0) * x).round() as usize];
            phases.push_str(&format!(
                "    \"{p:?}\": {{\"n\": {n}, \"p50\": {:.3}, \"p95\": {:.3}, \"p99\": {:.3}, \"max\": {:.3}, \"avg_layout\": {lay:.3}, \"avg_prepare\": {prep:.3}, \"avg_submit\": {sub:.3}, \"avg_gpu_wait\": {gpu:.3}, \"shaped_lines\": {shaped}}},\n",
                q(0.5), q(0.95), q(0.99), v[n - 1]
            ));
        }
        let marks: Vec<String> = self
            .marks
            .iter()
            .map(|(k, v)| format!("\"{k}\": {v:.2}"))
            .collect();
        let size = r.window.inner_size();
        println!(
            "{{\n  \"os\": \"{}\", \"adapter\": \"{}\", \"window\": [{}, {}], \"scale\": {}, \"lines\": {}, \"vsync\": {},\n  \"startup_ms\": {{{}}},\n  \"idle_mem_mb\": {:.1}, \"end_mem_mb\": {:.1},\n  \"frame_ms\": {{\n{}    \"_\": null\n  }}\n}}",
            std::env::consts::OS,
            r.adapter_info,
            size.width,
            size.height,
            r.scale,
            self.editor.lines.len(),
            self.opts.vsync,
            marks.join(", "),
            self.idle_mem.unwrap_or(0) as f64 / 1048576.0,
            mem_mb(),
            phases
        );
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.r.is_some() {
            return;
        }
        self.marks.push(("event_loop", ms(self.t_main)));
        let t = Instant::now();
        let window = Arc::new(
            el.create_window(
                Window::default_attributes()
                    .with_title("spur gui spike")
                    .with_inner_size(LogicalSize::new(1280.0, 800.0)),
            )
            .expect("window"),
        );
        window.set_ime_allowed(true);
        self.marks.push(("window", ms(t)));
        self.marks.push(("mem_before_gpu_mb", mem_mb()));
        let (mut r, im) = Renderer::new(window, self.opts.vsync, self.opts.system_fonts);
        r.wait_gpu = self.opts.bench;
        self.marks.push(("gpu_init", im.gpu_ms));
        self.marks.push(("font_init", im.fonts_ms));
        self.marks.push(("mem_after_init_mb", mem_mb()));
        r.window.request_redraw();
        self.r = Some(r);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, ev: WindowEvent) {
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(s) => {
                if let Some(r) = &mut self.r {
                    r.resize(s.width, s.height);
                    r.window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                let Some(r) = &mut self.r else { return };
                let t = Instant::now();
                let ft = r.render(&self.editor);
                if !self.first_frame {
                    self.first_frame = true;
                    // Ensure the first frame is actually done on the GPU before stamping.
                    self.marks.push(("first_frame_render", ms(t)));
                    self.marks.push(("first_frame_total", ms(self.t_main)));
                    let ns = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_nanos();
                    if self.opts.bench {
                        eprintln!("first_frame_epoch_ns={ns}");
                    }
                    self.idle_mem = memory_stats::memory_stats().map(|m| m.physical_mem as u64);
                    if let Some(s) = self.opts.idle_secs {
                        el.set_control_flow(ControlFlow::WaitUntil(
                            Instant::now() + Duration::from_secs(s),
                        ));
                    }
                }
                if self.bench.is_some() {
                    self.bench_tick(ft, el);
                }
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::KeyboardInput { event, .. } => self.key(event),
            WindowEvent::Ime(ime) => match ime {
                Ime::Preedit(s, c) => {
                    self.editor.preedit = if s.is_empty() { None } else { Some((s, c)) };
                    self.act(Action::ScrollBy(0.0));
                }
                Ime::Commit(s) => {
                    self.editor.preedit = None;
                    self.act(Action::Insert(s));
                }
                Ime::Disabled => {
                    self.editor.preedit = None;
                    self.act(Action::ScrollBy(0.0));
                }
                Ime::Enabled => {}
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let lh = self.r.as_ref().map_or(20.0, |r| r.line_h());
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y * 3.0 * lh,
                    MouseScrollDelta::PixelDelta(p) => -p.y as f32,
                };
                self.act(Action::ScrollBy(dy));
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = position;
                if self.dragging
                    && let Some(r) = &mut self.r
                {
                    let p = r.hit(&self.editor, position.x as f32, position.y as f32);
                    self.act(Action::Drag(p));
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                self.dragging = state == ElementState::Pressed;
                if self.dragging
                    && let Some(r) = &mut self.r
                {
                    let p = r.hit(&self.editor, self.mouse.x as f32, self.mouse.y as f32);
                    self.act(Action::Click(p, self.mods.shift_key()));
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if let (Some(s), true) = (self.opts.idle_secs, self.first_frame)
            && self.t_main.elapsed() >= Duration::from_secs(s)
        {
            el.exit();
        }
    }
}

fn main() {
    let t_main = Instant::now();
    let args: Vec<String> = std::env::args().collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let val = |f: &str| {
        args.iter()
            .position(|a| a == f)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
    };
    let opts = Opts {
        bench: flag("--bench"),
        vsync: flag("--vsync"),
        system_fonts: flag("--system-fonts"),
        lines: val("--lines").unwrap_or(10_000),
        idle_secs: val("--idle-secs").map(|v: usize| v as u64),
    };
    let editor = Editor::new(editor::synthetic_source(opts.lines));
    let el = EventLoop::new().expect("event loop");
    el.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        t_main,
        editor,
        r: None,
        mods: ModifiersState::empty(),
        mouse: PhysicalPosition::new(0.0, 0.0),
        dragging: false,
        bench: None,
        marks: Vec::new(),
        first_frame: false,
        idle_mem: None,
        opts,
    };
    if app.opts.bench {
        let steps = app.bench_steps();
        app.bench = Some(Bench {
            steps,
            next: 0,
            pending: None,
            samples: Vec::new(),
            warmup: 5,
        });
    }
    el.run_app(&mut app).expect("run");
}
