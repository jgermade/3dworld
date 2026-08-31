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
    pub test_pick_face: bool,
    pub test_pick_edge: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RibbonTab {
    #[default]
    Create,
    Modify,
    View,
    File,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingKind {
    Select { additive: bool },
    Hover,
}

struct Live<K: GeometryKernel + Default> {
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
    pending: Option<(PickPending, PendingKind)>,
    modifiers: ModifiersState,
    cursor: (f64, f64),
    ribbon_tab: RibbonTab,
    /// Where `--screenshot` copies the frame *before* it is presented. A
    /// presented surface texture is destroyed, so reading one back afterwards
    /// is a validation error — the copy has to happen in the same encoder that
    /// drew it, which also makes it exactly what was shown.
    capture: Option<wgpu::Texture>,
    options: Options,
    frames: u32,
}

pub struct Shell<K: GeometryKernel + Default> {
    options: Options,
    kernel: Option<K>,
    live: Option<Live<K>>,
    pub exit_code: i32,
}

impl<K: GeometryKernel + Default> Shell<K> {
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
            execute_command(&mut editor, command.clone());
        }
        if self.options.test_pick_face
            && let Some(id) = editor.selection().first().copied()
        {
            editor.picked(
                w3d_render::Pick {
                    object: id.index(),
                    face: 1,
                },
                false,
            );
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
            ribbon_tab: RibbonTab::default(),
            capture,
            options: self.options.clone(),
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
            WindowEvent::CursorMoved { position, .. } => {
                live.cursor = (position.x, position.y);
                let reaction = live.editor.input(Input::Move {
                    x: position.x,
                    y: position.y,
                });
                live.react(reaction);
            }
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
                            ctrl: live.modifiers.control_key() || live.modifiers.super_key(),
                            alt: live.modifiers.alt_key(),
                        })
                    }
                    ElementState::Released => live.editor.input(Input::Up { button }),
                };
                live.react(reaction);
            }
            _ if consumed => {}
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
                    execute_command(&mut live.editor, command);
                    live.window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

fn execute_command<K: GeometryKernel + Default>(editor: &mut Editor<K>, command: Command) {
    match command {
        Command::Open => {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("3D World Document (*.w3d)", &["w3d"])
                .pick_file()
            {
                editor.run(Command::OpenPath(path));
            }
        }
        Command::SaveAs => {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("3D World Document (*.w3d)", &["w3d"])
                .save_file()
            {
                editor.run(Command::SaveAsPath(path));
            }
        }
        Command::Save => {
            if editor.path().is_some() {
                editor.run(Command::Save);
            } else if let Some(path) = rfd::FileDialog::new()
                .add_filter("3D World Document (*.w3d)", &["w3d"])
                .save_file()
            {
                editor.run(Command::SaveAsPath(path));
            }
        }
        Command::ImportStep => {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("STEP File (*.step, *.stp)", &["step", "stp"])
                .pick_file()
            {
                editor.run(Command::ImportStepPath(path));
            }
        }
        Command::ExportStep => {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("STEP File (*.step, *.stp)", &["step", "stp"])
                .save_file()
            {
                editor.run(Command::ExportStepPath(path));
            }
        }
        cmd => editor.run(cmd),
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
    let ctrl_or_cmd = modifiers.control_key() || modifiers.super_key();
    let shift = modifiers.shift_key();
    match key {
        Key::Character(c) => match (c.as_str().to_lowercase().as_str(), ctrl_or_cmd, shift) {
            ("z", true, false) => Some(Command::Undo),
            ("z", true, true) | ("y", true, _) => Some(Command::Redo),
            ("o", true, _) => Some(Command::Open),
            ("s", true, true) => Some(Command::SaveAs),
            ("s", true, false) => Some(Command::Save),
            ("i", true, _) => Some(Command::ImportStep),
            ("e", true, _) => Some(Command::ExportStep),
            ("b", false, _) => Some(Command::AddBox),
            ("s", false, _) => Some(Command::AddSphere),
            ("c", false, _) => Some(Command::AddCylinder),
            ("f", false, _) => Some(Command::ZoomToFit),
            ("a", true, _) => Some(Command::SelectAll),
            ("u", false, _) => Some(Command::Boolean(BooleanOp::Union)),
            ("d", false, _) => Some(Command::Boolean(BooleanOp::Difference)),
            ("i", false, _) => Some(Command::Boolean(BooleanOp::Intersection)),
            ("r", false, _) => Some(Command::Fillet),
            ("p", false, _) => Some(Command::PushPullFace(5.0)),
            ("h", false, _) => Some(Command::Shell(1.5)),
            ("1", false, _) => Some(Command::SetSelectionMode(
                crate::editor::SelectionMode::Body,
            )),
            ("2", false, _) => Some(Command::SetSelectionMode(
                crate::editor::SelectionMode::Face,
            )),
            ("3", false, _) => Some(Command::SetSelectionMode(
                crate::editor::SelectionMode::Edge,
            )),
            _ => None,
        },
        Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
            Some(Command::DeleteSelected)
        }
        Key::Named(NamedKey::Escape) => Some(Command::ClearSelection),
        _ => None,
    }
}

impl<K: GeometryKernel + Default> Live<K> {
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
                let hovered_body = self.editor.hovered_body();
                let selected_face = self.editor.selected_face();
                let hovered_face = self.editor.hovered_face();
                let objects = self.scene.objects(
                    self.editor.document(),
                    &selected,
                    hovered_body,
                    selected_face,
                    hovered_face,
                );
                let vp = viewport(&self.gpu, &self.config);
                let pending = self
                    .renderer
                    .pick_begin(&vp, self.editor.camera(), x, y, &objects);
                drop(objects);
                self.pending = Some((pending, PendingKind::Select { additive }));
                self.window.request_redraw();
            }
            Reaction::PickHover { x, y } => {
                let selected = self.editor.selection();
                let hovered_body = self.editor.hovered_body();
                let selected_face = self.editor.selected_face();
                let hovered_face = self.editor.hovered_face();
                let objects = self.scene.objects(
                    self.editor.document(),
                    &selected,
                    hovered_body,
                    selected_face,
                    hovered_face,
                );
                let vp = viewport(&self.gpu, &self.config);
                let pending = self
                    .renderer
                    .pick_begin(&vp, self.editor.camera(), x, y, &objects);
                drop(objects);
                self.pending = Some((pending, PendingKind::Hover));
                self.window.request_redraw();
            }
        }
    }

    fn collect_pick(&mut self) {
        let Some((pending, kind)) = &self.pending else {
            return;
        };
        if let Some(pick) = pending.collect(&self.gpu.device) {
            let kind = *kind;
            self.pending = None;
            match kind {
                PendingKind::Select { additive } => {
                    self.editor.picked(pick, additive);
                }
                PendingKind::Hover => {
                    let old_hover = self.editor.hovered_face();
                    let old_hover_body = self.editor.hovered_body();
                    self.editor.hover_picked(pick);
                    if self.editor.hovered_face() != old_hover
                        || self.editor.hovered_body() != old_hover_body
                    {
                        self.window.request_redraw();
                    }
                }
            }
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

        if self.options.test_pick_edge {
            self.editor.input(Input::Move {
                x: 1100.0,
                y: 500.0,
            });
        }

        let hover = self.editor.hovered_edge();
        let hover_edge = hover.hit().map(|(_, _, p0, p1)| (p0, p1, hover.is_near()));
        let sel_pts = self.editor.selected_edge().map(|(_, _, p0, p1)| (p0, p1));
        self.scene.update_edge_highlight(
            &self.gpu.device,
            self.gpu.capabilities.max_buffer_size,
            hover_edge,
            sel_pts,
        );

        let cursor_icon = if hover.is_on() {
            winit::window::CursorIcon::Crosshair
        } else if self.editor.hovered_face().is_some() || self.editor.hovered_body().is_some() {
            winit::window::CursorIcon::Pointer
        } else {
            winit::window::CursorIcon::Default
        };
        self.window.set_cursor(cursor_icon);

        let raw = self.egui.take_egui_input(&self.window);
        let context = self.egui.egui_ctx().clone();
        let output = context.run_ui(raw, |ui| {
            chrome(
                ui,
                &mut self.editor,
                &self.scene,
                &mut self.renderer,
                self.modifiers,
                &mut self.ribbon_tab,
            )
        });
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
        let hovered_body = self.editor.hovered_body();
        let selected_face = self.editor.selected_face();
        let hovered_face = self.editor.hovered_face();
        let objects = self.scene.objects(
            self.editor.document(),
            &selected,
            hovered_body,
            selected_face,
            hovered_face,
        );
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

/// The chrome. Redesigned with an Office Ribbon UI style layout: top tabbed ribbon,
/// left outliner tree, and bottom status bar.
fn chrome<K: GeometryKernel + Default>(
    root: &mut egui::Ui,
    editor: &mut Editor<K>,
    scene: &Scene,
    renderer: &mut Renderer,
    modifiers: ModifiersState,
    active_tab: &mut RibbonTab,
) {
    // 1. Top Ribbon Bar Panel
    egui::Panel::top("ribbon_panel").show(root, |ui| {
        // Tab Headers Bar
        ui.horizontal(|ui| {
            ui.heading("3dworld");
            ui.separator();
            ui.selectable_value(active_tab, RibbonTab::Create, "Create");
            ui.selectable_value(active_tab, RibbonTab::Modify, "Modify");
            ui.selectable_value(active_tab, RibbonTab::View, "View");
            ui.selectable_value(active_tab, RibbonTab::File, "File");
            ui.separator();
            ui.label("Selection:");
            let mode = editor.selection_mode();
            if ui
                .selectable_label(mode == crate::editor::SelectionMode::Body, "Body [1]")
                .clicked()
            {
                execute_command(
                    editor,
                    Command::SetSelectionMode(crate::editor::SelectionMode::Body),
                );
            }
            if ui
                .selectable_label(mode == crate::editor::SelectionMode::Face, "Face [2]")
                .clicked()
            {
                execute_command(
                    editor,
                    Command::SetSelectionMode(crate::editor::SelectionMode::Face),
                );
            }
            if ui
                .selectable_label(mode == crate::editor::SelectionMode::Edge, "Edge [3]")
                .clicked()
            {
                execute_command(
                    editor,
                    Command::SetSelectionMode(crate::editor::SelectionMode::Edge),
                );
            }
        });
        ui.separator();

        // Active Ribbon Toolbar Content
        ui.horizontal(|ui| match active_tab {
            RibbonTab::Create => {
                ui.group(|ui| {
                    ui.label("Primitives & Sketches");
                    ui.horizontal(|ui| {
                        if ui.button("Box [B]").clicked() {
                            execute_command(editor, Command::AddBox);
                        }
                        if ui.button("Sphere [S]").clicked() {
                            execute_command(editor, Command::AddSphere);
                        }
                        if ui.button("Cylinder [C]").clicked() {
                            execute_command(editor, Command::AddCylinder);
                        }
                        if ui.button("Extrude [E]").clicked() {
                            execute_command(editor, Command::AddExtrude);
                        }
                        if ui.button("Revolve").clicked() {
                            execute_command(editor, Command::AddRevolve);
                        }
                        if ui.button("Sweep").clicked() {
                            execute_command(editor, Command::AddSweep);
                        }
                        if ui.button("Loft").clicked() {
                            execute_command(editor, Command::AddLoft);
                        }
                        if ui.button("Sketch 2D").clicked() {
                            execute_command(editor, Command::EnterSketchMode);
                        }
                        if ui.button("New Group").clicked() {
                            execute_command(editor, Command::AddGroup);
                        }
                    });
                });
                ui.group(|ui| {
                    ui.label("Quick Actions");
                    ui.horizontal(|ui| {
                        if ui.button("Fit View [F]").clicked() {
                            execute_command(editor, Command::ZoomToFit);
                        }
                        if ui.button("Select All [A]").clicked() {
                            execute_command(editor, Command::SelectAll);
                        }
                    });
                });
            }
            RibbonTab::Modify => {
                let two = editor.selection().len() == 2;
                let one_or_more = !editor.selection().is_empty();
                ui.group(|ui| {
                    ui.label("Booleans");
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(two, egui::Button::new("Unite [U]"))
                            .clicked()
                        {
                            execute_command(editor, Command::Boolean(BooleanOp::Union));
                        }
                        if ui
                            .add_enabled(two, egui::Button::new("Subtract [D]"))
                            .clicked()
                        {
                            execute_command(editor, Command::Boolean(BooleanOp::Difference));
                        }
                        if ui
                            .add_enabled(two, egui::Button::new("Intersect [I]"))
                            .clicked()
                        {
                            execute_command(editor, Command::Boolean(BooleanOp::Intersection));
                        }
                    });
                });
                ui.group(|ui| {
                    ui.label("Features");
                    ui.horizontal(|ui| {
                        let face_selected = editor.selected_face().is_some();
                        if ui
                            .add_enabled(one_or_more, egui::Button::new("Fillet [R]"))
                            .clicked()
                        {
                            execute_command(editor, Command::Fillet);
                        }
                        if ui
                            .add_enabled(one_or_more, egui::Button::new("Chamfer [C]"))
                            .clicked()
                        {
                            execute_command(editor, Command::Chamfer);
                        }
                        if ui
                            .add_enabled(face_selected, egui::Button::new("Push/Pull [P]"))
                            .clicked()
                        {
                            execute_command(editor, Command::PushPullFace(5.0));
                        }
                        if ui
                            .add_enabled(one_or_more, egui::Button::new("Shell [H]"))
                            .clicked()
                        {
                            execute_command(editor, Command::Shell(1.5));
                        }
                    });
                });
                ui.group(|ui| {
                    ui.label("History");
                    ui.horizontal(|ui| {
                        if ui.button("Undo").clicked() {
                            execute_command(editor, Command::Undo);
                        }
                        if ui.button("Redo").clicked() {
                            execute_command(editor, Command::Redo);
                        }
                    });
                });
            }
            RibbonTab::View => {
                let two_or_more = editor.selection().len() >= 2;
                ui.group(|ui| {
                    ui.label("Selection Mode");
                    ui.horizontal(|ui| {
                        let mode = editor.selection_mode();
                        if ui
                            .selectable_label(mode == crate::editor::SelectionMode::Body, "Body")
                            .clicked()
                        {
                            execute_command(
                                editor,
                                Command::SetSelectionMode(crate::editor::SelectionMode::Body),
                            );
                        }
                        if ui
                            .selectable_label(mode == crate::editor::SelectionMode::Face, "Face")
                            .clicked()
                        {
                            execute_command(
                                editor,
                                Command::SetSelectionMode(crate::editor::SelectionMode::Face),
                            );
                        }
                        if ui
                            .selectable_label(mode == crate::editor::SelectionMode::Edge, "Edge")
                            .clicked()
                        {
                            execute_command(
                                editor,
                                Command::SetSelectionMode(crate::editor::SelectionMode::Edge),
                            );
                        }
                    });
                });
                ui.group(|ui| {
                    ui.label("Camera Alignment");
                    ui.horizontal(|ui| {
                        if ui.button("Top").clicked() {
                            execute_command(
                                editor,
                                Command::SetView(crate::editor::ViewDirection::Top),
                            );
                        }
                        if ui.button("Front").clicked() {
                            execute_command(
                                editor,
                                Command::SetView(crate::editor::ViewDirection::Front),
                            );
                        }
                        if ui.button("Right").clicked() {
                            execute_command(
                                editor,
                                Command::SetView(crate::editor::ViewDirection::Right),
                            );
                        }
                        if ui.button("Isometric").clicked() {
                            execute_command(
                                editor,
                                Command::SetView(crate::editor::ViewDirection::Iso),
                            );
                        }
                    });
                });
                ui.group(|ui| {
                    ui.label("Viewport & Inspection");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut renderer.show_grid, "Ground Grid");
                        if ui.button("Zoom to Fit [F]").clicked() {
                            execute_command(editor, Command::ZoomToFit);
                        }
                        if ui
                            .add_enabled(two_or_more, egui::Button::new("Measure Distance [M]"))
                            .clicked()
                        {
                            execute_command(editor, Command::MeasureDistance);
                        }
                    });
                });
            }
            RibbonTab::File => {
                ui.group(|ui| {
                    ui.label("Document");
                    ui.horizontal(|ui| {
                        if ui.button("Open...").clicked() {
                            execute_command(editor, Command::Open);
                        }
                        if ui.button("Save").clicked() {
                            execute_command(editor, Command::Save);
                        }
                        if ui.button("Save As...").clicked() {
                            execute_command(editor, Command::SaveAs);
                        }
                    });
                });
                ui.group(|ui| {
                    ui.label("CAD Interchange");
                    ui.horizontal(|ui| {
                        if ui.button("Import STEP...").clicked() {
                            execute_command(editor, Command::ImportStep);
                        }
                        if ui.button("Export STEP...").clicked() {
                            execute_command(editor, Command::ExportStep);
                        }
                    });
                });
            }
        });
    });

    // 2. Left Outliner Tree Panel
    egui::Panel::left("tree")
        .default_size(220.0)
        .show(root, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Outliner");
                if ui.button("+ Group").clicked() {
                    execute_command(editor, Command::AddGroup);
                }
            });
            ui.separator();

            let sketch_active = editor.sketch_state().active;
            if sketch_active {
                ui.group(|ui| {
                    ui.label("✏️ 2D Sketch Mode Active");
                    let pt_count = editor.sketch_state().points.len();
                    ui.label(format!("Points: {pt_count}"));
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(pt_count >= 3, egui::Button::new("Finish [Extrude]"))
                            .clicked()
                        {
                            execute_command(editor, Command::FinishSketch);
                        }
                        if ui.button("Cancel").clicked() {
                            execute_command(editor, Command::CancelSketch);
                        }
                    });
                });
                ui.separator();
            }

            let nodes: Vec<_> = editor
                .document()
                .nodes()
                .map(|(id, node)| (id, node.name.clone(), node.parent, node.children.len()))
                .collect();
            let selected = editor.selection();

            egui::ScrollArea::vertical().show(ui, |ui| {
                for (id, name, parent, child_count) in nodes {
                    let is = selected.contains(&id);
                    let prefix = if parent.is_some() {
                        "  ↳ "
                    } else if child_count > 0 {
                        "📁 "
                    } else {
                        "📦 "
                    };
                    let label = format!("{prefix}{name}");
                    if ui.selectable_label(is, &label).clicked() {
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

            ui.separator();
            ui.collapsing("Shortcuts & Modifiers", |ui| {
                ui.label("Left Drag: Orbit");
                ui.label("Shift + Left Drag / Middle Drag: Pan");
                ui.label("Ctrl/Cmd + Left Drag / Scroll: Zoom");
                ui.label("Shift + Click: Toggle Selection");
                ui.label("Ctrl + O: Open");
                ui.label("Ctrl + S / Shift+S: Save / Save As");
                ui.label("Ctrl + I / E: Import / Export STEP");
                ui.label("B / S / C: Primitives");
                ui.label("U / D / I / R: Boolean / Fillet");
            });
        });

    // 3. Bottom Status Bar Panel
    egui::Panel::bottom("status").show(root, |ui| {
        ui.horizontal(|ui| {
            let shift = modifiers.shift_key();
            let ctrl = modifiers.control_key() || modifiers.super_key();
            let alt = modifiers.alt_key();

            let mod_str = if shift || ctrl || alt {
                format!(
                    " · [{}{}{}]",
                    if shift {
                        "SHIFT: Pan/Multi-Select "
                    } else {
                        ""
                    },
                    if ctrl { "CTRL: Zoom " } else { "" },
                    if alt { "ALT " } else { "" }
                )
            } else {
                String::new()
            };

            let mode_str = match editor.selection_mode() {
                crate::editor::SelectionMode::Body => "[Mode: Body (1)]",
                crate::editor::SelectionMode::Face => "[Mode: Face (2)]",
                crate::editor::SelectionMode::Edge => "[Mode: Edge (3)]",
            };

            ui.label(format!("{mode_str} · {}{mod_str}", editor.status()));
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

    // 4. Floating View Cube Overlay (Top Right Viewport)
    egui::Window::new("View Cube")
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 100.0))
        .resizable(false)
        .collapsible(false)
        .title_bar(false)
        .show(root, |ui| {
            ui.horizontal(|ui| {
                ui.label("CUBE:");
                if ui.button("TOP").clicked() {
                    execute_command(editor, Command::SetView(crate::editor::ViewDirection::Top));
                }
                if ui.button("FRONT").clicked() {
                    execute_command(
                        editor,
                        Command::SetView(crate::editor::ViewDirection::Front),
                    );
                }
                if ui.button("RIGHT").clicked() {
                    execute_command(
                        editor,
                        Command::SetView(crate::editor::ViewDirection::Right),
                    );
                }
                if ui.button("ISO").clicked() {
                    execute_command(editor, Command::SetView(crate::editor::ViewDirection::Iso));
                }
            });
        });

    // 4.5. Real-time 2D Sketch Overlay Painter
    let sketch = editor.sketch_state();
    if sketch.active && !sketch.points.is_empty() {
        let (vw, vh) = editor.viewport();
        if vw > 0 && vh > 0 {
            let aspect = f64::from(vw) / f64::from(vh);
            let vp = editor.camera().view_projection(aspect);
            let col0 = vp[0];
            let col1 = vp[1];
            let col2 = vp[2];
            let col3 = vp[3];

            let project = |u: f64, v: f64| -> Option<egui::Pos2> {
                let p = sketch.plane.origin + sketch.plane.x_axis * u + sketch.plane.y_axis * v;
                let (px, py, pz) = (p.x, p.y, p.z);
                let cx = f64::from(col0[0]) * px
                    + f64::from(col1[0]) * py
                    + f64::from(col2[0]) * pz
                    + f64::from(col3[0]);
                let cy = f64::from(col0[1]) * px
                    + f64::from(col1[1]) * py
                    + f64::from(col2[1]) * pz
                    + f64::from(col3[1]);
                let cw = f64::from(col0[3]) * px
                    + f64::from(col1[3]) * py
                    + f64::from(col2[3]) * pz
                    + f64::from(col3[3]);
                if cw <= 1.0e-5 {
                    None
                } else {
                    let sx = ((cx / cw) * 0.5 + 0.5) * f64::from(vw);
                    let sy = ((1.0 - cy / cw) * 0.5) * f64::from(vh);
                    Some(egui::pos2(sx as f32, sy as f32))
                }
            };

            let painter = root.painter();
            let screen_points: Vec<_> = sketch
                .points
                .iter()
                .filter_map(|&(u, v)| project(u, v))
                .collect();

            for i in 0..screen_points.len().saturating_sub(1) {
                painter.line_segment(
                    [screen_points[i], screen_points[i + 1]],
                    egui::Stroke::new(2.5, egui::Color32::from_rgb(0, 220, 255)),
                );
            }
            if screen_points.len() >= 3 {
                painter.line_segment(
                    [*screen_points.last().unwrap(), screen_points[0]],
                    egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 200, 0)),
                );
            }

            for (idx, &pt) in screen_points.iter().enumerate() {
                let color = if idx == 0 {
                    egui::Color32::from_rgb(255, 200, 0)
                } else {
                    egui::Color32::from_rgb(0, 240, 255)
                };
                painter.circle_filled(pt, 5.0, color);
            }
        }
    }

    // 5. Floating 3D Translate Gizmo Overlay (Bottom Right Viewport)
    if !editor.selection().is_empty() {
        egui::Window::new("3D Gizmo Translate")
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -40.0))
            .resizable(false)
            .collapsible(false)
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.label("X:");
                    if ui.button("-5").clicked() {
                        execute_command(
                            editor,
                            Command::TranslateSelection(w3d_core::kernel::Vec3::new(
                                -5.0, 0.0, 0.0,
                            )),
                        );
                    }
                    if ui.button("+5").clicked() {
                        execute_command(
                            editor,
                            Command::TranslateSelection(w3d_core::kernel::Vec3::new(5.0, 0.0, 0.0)),
                        );
                    }

                    ui.label("Y:");
                    if ui.button("-5").clicked() {
                        execute_command(
                            editor,
                            Command::TranslateSelection(w3d_core::kernel::Vec3::new(
                                0.0, -5.0, 0.0,
                            )),
                        );
                    }
                    if ui.button("+5").clicked() {
                        execute_command(
                            editor,
                            Command::TranslateSelection(w3d_core::kernel::Vec3::new(0.0, 5.0, 0.0)),
                        );
                    }

                    ui.label("Z:");
                    if ui.button("-5").clicked() {
                        execute_command(
                            editor,
                            Command::TranslateSelection(w3d_core::kernel::Vec3::new(
                                0.0, 0.0, -5.0,
                            )),
                        );
                    }
                    if ui.button("+5").clicked() {
                        execute_command(
                            editor,
                            Command::TranslateSelection(w3d_core::kernel::Vec3::new(0.0, 0.0, 5.0)),
                        );
                    }
                });
            });
    }

    // 6. Floating Sub-object Inspector Overlay (Left Viewport)
    if let Some((node_id, face_id)) = editor.selected_face()
        && let Some(metrics) = editor.face_metrics(node_id, face_id)
    {
        egui::Window::new(format!("Sub-object Inspector [Face #{face_id}]"))
            .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(240.0, -40.0))
            .resizable(false)
            .collapsible(true)
            .show(root, |ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("Face ID:");
                        ui.label(format!("#{face_id}"));
                        ui.separator();
                        ui.label("Mode:");
                        ui.colored_label(
                            egui::Color32::from_rgb(50, 180, 255),
                            format!("{:?}", editor.selection_mode()),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Surface Area:");
                        ui.label(format!("{:.3} mm²", metrics.area));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Face Normal:");
                        ui.label(format!(
                            "[{:.3}, {:.3}, {:.3}]",
                            metrics.normal.x, metrics.normal.y, metrics.normal.z
                        ));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Centroid:");
                        ui.label(format!(
                            "[{:.2}, {:.2}, {:.2}]",
                            metrics.centroid.x, metrics.centroid.y, metrics.centroid.z
                        ));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Triangles:");
                        ui.label(format!("{}", metrics.triangle_count));
                    });
                    ui.separator();
                    ui.label("↔ Direct Face Extrude Normal Gizmo:");
                    ui.horizontal(|ui| {
                        if ui.button("−10 mm").clicked() {
                            execute_command(editor, Command::PushPullFace(-10.0));
                        }
                        if ui.button("−5 mm").clicked() {
                            execute_command(editor, Command::PushPullFace(-5.0));
                        }
                        if ui.button("+5 mm").clicked() {
                            execute_command(editor, Command::PushPullFace(5.0));
                        }
                        if ui.button("+10 mm").clicked() {
                            execute_command(editor, Command::PushPullFace(10.0));
                        }
                        if ui.button("+25 mm").clicked() {
                            execute_command(editor, Command::PushPullFace(25.0));
                        }
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Deselect Sub-object").clicked() {
                            editor.clear_selected_face();
                        }
                        if ui.button("Fillet [R]").clicked() {
                            execute_command(editor, Command::Fillet);
                        }
                        if ui.button("Chamfer [C]").clicked() {
                            execute_command(editor, Command::Chamfer);
                        }
                        if ui.button("Shell [H]").clicked() {
                            execute_command(editor, Command::Shell(1.5));
                        }
                    });
                });
            });
    }
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
