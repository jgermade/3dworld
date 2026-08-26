//! The window, the surface and the chrome.
//!
//! Thin by construction. Everything here that a test could have caught lives
//! in [`crate::editor`] instead, so what is left is the part that genuinely
//! needs a display: a winit event loop, a swapchain, and egui.
//!
//! **egui draws in the same render pass as the scene.** That is not a detail —
//! it is the reason egui was chosen over a DOM framework at all, recorded in
//! `STACK.md`: on the desktop, compositing a webview over a wgpu surface has
//! only two answers and both are bad. One surface, one pass, nothing to
//! composite.

use std::sync::Arc;

use w3d_core::kernel::{BooleanOp, GeometryKernel};
use w3d_render::{Gpu, PickPending, Renderer, Viewport};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use crate::editor::{Button, Command, Editor, Input, Reaction};
use crate::scene::Scene;

/// How the modeller was asked to run. A window that closes itself after a
/// fixed number of frames is what makes this crate testable on a machine with
/// no display but a virtual framebuffer.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Quit after this many frames. `None` runs until closed.
    pub frames: Option<u32>,
    /// Write the last presented frame here, as a binary PPM.
    pub screenshot: Option<String>,
    /// Commands to run at startup, so a screenshot has something in it.
    pub startup: Vec<Command>,
    /// A document to open before anything else.
    pub open: Option<std::path::PathBuf>,
    /// A STEP file to import into the document once it is open. An option
    /// rather than a command because importing needs a file that exists and
    /// there is no file dialogue to name one with — see [`Command::ExportStep`].
    pub import_step: Option<std::path::PathBuf>,
    /// Write the document as STEP once the startup commands have run. The
    /// headless half of the export button, and what makes a STEP file
    /// something a test can produce and read back.
    pub export_step: Option<std::path::PathBuf>,
    /// Save here once the startup commands have run, then carry on. Exists so
    /// that a headless run can produce a file to check.
    pub save_as: Option<std::path::PathBuf>,
}

struct Live<K: GeometryKernel> {
    window: Arc<Window>,
    gpu: Gpu,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    depth: wgpu::Texture,
    renderer: Renderer,
    scene: Scene,
    egui: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    editor: Editor<K>,
    pending: Option<(PickPending, bool)>,
    modifiers: ModifiersState,
    cursor: (f64, f64),
    /// Where `--screenshot` copies the frame *before* it is presented. A
    /// presented surface texture is destroyed, so reading one back afterwards
    /// is a validation error — the copy has to happen in the same encoder that
    /// drew it, which also makes it exactly what was shown.
    capture: Option<wgpu::Texture>,
    frames: u32,
}

pub struct Shell<K: GeometryKernel> {
    options: Options,
    kernel: Option<K>,
    live: Option<Live<K>>,
    pub exit_code: i32,
}

impl<K: GeometryKernel> Shell<K> {
    pub fn new(kernel: K, options: Options) -> Self {
        Self {
            options,
            kernel: Some(kernel),
            live: None,
            exit_code: 0,
        }
    }
}

const INITIAL: (u32, u32) = (1100, 720);

impl<K: GeometryKernel + Default> ApplicationHandler for Shell<K> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.live.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("3dworld")
            .with_inner_size(winit::dpi::LogicalSize::new(INITIAL.0, INITIAL.1));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(e) => {
                eprintln!("no window: {e}");
                self.exit_code = 1;
                event_loop.exit();
                return;
            }
        };

        let instance = pollster::block_on(Gpu::instance());
        let surface = match instance.create_surface(window.clone()) {
            Ok(surface) => surface,
            Err(e) => {
                eprintln!("no surface: {e}");
                self.exit_code = 1;
                event_loop.exit();
                return;
            }
        };
        let gpu = match pollster::block_on(Gpu::open(&instance, Some(&surface))) {
            Ok(gpu) => gpu,
            Err(e) => {
                eprintln!("{e}");
                self.exit_code = 1;
                event_loop.exit();
                return;
            }
        };
        println!("{}", gpu.capabilities);
        if let Some(warning) = gpu.capabilities.degradation() {
            println!("{warning}");
        }

        let caps = surface.get_capabilities(&gpu.adapter);
        let format = caps.formats[0];
        let size = window.inner_size();
        // `COPY_SRC` only when a screenshot was asked for, and only if the
        // surface offers it: reading back what was *presented* is the only
        // screenshot worth taking, and paying for the usage on every run is
        // not worth it.
        let capturing = self.options.screenshot.is_some()
            && caps.usages.contains(wgpu::TextureUsages::COPY_SRC);
        let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
        if capturing {
            usage |= wgpu::TextureUsages::COPY_SRC;
        }
        let config = wgpu::SurfaceConfiguration {
            usage,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            color_space: wgpu::SurfaceColorSpace::Auto,
            view_formats: vec![],
        };
        surface.configure(&gpu.device, &config);

        let renderer = Renderer::new(&gpu.device, format);
        let depth = depth_texture(&gpu.device, config.width, config.height);
        // The depth format must match the pass egui will be recorded into,
        // and that pass is the scene's — one surface, one pass.
        let egui_renderer = egui_wgpu::Renderer::new(
            &gpu.device,
            format,
            egui_wgpu::RendererOptions {
                depth_stencil_format: Some(w3d_render::DEPTH_FORMAT),
                ..Default::default()
            },
        );
        let context = egui::Context::default();
        let egui = egui_winit::State::new(
            context,
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );

        let capture =
            capturing.then(|| capture_texture(&gpu.device, format, config.width, config.height));

        let mut editor = Editor::new(self.kernel.take().expect("one window"));
        editor.set_viewport(config.width, config.height);
        if let Some(path) = self.options.open.clone() {
            // A second kernel, because opening replaces the document around a
            // kernel rather than adding to the one already loaded.
            match editor.open(path, K::default()) {
                Ok(message) => println!("{message}"),
                Err(e) => {
                    eprintln!("{e}");
                    self.exit_code = 1;
                    event_loop.exit();
                    return;
                }
            }
        }
        if let Some(path) = self.options.import_step.clone() {
            match editor.import_step(path) {
                Ok(message) => println!("{message}"),
                Err(e) => {
                    eprintln!("{e}");
                    self.exit_code = 1;
                    event_loop.exit();
                    return;
                }
            }
        }
        for command in &self.options.startup {
            editor.run(*command);
        }
        if let Some(path) = self.options.save_as.clone() {
            match editor.save(Some(path)) {
                Ok(message) => println!("{message}"),
                Err(e) => {
                    eprintln!("{e}");
                    self.exit_code = 1;
                    event_loop.exit();
                    return;
                }
            }
        }
        if let Some(path) = self.options.export_step.clone() {
            match editor.export_step(Some(path)) {
                Ok(message) => println!("{message}"),
                Err(e) => {
                    // A failed export is an exit code, not a status line: a
                    // build with the fake kernel cannot write STEP, and a
                    // script that asked for a file has to hear about it.
                    eprintln!("{e}");
                    self.exit_code = 1;
                    event_loop.exit();
                    return;
                }
            }
        }

        self.live = Some(Live {
            window,
            gpu,
            surface,
            config,
            depth,
            renderer,
            scene: Scene::default(),
            egui,
            egui_renderer,
            editor,
            pending: None,
            modifiers: ModifiersState::empty(),
            cursor: (0.0, 0.0),
            capture,
            frames: 0,
        });
        self.live.as_ref().unwrap().window.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(live) = &mut self.live else { return };

        // egui first, and it may swallow the event. A click on a button is not
        // a click in the viewport, and getting that order wrong makes every
        // button also rotate the model.
        let response = live.egui.on_window_event(&live.window, &event);
        let consumed = response.consumed;
        if response.repaint {
            live.window.request_redraw();
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(m) => live.modifiers = m.state(),
            WindowEvent::Resized(size) => {
                live.resize(size.width, size.height);
                live.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                live.frame();
                match self.options.frames {
                    Some(limit) if live.frames >= limit => {
                        if let Some(path) = &self.options.screenshot
                            && let Err(e) = live.capture(path)
                        {
                            eprintln!("no screenshot: {e}");
                            self.exit_code = 1;
                        }
                        event_loop.exit();
                    }
                    _ => live.window.request_redraw(),
                }
            }
            _ if consumed => {}
            WindowEvent::MouseInput { state, button, .. } => {
                let Some(button) = map_button(button) else {
                    return;
                };
                let reaction = match state {
                    ElementState::Pressed => {
                        let (x, y) = live.cursor;
                        live.editor.input(Input::Down {
                            x,
                            y,
                            button,
                            additive: live.modifiers.shift_key(),
                        })
                    }
                    ElementState::Released => live.editor.input(Input::Up { button }),
                };
                live.react(reaction);
            }
            WindowEvent::CursorMoved { position, .. } => {
                live.cursor = (position.x, position.y);
                let reaction = live.editor.input(Input::Move {
                    x: position.x,
                    y: position.y,
                });
                live.react(reaction);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let amount = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(y),
                    MouseScrollDelta::PixelDelta(p) => p.y / 50.0,
                };
                let reaction = live.editor.input(Input::Scroll(-amount));
                live.react(reaction);
            }
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                if let Some(command) = map_key(&event.logical_key, live.modifiers) {
                    live.editor.run(command);
                    live.window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

fn map_button(button: MouseButton) -> Option<Button> {
    match button {
        MouseButton::Left => Some(Button::Left),
        MouseButton::Middle => Some(Button::Middle),
        _ => None,
    }
}

fn map_key(key: &Key, modifiers: ModifiersState) -> Option<Command> {
    match key {
        Key::Character(c) => match (c.as_str(), modifiers.control_key(), modifiers.shift_key()) {
            ("z", true, false) => Some(Command::Undo),
            ("z", true, true) | ("y", true, _) => Some(Command::Redo),
            ("b", false, _) => Some(Command::AddBox),
            ("s", false, _) => Some(Command::AddSphere),
            ("c", false, _) => Some(Command::AddCylinder),
            ("f", false, _) => Some(Command::ZoomToFit),
            ("a", true, _) => Some(Command::SelectAll),
            ("s", true, _) => Some(Command::Save),
            ("e", true, _) => Some(Command::ExportStep),
            ("u", false, _) => Some(Command::Boolean(BooleanOp::Union)),
            ("d", false, _) => Some(Command::Boolean(BooleanOp::Difference)),
            ("i", false, _) => Some(Command::Boolean(BooleanOp::Intersection)),
            _ => None,
        },
        Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
            Some(Command::DeleteSelected)
        }
        Key::Named(NamedKey::Escape) => Some(Command::ClearSelection),
        _ => None,
    }
}

impl<K: GeometryKernel> Live<K> {
    fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) == (self.config.width, self.config.height) {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.gpu.device, &self.config);
        self.depth = depth_texture(&self.gpu.device, width, height);
        if self.capture.is_some() {
            self.capture = Some(capture_texture(
                &self.gpu.device,
                self.config.format,
                width,
                height,
            ));
        }
        self.editor.set_viewport(width, height);
    }

    fn react(&mut self, reaction: Reaction) {
        match reaction {
            Reaction::Nothing => {}
            Reaction::Redraw => self.window.request_redraw(),
            Reaction::Pick { x, y, additive } => {
                let selected = self.editor.selection();
                let objects = self.scene.objects(self.editor.document(), &selected);
                let vp = viewport(&self.gpu, &self.config);
                let pending = self
                    .renderer
                    .pick_begin(&vp, self.editor.camera(), x, y, &objects);
                drop(objects);
                self.pending = Some((pending, additive));
                self.window.request_redraw();
            }
        }
    }

    fn collect_pick(&mut self) {
        let Some((pending, additive)) = &self.pending else {
            return;
        };
        if let Some(pick) = pending.collect(&self.gpu.device) {
            let additive = *additive;
            self.pending = None;
            self.editor.picked(pick, additive);
        }
    }

    fn frame(&mut self) {
        self.collect_pick();

        let failures = self.scene.sync(
            &self.gpu.device,
            self.gpu.capabilities.max_buffer_size,
            self.editor.document_mut(),
        );
        for (_, message) in &failures {
            eprintln!("{message}");
        }

        let raw = self.egui.take_egui_input(&self.window);
        let context = self.egui.egui_ctx().clone();
        let output = context.run_ui(raw, |ui| chrome(ui, &mut self.editor, &self.scene));
        self.egui
            .handle_platform_output(&self.window, output.platform_output);
        let jobs = context.tessellate(output.shapes, output.pixels_per_point);
        let descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: output.pixels_per_point,
        };

        // Texture deltas are *taken*, not borrowed: `TexturesDelta` panics on
        // drop if its `set` list was never consumed, and there are early
        // returns below — a surface that is not ready is a normal frame, not a
        // reason to crash. Applying them before the surface is acquired also
        // means the font atlas is uploaded even on a frame that never draws.
        let mut deltas = output.textures_delta;
        for (id, updates) in core::mem::take(&mut deltas.set) {
            // One id can carry several deltas in a frame — a font atlas grows
            // in pieces — and they must be applied in order.
            for delta in &updates {
                self.egui_renderer
                    .update_texture(&self.gpu.device, &self.gpu.queue, id, delta);
            }
        }

        use wgpu::CurrentSurfaceTexture as Got;
        let frame = match self.surface.get_current_texture() {
            Got::Success(frame) | Got::Suboptimal(frame) => frame,
            _ => {
                self.surface.configure(&self.gpu.device, &self.config);
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let depth_view = self.depth.create_view(&Default::default());

        let selected = self.editor.selection();
        let objects = self.scene.objects(self.editor.document(), &selected);
        let vp = viewport(&self.gpu, &self.config);
        self.renderer.prepare(&vp, self.editor.camera(), &objects);

        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        self.egui_renderer.update_buffers(
            &self.gpu.device,
            &self.gpu.queue,
            &mut encoder,
            &jobs,
            &descriptor,
        );
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene and chrome"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.055,
                            g: 0.059,
                            b: 0.067,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            self.renderer.draw_into(&mut pass, &objects);
            // Same pass, after the scene. One surface, one loop.
            self.egui_renderer
                .render(&mut pass.forget_lifetime(), &jobs, &descriptor);
        }
        drop(objects);
        for id in core::mem::take(&mut deltas.free) {
            self.egui_renderer.free_texture(&id);
        }
        if let Some(target) = &self.capture {
            encoder.copy_texture_to_texture(
                frame.texture.as_image_copy(),
                target.as_image_copy(),
                wgpu::Extent3d {
                    width: self.config.width,
                    height: self.config.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        self.gpu.queue.submit([encoder.finish()]);
        self.gpu.queue.present(frame);
        self.frames += 1;
    }

    /// Writes the last presented frame as a binary PPM.
    ///
    /// PPM because it needs no dependency and no encoder, and because the only
    /// consumer is a person or a script checking that something was drawn.
    fn capture(&self, path: &str) -> Result<(), String> {
        let Some(texture) = &self.capture else {
            return Err(String::from(
                "this surface cannot be read back: it does not support COPY_SRC",
            ));
        };
        let (width, height) = (self.config.width, self.config.height);
        let row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture"),
            size: u64::from(row * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.gpu.queue.submit([encoder.finish()]);

        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.gpu.device.poll(wgpu::PollType::wait_indefinitely());
        let data = slice.get_mapped_range().map_err(|e| e.to_string())?;

        let bgra = matches!(
            self.config.format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );
        let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
        for y in 0..height {
            let at = (y * row) as usize;
            for x in 0..width as usize {
                let p = &data[at + x * 4..at + x * 4 + 4];
                let rgb = if bgra {
                    [p[2], p[1], p[0]]
                } else {
                    [p[0], p[1], p[2]]
                };
                out.extend_from_slice(&rgb);
            }
        }
        drop(data);
        buffer.unmap();
        std::fs::write(path, out).map_err(|e| e.to_string())
    }
}

/// The chrome. Deliberately plain — it is a list, some buttons and a status
/// line, and the viewport is the product.
fn chrome<K: GeometryKernel>(root: &mut egui::Ui, editor: &mut Editor<K>, scene: &Scene) {
    egui::Panel::left("tree")
        .default_size(240.0)
        .show(root, |ui| {
            ui.heading("3dworld");
            ui.separator();

            ui.horizontal_wrapped(|ui| {
                if ui.button("Box").clicked() {
                    editor.run(Command::AddBox);
                }
                if ui.button("Sphere").clicked() {
                    editor.run(Command::AddSphere);
                }
                if ui.button("Cylinder").clicked() {
                    editor.run(Command::AddCylinder);
                }
            });
            // Words, not set-theory glyphs. `∪ − ∩` are not in egui's default
            // font and render as three identical empty boxes, which is worse
            // than verbose — it was only visible by looking at a screenshot.
            ui.horizontal_wrapped(|ui| {
                let two = editor.selection().len() == 2;
                if ui.add_enabled(two, egui::Button::new("Unite")).clicked() {
                    editor.run(Command::Boolean(BooleanOp::Union));
                }
                if ui.add_enabled(two, egui::Button::new("Subtract")).clicked() {
                    editor.run(Command::Boolean(BooleanOp::Difference));
                }
                if ui
                    .add_enabled(two, egui::Button::new("Intersect"))
                    .clicked()
                {
                    editor.run(Command::Boolean(BooleanOp::Intersection));
                }
            });
            ui.horizontal_wrapped(|ui| {
                if ui.button("Save").clicked() {
                    editor.run(Command::Save);
                }
                if ui.button("Export STEP").clicked() {
                    editor.run(Command::ExportStep);
                }
                if ui.button("Undo").clicked() {
                    editor.run(Command::Undo);
                }
                if ui.button("Redo").clicked() {
                    editor.run(Command::Redo);
                }
                if ui.button("Fit").clicked() {
                    editor.run(Command::ZoomToFit);
                }
            });

            ui.separator();
            let nodes: Vec<_> = editor
                .document()
                .nodes()
                .map(|(id, node)| (id, node.name.clone()))
                .collect();
            let selected = editor.selection();
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (id, name) in nodes {
                    let is = selected.contains(&id);
                    if ui.selectable_label(is, &name).clicked() {
                        let doc = editor.document_mut();
                        if is {
                            doc.deselect(id);
                        } else {
                            doc.clear_selection();
                            let _ = doc.select(id);
                        }
                    }
                }
            });
        });

    egui::Panel::bottom("status").show(root, |ui| {
        ui.horizontal(|ui| {
            ui.label(editor.status());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let bodies = editor.document().len();
                ui.label(format!(
                    "{bodies} {} · {} uploaded · {} triangles",
                    if bodies == 1 { "body" } else { "bodies" },
                    scene.uploaded(),
                    scene.triangles()
                ));
            });
        });
    });
}

/// A free function rather than a method: borrowing the fields separately is
/// what lets the renderer be borrowed mutably alongside the scene it draws.
fn viewport<'a>(gpu: &'a Gpu, config: &wgpu::SurfaceConfiguration) -> Viewport<'a> {
    Viewport {
        device: &gpu.device,
        queue: &gpu.queue,
        width: config.width,
        height: config.height,
    }
}

fn capture_texture(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("capture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn depth_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: w3d_render::DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    })
}
