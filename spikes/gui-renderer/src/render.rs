//! Renderer: wgpu surface + an instanced rect pipeline + glyphon (cosmic-text + swash)
//! text. Only visible lines are shaped; shaped lines are cached by line index and
//! validated by content, so scrolling reshapes only lines entering the viewport.

use crate::editor::{COMPLETIONS, Editor, Pos};
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Wrap,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use winit::window::Window;

const FONT: &[u8] = include_bytes!("../assets/DejaVuSansMono.ttf");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RectInst {
    rect: [f32; 4],
    color: [f32; 4],
}

const RECT_WGSL: &str = r#"
struct U { screen: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };
@vertex fn vs(@builtin(vertex_index) vi: u32, @location(0) rect: vec4<f32>, @location(1) color: vec4<f32>) -> VOut {
    let c = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let p = rect.xy + c * rect.zw;
    var o: VOut;
    o.pos = vec4<f32>(p.x / u.screen.x * 2.0 - 1.0, 1.0 - p.y / u.screen.y * 2.0, 0.0, 1.0);
    o.color = color;
    return o;
}
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> { return i.color; }
"#;

const RECT_ATTRS: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4];

fn srgb(c: u32, a: f32) -> [f32; 4] {
    let f = |v: u32| {
        let s = (v & 0xff) as f32 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    [f(c >> 16), f(c >> 8), f(c), a]
}

struct CachedLine {
    src: String,
    buf: Buffer,
}

#[derive(Default, Clone, Copy)]
pub struct FrameTiming {
    pub layout_ms: f64,
    pub prepare_ms: f64,
    pub submit_ms: f64,
    pub gpu_wait_ms: f64,
    pub shaped_lines: usize,
}

pub struct Renderer {
    pub window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    rect_pipeline: wgpu::RenderPipeline,
    rect_bind: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    rect_buf: wgpu::Buffer,
    rect_cap: usize,

    pub font_system: FontSystem,
    swash: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text: TextRenderer,
    overlay_text: TextRenderer,
    lines: HashMap<usize, CachedLine>,
    gutter: Buffer,
    gutter_key: (usize, usize),
    popup_buf: Buffer,

    pub scale: f32,
    pub metrics: Metrics,
    pub cell_w: f32,
    pub wait_gpu: bool,
    pub adapter_info: String,
}

pub struct InitMarks {
    pub gpu_ms: f64,
    pub fonts_ms: f64,
}

impl Renderer {
    pub fn new(window: Arc<Window>, vsync: bool, system_fonts: bool) -> (Self, InitMarks) {
        let t = Instant::now();
        let size = window.inner_size();
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let surface = instance.create_surface(window.clone()).expect("surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .expect("adapter");
        let info = adapter.get_info();
        let adapter_info = format!("{} ({:?}, {:?})", info.name, info.backend, info.device_type);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits:
                wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .expect("device");
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let present_mode = if vsync {
            wgpu::PresentMode::AutoVsync
        } else {
            wgpu::PresentMode::AutoNoVsync
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &config);

        // Rect pipeline.
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rect"),
            source: wgpu::ShaderSource::Wgsl(RECT_WGSL.into()),
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rect uniform"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let rect_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let rect_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rect"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<RectInst>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &RECT_ATTRS,
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            cache: None,
            multiview_mask: None,
        });
        let rect_cap = 1024;
        let rect_buf = Self::make_rect_buf(&device, rect_cap);
        let gpu_ms = t.elapsed().as_secs_f64() * 1e3;

        // Text. Embedded mono font only by default: scanning system fonts costs
        // tens to hundreds of ms, so the real editor should load fallbacks lazily.
        let t = Instant::now();
        let mut font_system = if system_fonts {
            let mut fs = FontSystem::new();
            fs.db_mut().load_font_data(FONT.to_vec());
            fs
        } else {
            let mut db = glyphon::fontdb::Database::new();
            db.load_font_data(FONT.to_vec());
            FontSystem::new_with_locale_and_db("en-US".into(), db)
        };
        font_system
            .db_mut()
            .set_monospace_family("DejaVu Sans Mono");
        let fonts_ms = t.elapsed().as_secs_f64() * 1e3;

        let swash = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text = TextRenderer::new(&mut atlas, &device, Default::default(), None);
        let overlay_text = TextRenderer::new(&mut atlas, &device, Default::default(), None);

        let scale = window.scale_factor() as f32;
        let metrics = Metrics::new(14.0 * scale, (20.0 * scale).round());
        let mut probe = Buffer::new(&mut font_system, metrics);
        probe.set_text("MMMMMMMMMM", &mono(), Shaping::Advanced, None);
        probe.shape_until_scroll(&mut font_system, false);
        let cell_w = probe.layout_runs().next().map_or(8.0, |r| r.line_w / 10.0);
        let gutter = Buffer::new(&mut font_system, metrics);
        let popup_buf = Buffer::new(&mut font_system, metrics);

        let r = Self {
            window,
            surface,
            device,
            queue,
            config,
            rect_pipeline,
            rect_bind,
            uniform,
            rect_buf,
            rect_cap,
            font_system,
            swash,
            viewport,
            atlas,
            text,
            overlay_text,
            lines: HashMap::new(),
            gutter,
            gutter_key: (usize::MAX, 0),
            popup_buf,
            scale,
            metrics,
            cell_w,
            wait_gpu: false,
            adapter_info,
        };
        (r, InitMarks { gpu_ms, fonts_ms })
    }

    fn make_rect_buf(device: &wgpu::Device, cap: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rects"),
            size: (cap * std::mem::size_of::<RectInst>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        self.config.width = w.max(1);
        self.config.height = h.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    pub fn line_h(&self) -> f32 {
        self.metrics.line_height
    }
    pub fn text_left(&self) -> f32 {
        self.cell_w * 7.0 + 8.0 * self.scale
    }
    pub fn view_h(&self) -> f32 {
        self.config.height as f32
    }

    /// x offset (relative to text_left) of byte `col` in cached line `line`.
    fn x_of(&self, line: usize, col: usize) -> f32 {
        let Some(c) = self.lines.get(&line) else {
            return 0.0;
        };
        let Some(run) = c.buf.layout_runs().next() else {
            return 0.0;
        };
        for g in run.glyphs.iter() {
            if col < g.end || g.start >= col {
                return g.x;
            }
        }
        run.line_w
    }

    /// Hit test a physical pixel position to a buffer position.
    pub fn hit(&mut self, ed: &Editor, x: f32, y: f32) -> Pos {
        let line =
            (((y + ed.scroll_y) / self.line_h()).floor().max(0.0) as usize).min(ed.lines.len() - 1);
        let xr = (x - self.text_left()).max(0.0);
        let col = self
            .lines
            .get(&line)
            .and_then(|c| c.buf.hit(xr, self.line_h() * 0.5))
            .map_or(0, |c| c.index)
            .min(ed.lines[line].len());
        Pos { line, col }
    }

    pub fn ime_area(&self, ed: &Editor) -> (f32, f32, f32) {
        let x = self.text_left() + self.x_of(ed.cur.line, ed.cur.col);
        let y = ed.cur.line as f32 * self.line_h() - ed.scroll_y;
        (x, y, self.line_h())
    }

    pub fn render(&mut self, ed: &Editor) -> FrameTiming {
        let mut ft = FrameTiming::default();
        let t0 = Instant::now();
        let (w, h) = (self.config.width as f32, self.config.height as f32);
        let lh = self.line_h();
        let first = (ed.scroll_y / lh).floor() as usize;
        let rows = (h / lh).ceil() as usize + 1;
        let last = (first + rows).min(ed.lines.len());
        let y_off = ed.scroll_y - first as f32 * lh;
        let left = self.text_left();

        // Shape visible lines (cache hit if content unchanged).
        for i in first..last {
            let src = ed.display_line(i);
            let fresh = self.lines.get(&i).is_some_and(|c| c.src == *src);
            if !fresh {
                let entry = self.lines.entry(i).or_insert_with(|| CachedLine {
                    src: String::new(),
                    buf: {
                        let mut b = Buffer::new(&mut self.font_system, self.metrics);
                        b.set_wrap(Wrap::None);
                        b.set_size(None, Some(lh));
                        b
                    },
                });
                let spans = highlight(&src);
                entry.buf.set_rich_text(
                    spans
                        .iter()
                        .map(|(r, c)| (&src[r.clone()], mono().color(*c))),
                    &mono(),
                    Shaping::Advanced,
                    None,
                );
                entry.buf.shape_until_scroll(&mut self.font_system, false);
                entry.src.clear();
                entry.src.push_str(&src);
                ft.shaped_lines += 1;
            }
        }
        // Evict far-away lines so memory stays bounded.
        if self.lines.len() > rows * 4 {
            let (lo, hi) = (first.saturating_sub(rows), last + rows);
            self.lines.retain(|k, _| *k >= lo && *k < hi);
        }
        if self.gutter_key != (first, last) {
            let nums: String = (first..last).map(|i| format!("{:>6}\n", i + 1)).collect();
            self.gutter.set_size(None, Some(h + lh * 2.0));
            self.gutter.set_text(
                &nums,
                &mono().color(Color::rgb(0x6b, 0x72, 0x80)),
                Shaping::Basic,
                None,
            );
            self.gutter.shape_until_scroll(&mut self.font_system, false);
            self.gutter_key = (first, last);
        }

        // Rects: background layer then overlay layer.
        let mut rects: Vec<RectInst> = Vec::with_capacity(256);
        let rect = |x: f32, y: f32, w: f32, h: f32, c: [f32; 4]| RectInst {
            rect: [x, y, w, h],
            color: c,
        };
        rects.push(rect(
            0.0,
            0.0,
            left - 4.0 * self.scale,
            h,
            srgb(0x16181d, 1.0),
        ));
        let cur_y = ed.cur.line as f32 * lh - ed.scroll_y;
        rects.push(rect(
            left - 4.0 * self.scale,
            cur_y,
            w,
            lh,
            srgb(0x2a2e37, 1.0),
        ));
        if let Some((s, e)) = ed.selection() {
            for i in s.line.max(first)..=e.line.min(last.saturating_sub(1)) {
                let x0 = if i == s.line {
                    self.x_of(i, s.col)
                } else {
                    0.0
                };
                let x1 = if i == e.line {
                    self.x_of(i, e.col)
                } else {
                    self.x_of(i, usize::MAX) + self.cell_w
                };
                let y = (i - first) as f32 * lh - y_off;
                rects.push(rect(left + x0, y, x1 - x0, lh, srgb(0x264f78, 1.0)));
            }
        }
        let bg_count = rects.len();
        let caret_x = left + self.x_of(ed.cur.line, ed.cur.col);
        if let Some((p, _)) = &ed.preedit
            && !p.is_empty()
        {
            let x1 = left + self.x_of(ed.cur.line, ed.cur.col + p.len());
            rects.push(rect(
                caret_x,
                cur_y + lh - 2.0 * self.scale,
                x1 - caret_x,
                1.0 * self.scale,
                srgb(0xe5c07b, 1.0),
            ));
        }
        let caret_extra = ed
            .preedit
            .as_ref()
            .map_or(0, |(p, c)| c.map_or(p.len(), |c| c.0));
        let cx = left + self.x_of(ed.cur.line, ed.cur.col + caret_extra);
        rects.push(rect(cx, cur_y, 2.0 * self.scale, lh, srgb(0x61afef, 1.0)));

        let mut popup_at = None;
        if let Some(sel) = ed.popup {
            let pw = self.cell_w * 22.0;
            let ph = lh * COMPLETIONS.len() as f32 + 8.0 * self.scale;
            let mut px = cx.min(w - pw - 4.0);
            let mut py = cur_y + lh + 2.0;
            if py + ph > h {
                py = cur_y - ph - 2.0;
            }
            px = px.max(0.0);
            py = py.clamp(0.0, (h - ph).max(0.0));
            let b = self.scale;
            rects.push(rect(
                px - b,
                py - b,
                pw + 2.0 * b,
                ph + 2.0 * b,
                srgb(0x3e4451, 1.0),
            ));
            rects.push(rect(px, py, pw, ph, srgb(0x21252b, 0.98)));
            rects.push(rect(
                px,
                py + 4.0 * self.scale + sel as f32 * lh,
                pw,
                lh,
                srgb(0x2c313c, 1.0),
            ));
            popup_at = Some((px, py, pw, ph));
            let items: String = COMPLETIONS.iter().map(|c| format!(" ƒ {c}\n")).collect();
            self.popup_buf.set_size(Some(pw), Some(ph));
            self.popup_buf
                .set_text(&items, &mono(), Shaping::Advanced, None);
            self.popup_buf
                .shape_until_scroll(&mut self.font_system, false);
        }
        ft.layout_ms = t0.elapsed().as_secs_f64() * 1e3;

        // Prepare GPU data.
        let t1 = Instant::now();
        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.config.width,
                height: self.config.height,
            },
        );
        let bounds = TextBounds {
            left: left as i32,
            top: 0,
            right: w as i32,
            bottom: h as i32,
        };
        let areas = (first..last).filter_map(|i| {
            let c = self.lines.get(&i)?;
            Some(TextArea {
                buffer: &c.buf,
                left,
                top: (i - first) as f32 * lh - y_off,
                scale: 1.0,
                bounds,
                default_color: Color::rgb(0xab, 0xb2, 0xbf),
                custom_glyphs: &[],
            })
        });
        let gutter = TextArea {
            buffer: &self.gutter,
            left: 0.0,
            top: -y_off,
            scale: 1.0,
            bounds: TextBounds {
                left: 0,
                top: 0,
                right: left as i32,
                bottom: h as i32,
            },
            default_color: Color::rgb(0x6b, 0x72, 0x80),
            custom_glyphs: &[],
        };
        self.text
            .prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                areas.chain(std::iter::once(gutter)),
                &mut self.swash,
            )
            .expect("prepare text");
        let overlay = popup_at.map(|(px, py, pw, ph)| TextArea {
            buffer: &self.popup_buf,
            left: px,
            top: py + 4.0 * self.scale,
            scale: 1.0,
            bounds: TextBounds {
                left: px as i32,
                top: py as i32,
                right: (px + pw) as i32,
                bottom: (py + ph) as i32,
            },
            default_color: Color::rgb(0xd7, 0xda, 0xe0),
            custom_glyphs: &[],
        });
        self.overlay_text
            .prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                overlay,
                &mut self.swash,
            )
            .expect("prepare overlay");
        if rects.len() > self.rect_cap {
            self.rect_cap = rects.len().next_power_of_two();
            self.rect_buf = Self::make_rect_buf(&self.device, self.rect_cap);
        }
        self.queue
            .write_buffer(&self.rect_buf, 0, bytemuck::cast_slice(&rects));
        self.queue
            .write_buffer(&self.uniform, 0, bytemuck::cast_slice(&[w, h, 0.0, 0.0]));
        ft.prepare_ms = t1.elapsed().as_secs_f64() * 1e3;

        // Encode + present.
        let t2 = Instant::now();
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            _ => {
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                return ft;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear({
                            let c = srgb(0x1e2127, 1.0);
                            wgpu::Color {
                                r: c[0] as f64,
                                g: c[1] as f64,
                                b: c[2] as f64,
                                a: 1.0,
                            }
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.rect_pipeline);
            pass.set_bind_group(0, &self.rect_bind, &[]);
            pass.set_vertex_buffer(0, self.rect_buf.slice(..));
            pass.draw(0..4, 0..bg_count as u32);
            self.text
                .render(&self.atlas, &self.viewport, &mut pass)
                .expect("render text");
            pass.set_pipeline(&self.rect_pipeline);
            pass.set_bind_group(0, &self.rect_bind, &[]);
            pass.set_vertex_buffer(0, self.rect_buf.slice(..));
            pass.draw(0..4, bg_count as u32..rects.len() as u32);
            self.overlay_text
                .render(&self.atlas, &self.viewport, &mut pass)
                .expect("render overlay");
        }
        self.queue.submit(Some(enc.finish()));
        self.window.pre_present_notify();
        self.queue.present(frame);
        ft.submit_ms = t2.elapsed().as_secs_f64() * 1e3;
        if self.wait_gpu {
            let t3 = Instant::now();
            let _ = self.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            });
            ft.gpu_wait_ms = t3.elapsed().as_secs_f64() * 1e3;
        }
        self.atlas.trim();
        ft
    }
}

fn mono() -> Attrs<'static> {
    Attrs::new().family(Family::Monospace)
}

const KEYWORDS: &[&str] = &[
    "pub", "fn", "let", "mut", "if", "else", "for", "while", "return", "self", "struct", "impl",
    "as", "match", "use", "mod", "const",
];

/// Toy lexer standing in for tree-sitter highlighting: produces colored spans so the
/// shaping path handles multi-attribute lines like it will with real highlighting.
fn highlight(s: &str) -> Vec<(std::ops::Range<usize>, Color)> {
    let (kw, st, num, com, typ, def) = (
        Color::rgb(0xc6, 0x78, 0xdd),
        Color::rgb(0x98, 0xc3, 0x79),
        Color::rgb(0xd1, 0x9a, 0x66),
        Color::rgb(0x5c, 0x63, 0x70),
        Color::rgb(0xe5, 0xc0, 0x7b),
        Color::rgb(0xab, 0xb2, 0xbf),
    );
    let b = s.as_bytes();
    let mut out: Vec<(std::ops::Range<usize>, Color)> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let c = b[i];
        let color = if c == b'/' && b.get(i + 1) == Some(&b'/') {
            i = b.len();
            com
        } else if c == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                i += 1;
            }
            i = (i + 1).min(b.len());
            st
        } else if c.is_ascii_digit() {
            while i < b.len() && b[i].is_ascii_alphanumeric() {
                i += 1;
            }
            num
        } else if c.is_ascii_alphabetic() || c == b'_' {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let w = &s[start..i];
            if KEYWORDS.contains(&w) {
                kw
            } else if w.starts_with(|c: char| c.is_ascii_uppercase()) {
                typ
            } else {
                def
            }
        } else {
            i += 1;
            while i < b.len() && !s.is_char_boundary(i) {
                i += 1;
            }
            def
        };
        match out.last_mut() {
            Some((r, c)) if *c == color && r.end == start => r.end = i,
            _ => out.push((start..i, color)),
        }
    }
    if out.is_empty() {
        out.push((0..0, def));
    }
    out
}
