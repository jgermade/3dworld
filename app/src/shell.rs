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
use crate::gizmo;
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

/// What the manipulators are doing, as the window's event handling needs to
/// know it.
///
/// `dragging` takes the pointer away from the camera; `open` takes only the
/// `Escape` key, because a readout that is merely open must not stop the
/// viewport orbiting — the version this replaced could not tell the two apart
/// and left the mouse stuck down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GizmoStatus {
    pub dragging: bool,
    pub open: bool,
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
    gizmo: GizmoStatus,
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
        if let Some(warning) = gpu.capabilities.software_rendering() {
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
            gizmo: GizmoStatus::default(),
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
        if response.repaint {
            live.window.request_redraw();
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
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
            WindowEvent::ModifiersChanged(m) => {
                live.modifiers = m.state();
                let reaction = live.editor.input(Input::ModifiersChanged {
                    additive: live.modifiers.shift_key(),
                    ctrl: live.modifiers.control_key() || live.modifiers.super_key(),
                    alt: live.modifiers.alt_key(),
                });
                live.react(reaction);
                live.window.request_redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                live.cursor = (position.x, position.y);
                let _ = live.editor.input(Input::ModifiersChanged {
                    additive: live.modifiers.shift_key(),
                    ctrl: live.modifiers.control_key() || live.modifiers.super_key(),
                    alt: live.modifiers.alt_key(),
                });
                // If a gizmo is actively dragging, cancel any editor drag and let gizmo handle movement
                if live.gizmo.dragging {
                    live.editor.cancel_drag();
                    return;
                }
                // If egui widget consumed this cursor movement (e.g. over a ribbon button or outliner),
                // do not orbit or hover-test 3D geometry.
                if response.consumed {
                    return;
                }
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
                        if response.consumed || live.gizmo.dragging {
                            live.editor.cancel_drag();
                            Reaction::Nothing
                        } else {
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
                    }
                    ElementState::Released => {
                        // CRITICAL: Mouse release MUST ALWAYS clear editor drag,
                        // preventing clicks from getting hooked/stuck.
                        live.editor.input(Input::Up { button })
                    }
                };
                live.react(reaction);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if response.consumed {
                    return;
                }
                let amount = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(y),
                    MouseScrollDelta::PixelDelta(p) => p.y / 50.0,
                };
                let reaction = live.editor.input(Input::Scroll(-amount));
                live.react(reaction);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // If egui text edit wants keyboard input, don't execute 3dworld single-key shortcuts.
                if live.egui.egui_ctx().egui_wants_keyboard_input() {
                    return;
                }
                // An open manipulator owns `Escape`: it cancels the handle, and
                // the selection stays where it was.
                if live.gizmo.open && event.logical_key == Key::Named(NamedKey::Escape) {
                    live.window.request_redraw();
                    return;
                }
                if event.state.is_pressed()
                    && let Some(command) = map_key(&event.logical_key, live.modifiers)
                {
                    execute_command(&mut live.editor, command);
                }
                let reaction = live.editor.input(Input::ModifiersChanged {
                    additive: live.modifiers.shift_key(),
                    ctrl: live.modifiers.control_key() || live.modifiers.super_key(),
                    alt: live.modifiers.alt_key(),
                });
                live.react(reaction);
                live.window.request_redraw();
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
        Key::Named(NamedKey::Escape) => Some(Command::Escape),
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
                &mut self.gizmo,
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
    gizmo: &mut GizmoStatus,
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

            // Depth first from the roots, with the depth in hand — an imported
            // assembly is three levels deep and the arena's own order says
            // nothing about which node is inside which.
            let rows = editor.outline_rows();
            let selected = editor.selection();

            egui::ScrollArea::vertical().show(ui, |ui| {
                for row in rows {
                    let id = row.id;
                    let is = selected.contains(&id);
                    let icon = if row.group || row.children > 0 {
                        "📁"
                    } else {
                        "📦"
                    };
                    let label = format!("{}{icon} {}", "    ".repeat(row.depth), row.name);
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

    // The scene is drawn over the *whole* window, with the panels on top of it,
    // so a 3D point's place on screen is measured against the window. Named
    // outright rather than taken from the root `Ui`, whose rect happens to be
    // the same today and is not the thing being relied on. In points, not
    // pixels: that part the HiDPI fix got right.
    let screen = root.ctx().viewport_rect();
    let sw = f64::from(screen.width());
    let sh = f64::from(screen.height());
    let (vw, vh) = editor.viewport();
    let vp = if vw > 0 && vh > 0 {
        let aspect = f64::from(vw) / f64::from(vh);
        let proj = editor.camera().projection(aspect);
        let view = editor.camera().view();
        Some(proj.mul(&view))
    } else {
        None
    };
    let project_3d = move |p: w3d_core::kernel::Vec3| -> Option<egui::Pos2> {
        let vp = vp?;
        let (px, py, pz) = (p.x, p.y, p.z);
        let clip_x = vp.0[0][0] * px + vp.0[0][1] * py + vp.0[0][2] * pz + vp.0[0][3];
        let clip_y = vp.0[1][0] * px + vp.0[1][1] * py + vp.0[1][2] * pz + vp.0[1][3];
        let clip_w = vp.0[3][0] * px + vp.0[3][1] * py + vp.0[3][2] * pz + vp.0[3][3];
        if clip_w <= 1.0e-5 {
            None
        } else {
            let sx = screen.min.x as f64 + (clip_x / clip_w + 1.0) * 0.5 * sw;
            let sy = screen.min.y as f64 + (1.0 - clip_y / clip_w) * 0.5 * sh;
            Some(egui::pos2(sx as f32, sy as f32))
        }
    };

    let draw_arrow_3d = |painter: &egui::Painter,
                         from: egui::Pos2,
                         to: egui::Pos2,
                         color: egui::Color32,
                         width: f32,
                         highlight: bool| {
        let dir = to - from;
        let len = dir.length();
        if len < 4.0 {
            return;
        }
        let u = dir / len;
        let norm = egui::vec2(-u.y, u.x);
        let head_len = 16.0f32.min(len * 0.45);
        let head_w = head_len * 0.6;
        let shaft_end = to - u * (head_len * 0.8);

        // Dark background halo for high contrast in 3D scene
        painter.line_segment(
            [from, shaft_end],
            egui::Stroke::new(width + 3.0, egui::Color32::from_black_alpha(180)),
        );
        // Colored Arrow Shaft
        painter.line_segment(
            [from, shaft_end],
            egui::Stroke::new(if highlight { width + 2.0 } else { width }, color),
        );

        // Arrow Head triangle
        let tip = to;
        let p1 = to - u * head_len + norm * head_w;
        let p2 = to - u * head_len - norm * head_w;
        painter.add(egui::Shape::convex_polygon(
            vec![tip, p1, p2],
            color,
            egui::Stroke::new(
                1.5,
                if highlight {
                    egui::Color32::WHITE
                } else {
                    color
                },
            ),
        ));

        // Interactive grab handle node at tip
        painter.circle_filled(to, if highlight { 6.0 } else { 4.5 }, color);
        painter.circle_stroke(
            to,
            if highlight { 8.5 } else { 6.0 },
            egui::Stroke::new(1.5, egui::Color32::WHITE),
        );
    };

    // 4.5. Real-time 2D Sketch Overlay Painter
    let sketch = editor.sketch_state();
    if sketch.active && !sketch.points.is_empty() {
        let painter = root.painter();
        let screen_points: Vec<_> = sketch
            .points
            .iter()
            .filter_map(|&(u, v)| {
                let p = sketch.plane.origin + sketch.plane.x_axis * u + sketch.plane.y_axis * v;
                project_3d(p)
            })
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

        for (i, &pt) in screen_points.iter().enumerate() {
            let color = if i == 0 {
                egui::Color32::from_rgb(0, 255, 120)
            } else {
                egui::Color32::WHITE
            };
            painter.circle_filled(pt, 5.0, color);
        }
    }

    // ---- 5. The manipulators --------------------------------------------
    //
    // The arithmetic is in [`crate::gizmo`], where a test can reach it. What is
    // left here is where the handles are, what they look like, and the three
    // moments that matter: grabbing one, letting it go, and typing a number
    // into the box it leaves behind.
    //
    // Two things this does not do, both of which it used to. It does not claim
    // the bounding box of a diagonal arrow — a press is on a handle only if it
    // is on the *shaft*, so the viewport around a selected part still orbits
    // and still picks. And it does not touch geometry while the mouse is
    // moving: a drag is a number until it is let go, which is one kernel call
    // and one undo step per drag rather than per frame.
    let mods = gizmo::Mods {
        snap: modifiers.shift_key() || root.input(|i| i.modifiers.shift),
        fine: modifiers.control_key()
            || modifiers.super_key()
            || root.input(|i| i.modifiers.ctrl || i.modifiers.mac_cmd),
    };
    // Handles belong to the viewport, not to the chrome: what the panels cover
    // is theirs, and an arrow that runs under the ribbon is clipped there
    // rather than drawn over a row of buttons.
    let stage = root.available_rect_before_wrap();
    let pointer = root
        .input(|i| i.pointer.latest_pos())
        .filter(|at| stage.contains(*at));
    let held = root.input(|i| i.pointer.primary_down());
    let pressed = root.input(|i| i.pointer.primary_pressed());

    let session_slot = egui::Id::new("w3d_gizmo_session");
    let mut session: Option<gizmo::Session> = root.data_mut(|d| d.get_temp(session_slot));

    // 5.1. A drag in flight. It reads the cursor, draws where it would land,
    // and applies nothing until the button comes up.
    if let Some(state) = session.as_mut().filter(|s| s.dragging) {
        if held {
            if let Some(at) = pointer {
                state.track(at, mods);
            }
            root.output_mut(|o| o.cursor_icon = egui::CursorIcon::Grabbing);

            let painter = root.painter().with_clip_rect(stage);
            if !state.handle.is_angular() {
                // The line the drag is constrained to, across the whole
                // viewport, in the handle's own colour.
                let far = 1.0e4;
                let a = project_3d(state.anchor - state.direction * far)
                    .unwrap_or(state.axis.origin - state.axis.dir * 4000.0);
                let b = project_3d(state.anchor + state.direction * far)
                    .unwrap_or(state.axis.origin + state.axis.dir * 4000.0);
                painter.line_segment(
                    [a, b],
                    egui::Stroke::new(1.0, gizmo_colour(state.handle).linear_multiply(0.35)),
                );
            }
        } else {
            state.dragging = false;
            state.focus_wanted = true;
            apply_pending(editor, state);
        }
    }

    // 5.2. The preview, for a drag that has not been applied yet.
    if let Some(state) = session.as_ref().filter(|s| s.dragging) {
        let colour = gizmo_colour(state.handle);
        match state.handle {
            gizmo::Handle::PushPull { node, face, .. } => {
                if let Some(outline) = editor.face_outline(node, face) {
                    let offset = state.direction * state.value;
                    let here: Vec<_> = outline.iter().filter_map(|&p| project_3d(p)).collect();
                    let there: Vec<_> = outline
                        .iter()
                        .filter_map(|&p| project_3d(p + offset))
                        .collect();
                    let painter = root.painter().with_clip_rect(stage);
                    if here.len() == outline.len() && there.len() == outline.len() {
                        for i in 0..there.len() {
                            let j = (i + 1) % there.len();
                            painter
                                .line_segment([there[i], there[j]], egui::Stroke::new(2.0, colour));
                            painter.line_segment(
                                [here[i], there[i]],
                                egui::Stroke::new(1.0, colour.linear_multiply(0.5)),
                            );
                        }
                    }
                }
            }
            gizmo::Handle::Translate { axis } => {
                ghost_box(
                    root,
                    stage,
                    editor,
                    &project_3d,
                    &w3d_core::kernel::Mat4::from_translation(axis * state.value),
                    colour,
                );
            }
            gizmo::Handle::Rotate { axis } => {
                let centre = state.anchor;
                let m = w3d_core::kernel::Mat4::from_translation(centre)
                    .mul(&w3d_core::kernel::Mat4::from_axis_angle(
                        axis,
                        state.value.to_radians(),
                        1.0e-12,
                    ))
                    .mul(&w3d_core::kernel::Mat4::from_translation(-centre));
                ghost_box(root, stage, editor, &project_3d, &m, colour);
            }
            gizmo::Handle::Fillet | gizmo::Handle::Chamfer => {}
        }
    }

    // 5.3. The handles for whatever is selected. One set, never three at once:
    // an edge is a blend, a face is a pull, a body is a move and a turn.
    let mut handles: Vec<Candidate> = Vec::new();
    let eye = editor.camera().eye();

    if let Some((node_id, _, p0, p1)) = editor.selected_edge() {
        let mid = w3d_core::kernel::Vec3::new(
            f64::from(p0[0] + p1[0]) * 0.5,
            f64::from(p0[1] + p1[1]) * 0.5,
            f64::from(p0[2] + p1[2]) * 0.5,
        );
        // The tessellation's box, not the kernel's: `Document::bounds` costs a
        // fresh tessellation on `truck`, and this runs on every frame.
        let body_box = editor.document_mut().mesh_bounds(node_id).ok();
        let centre = body_box.map_or(w3d_core::kernel::Vec3::ZERO, |b| b.center());
        let out = (mid - centre)
            .normalize(1.0e-9)
            .unwrap_or(w3d_core::kernel::Vec3::Z);
        let edge = (w3d_core::kernel::Vec3::new(
            f64::from(p1[0] - p0[0]),
            f64::from(p1[1] - p0[1]),
            f64::from(p1[2] - p0[2]),
        ))
        .normalize(1.0e-9)
        .unwrap_or(w3d_core::kernel::Vec3::X);
        let across = edge
            .cross(out)
            .normalize(1.0e-9)
            .unwrap_or(w3d_core::kernel::Vec3::Y);
        let span = screen_span(editor, mid, &project_3d, 90.0);

        handles.push(Candidate::arrow(
            gizmo::Handle::Fillet,
            mid,
            out,
            span,
            &project_3d,
        ));
        handles.push(Candidate::arrow(
            gizmo::Handle::Chamfer,
            mid,
            (out + across).normalize(1.0e-9).unwrap_or(across),
            span,
            &project_3d,
        ));
    } else if let Some((node_id, face_id)) = editor.selected_face() {
        if let Some(metrics) = editor.face_metrics(node_id, face_id) {
            let span = metrics.area.sqrt().max(5.0) * 0.8;
            handles.push(Candidate::arrow(
                gizmo::Handle::PushPull {
                    node: node_id,
                    face: face_id,
                    outward: true,
                },
                metrics.centroid,
                metrics.normal,
                span,
                &project_3d,
            ));
            handles.push(Candidate::arrow(
                gizmo::Handle::PushPull {
                    node: node_id,
                    face: face_id,
                    outward: false,
                },
                metrics.centroid,
                -metrics.normal,
                span * 0.7,
                &project_3d,
            ));
        }
    } else if let Some(bounds) = selection_bounds(editor) {
        let centre = bounds.center();
        // A manipulator is the same size on screen whatever the part is: sized
        // off the solid, it ran off the top of the window on a part the view
        // was framed to, and shrank to nothing on an assembly. The arrows stand
        // clear of the rings on purpose — a handle you have to aim between two
        // others is a handle you grab the wrong one of.
        let span = screen_span(editor, centre, &project_3d, 120.0);
        let ring_span = screen_span(editor, centre, &project_3d, 78.0);
        for axis in [
            w3d_core::kernel::Vec3::X,
            w3d_core::kernel::Vec3::Y,
            w3d_core::kernel::Vec3::Z,
        ] {
            handles.push(Candidate::arrow(
                gizmo::Handle::Translate { axis },
                centre,
                axis,
                span,
                &project_3d,
            ));
            handles.push(Candidate::ring(
                gizmo::Handle::Rotate { axis },
                centre,
                axis,
                ring_span,
                eye,
                &project_3d,
            ));
        }
    }

    // 5.4. Hovering, and grabbing. A handle is grabbed by its shaft or its
    // ring, never by the box around it.
    let busy = session.as_ref().is_some_and(|s| s.dragging);
    let hovered = pointer.filter(|_| !busy).and_then(|at| {
        handles
            .iter()
            .position(|c| c.hit(at, gizmo::GRAB_TOLERANCE))
    });

    if let Some(index) = hovered {
        root.output_mut(|o| o.cursor_icon = egui::CursorIcon::Grab);
        if pressed && let Some(at) = pointer {
            let candidate = &handles[index];
            if let Some(mut started) = candidate.session(at, eye) {
                // The press reached the editor too — egui only learns a handle
                // is under the cursor a frame later. Cancelling here is what
                // keeps a grab from also orbiting the camera, and what keeps
                // the release from being read as a click on whatever is behind.
                editor.cancel_drag();
                started.track(at, mods);
                session = Some(started);
            }
        }
    }

    // While the cursor is on a handle, one small widget under it tells egui the
    // pointer is spoken for, so the next press does not reach the viewport. It
    // is the size of the cursor, not the size of the arrow's bounding box.
    if (hovered.is_some() || busy)
        && let Some(at) = pointer
    {
        let _ = root.interact(
            egui::Rect::from_center_size(at, egui::vec2(26.0, 26.0)),
            egui::Id::new("w3d_gizmo_grab"),
            egui::Sense::click_and_drag(),
        );
    }

    // 5.5. Drawing them.
    let active = session.as_ref().map(|s| s.handle);
    let painter = root.painter().with_clip_rect(stage);
    for (index, candidate) in handles.iter().enumerate() {
        let lit = hovered == Some(index) || active == Some(candidate.handle);
        candidate.draw(&painter, &draw_arrow_3d, lit);
    }

    // 5.6. The readout: what the drag measures, and a box to type it into.
    let mut dismiss = false;
    if let Some(state) = session.as_mut() {
        let colour = gizmo_colour(state.handle);
        let area = egui::Area::new(egui::Id::new("w3d_gizmo_readout"))
            .fixed_pos(state.readout_at)
            .order(egui::Order::Foreground)
            .show(root.ctx(), |ui| {
                egui::Frame::popup(ui.style())
                    .fill(egui::Color32::from_rgba_premultiplied(18, 22, 28, 240))
                    .stroke(egui::Stroke::new(1.5, colour))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(state.handle.label())
                                    .color(colour)
                                    .strong(),
                            );
                            if mods.snap {
                                let step = if state.handle.is_angular() {
                                    format!("{:.0}°", gizmo::SNAP_ANGLE)
                                } else if state.handle.is_blend() {
                                    format!("{:.0} mm", gizmo::SNAP_DISTANCE_BLEND)
                                } else {
                                    format!("{:.0} mm", gizmo::SNAP_DISTANCE)
                                };
                                ui.label(
                                    egui::RichText::new(format!("paso {step}"))
                                        .color(egui::Color32::from_rgb(0, 255, 160))
                                        .size(10.0),
                                );
                            }
                            if mods.fine {
                                ui.label(
                                    egui::RichText::new("fino ×0.1")
                                        .color(egui::Color32::from_rgb(255, 215, 0))
                                        .size(10.0),
                                );
                            }

                            if state.dragging {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{:+.2} {}",
                                        state.value,
                                        state.handle.unit()
                                    ))
                                    .monospace()
                                    .strong(),
                                );
                                Outcome::Nothing
                            } else {
                                let box_ = ui.add(
                                    egui::TextEdit::singleline(&mut state.text).desired_width(56.0),
                                );
                                if state.focus_wanted {
                                    box_.request_focus();
                                    state.focus_wanted = false;
                                }
                                ui.label(state.handle.unit());
                                let entered = ui.input(|i| i.key_pressed(egui::Key::Enter));
                                if ui.button("✔").clicked() || entered {
                                    match state.text.trim().replace(',', ".").parse::<f64>() {
                                        Ok(typed) => {
                                            state.value = typed;
                                            Outcome::Apply
                                        }
                                        Err(_) => Outcome::Nothing,
                                    }
                                } else if ui.button("✕").clicked()
                                    || ui.input(|i| i.key_pressed(egui::Key::Escape))
                                {
                                    Outcome::Cancel
                                } else {
                                    Outcome::Nothing
                                }
                            }
                        })
                        .inner
                    })
                    .inner
            });

        match area.inner {
            Outcome::Apply => {
                apply_pending(editor, state);
                dismiss = true;
            }
            // Cancelling undoes what this session applied, and nothing else.
            // The version this replaced undid one step unconditionally, so
            // cancelling a handle that had done nothing ate the user's previous
            // operation instead.
            Outcome::Cancel => {
                if state.touched_the_document() {
                    execute_command(editor, Command::Undo);
                }
                dismiss = true;
            }
            Outcome::Nothing => {}
        }

        // A click anywhere else puts the readout away and belongs to the
        // viewport, not to the gizmo.
        if !state.dragging && pressed && pointer.is_some_and(|at| !area.response.rect.contains(at))
        {
            dismiss = true;
        }
    }

    if dismiss || editor.selection().is_empty() {
        session = None;
    }

    gizmo.dragging = session.as_ref().is_some_and(|s| s.dragging);
    gizmo.open = session.is_some();
    root.data_mut(|d| d.insert_temp(session_slot, session));
}

/// What the readout asked for this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Outcome {
    #[default]
    Nothing,
    Apply,
    Cancel,
}

/// The CAD convention, and the one the viewport's axes already use: X red,
/// Y green, Z blue, a pull in cyan, a blend in gold.
fn gizmo_colour(handle: gizmo::Handle) -> egui::Color32 {
    match handle {
        gizmo::Handle::Translate { axis } | gizmo::Handle::Rotate { axis } => {
            if axis.x != 0.0 {
                egui::Color32::from_rgb(235, 80, 80)
            } else if axis.y != 0.0 {
                egui::Color32::from_rgb(90, 220, 110)
            } else {
                egui::Color32::from_rgb(90, 150, 255)
            }
        }
        gizmo::Handle::PushPull { outward: true, .. } => egui::Color32::from_rgb(0, 230, 255),
        gizmo::Handle::PushPull { outward: false, .. } => egui::Color32::from_rgb(255, 140, 30),
        gizmo::Handle::Fillet => egui::Color32::from_rgb(80, 240, 140),
        gizmo::Handle::Chamfer => egui::Color32::from_rgb(255, 200, 40),
    }
}

/// One handle, placed on screen for this frame.
struct Candidate {
    handle: gizmo::Handle,
    anchor: w3d_core::kernel::Vec3,
    direction: w3d_core::kernel::Vec3,
    world_len: f64,
    /// An arrow's two ends, or empty for a ring.
    ends: Option<(egui::Pos2, egui::Pos2)>,
    /// A ring's projected outline, or empty for an arrow.
    ring: Vec<egui::Pos2>,
    /// A ring's centre on screen, which is what an angle is measured about.
    centre: Option<egui::Pos2>,
}

impl Candidate {
    fn arrow(
        handle: gizmo::Handle,
        anchor: w3d_core::kernel::Vec3,
        direction: w3d_core::kernel::Vec3,
        world_len: f64,
        project: &impl Fn(w3d_core::kernel::Vec3) -> Option<egui::Pos2>,
    ) -> Self {
        let ends = project(anchor).zip(project(anchor + direction * world_len));
        Self {
            handle,
            anchor,
            direction,
            world_len,
            ends,
            ring: Vec::new(),
            centre: None,
        }
    }

    fn ring(
        handle: gizmo::Handle,
        centre: w3d_core::kernel::Vec3,
        axis: w3d_core::kernel::Vec3,
        radius: f64,
        eye: w3d_core::kernel::Vec3,
        project: &impl Fn(w3d_core::kernel::Vec3) -> Option<egui::Pos2>,
    ) -> Self {
        let seed = if axis.x.abs() < 0.9 {
            w3d_core::kernel::Vec3::X
        } else {
            w3d_core::kernel::Vec3::Y
        };
        let u = seed
            .cross(axis)
            .normalize(1.0e-12)
            .unwrap_or(w3d_core::kernel::Vec3::X);
        let v = axis.cross(u);
        let mut ring = Vec::with_capacity(65);
        for step in 0..=64 {
            let t = f64::from(step) / 64.0 * std::f64::consts::TAU;
            let p = centre + u * (radius * t.cos()) + v * (radius * t.sin());
            // The far half of the ring is dropped rather than drawn through the
            // solid: a handle you cannot see is a handle you grab by accident.
            if (eye - p).dot(p - centre) < 0.0 {
                ring.push(egui::Pos2::new(f32::NAN, f32::NAN));
                continue;
            }
            match project(p) {
                Some(at) => ring.push(at),
                None => ring.push(egui::Pos2::new(f32::NAN, f32::NAN)),
            }
        }
        Self {
            handle,
            anchor: centre,
            direction: axis,
            world_len: radius,
            ends: None,
            ring,
            centre: project(centre),
        }
    }

    fn hit(&self, at: egui::Pos2, tolerance: f32) -> bool {
        if let Some((from, to)) = self.ends {
            return gizmo::hit_segment(at, from, to, tolerance);
        }
        self.ring
            .windows(2)
            .filter(|w| w.iter().all(|p| p.x.is_finite() && p.y.is_finite()))
            .any(|w| gizmo::hit_segment(at, w[0], w[1], tolerance))
    }

    /// The drag this handle starts, or `None` when it is edge-on and there is
    /// no direction to drag it in.
    fn session(&self, at: egui::Pos2, eye: w3d_core::kernel::Vec3) -> Option<gizmo::Session> {
        if let Some((from, to)) = self.ends {
            let axis = gizmo::ScreenAxis::new(from, to, self.world_len)?;
            return Some(gizmo::Session::new(
                self.handle,
                self.anchor,
                self.direction,
                axis,
                at,
            ));
        }
        let mut started = gizmo::Session::new(
            self.handle,
            self.anchor,
            self.direction,
            gizmo::ScreenAxis {
                origin: self.centre?,
                dir: egui::vec2(1.0, 0.0),
                points_per_unit: 1.0,
            },
            at,
        );
        // Turning the same way the cursor goes means knowing which way the axis
        // points: seen from behind, a ring turns the other way.
        started.facing = if self.direction.dot(eye - self.anchor) >= 0.0 {
            1.0
        } else {
            -1.0
        };
        Some(started)
    }

    fn draw(
        &self,
        painter: &egui::Painter,
        arrow: &impl Fn(&egui::Painter, egui::Pos2, egui::Pos2, egui::Color32, f32, bool),
        lit: bool,
    ) {
        let colour = if lit {
            egui::Color32::from_rgb(255, 215, 0)
        } else {
            gizmo_colour(self.handle)
        };
        if let Some((from, to)) = self.ends {
            arrow(painter, from, to, colour, if lit { 4.5 } else { 3.0 }, lit);
            return;
        }
        for pair in self.ring.windows(2) {
            if pair.iter().any(|p| !p.x.is_finite() || !p.y.is_finite()) {
                continue;
            }
            painter.line_segment(
                [pair[0], pair[1]],
                egui::Stroke::new(if lit { 3.5 } else { 2.0 }, colour),
            );
        }
    }
}

/// How long a handle at `at` has to be, in document units, to come out
/// `points` long on screen.
///
/// Measured across the screen rather than along any one axis, so all three
/// arrows of a triad are the same length in the world and foreshorten the way
/// the geometry does.
fn screen_span<K: GeometryKernel + Default>(
    editor: &Editor<K>,
    at: w3d_core::kernel::Vec3,
    project: &impl Fn(w3d_core::kernel::Vec3) -> Option<egui::Pos2>,
    points: f32,
) -> f64 {
    let camera = editor.camera();
    let forward = (camera.target - camera.eye())
        .normalize(1.0e-12)
        .unwrap_or(w3d_core::kernel::Vec3::Y);
    let across = forward
        .cross(w3d_render::camera::UP)
        .normalize(1.0e-12)
        .unwrap_or(w3d_core::kernel::Vec3::X);
    // A unit of world, measured where the handle is: one probe, and the answer
    // is in points per unit.
    let Some((here, there)) = project(at).zip(project(at + across)) else {
        return 10.0;
    };
    let per_unit = (there - here).length();
    if per_unit < 1.0e-3 {
        return 10.0;
    }
    f64::from(points / per_unit)
}

/// The box around everything selected, which is what the move and turn handles
/// stand on.
fn selection_bounds<K: GeometryKernel + Default>(
    editor: &mut Editor<K>,
) -> Option<w3d_core::kernel::Aabb> {
    let mut bounds = w3d_core::kernel::Aabb::EMPTY;
    for id in editor.selection() {
        // `mesh_bounds` and not `bounds`: this is asked every frame, and on
        // `truck` the kernel's answer is a tessellation — 2.4 seconds of one,
        // on the solid the demo builds.
        if let Ok(b) = editor.document_mut().mesh_bounds(id) {
            bounds = bounds.union(&b);
        }
    }
    (!bounds.is_empty()).then_some(bounds)
}

/// The selection's box, as it would be after `m` — the preview a drag draws
/// instead of rebuilding the solid on every frame.
fn ghost_box<K: GeometryKernel + Default>(
    root: &mut egui::Ui,
    stage: egui::Rect,
    editor: &mut Editor<K>,
    project: &impl Fn(w3d_core::kernel::Vec3) -> Option<egui::Pos2>,
    m: &w3d_core::kernel::Mat4,
    colour: egui::Color32,
) {
    let Some(bounds) = selection_bounds(editor) else {
        return;
    };
    let (lo, hi) = (bounds.min, bounds.max);
    let corners = [
        w3d_core::kernel::Vec3::new(lo.x, lo.y, lo.z),
        w3d_core::kernel::Vec3::new(hi.x, lo.y, lo.z),
        w3d_core::kernel::Vec3::new(hi.x, hi.y, lo.z),
        w3d_core::kernel::Vec3::new(lo.x, hi.y, lo.z),
        w3d_core::kernel::Vec3::new(lo.x, lo.y, hi.z),
        w3d_core::kernel::Vec3::new(hi.x, lo.y, hi.z),
        w3d_core::kernel::Vec3::new(hi.x, hi.y, hi.z),
        w3d_core::kernel::Vec3::new(lo.x, hi.y, hi.z),
    ];
    let screen: Vec<_> = corners
        .iter()
        .map(|&c| project(m.transform_point(c)))
        .collect();
    let painter = root.painter().with_clip_rect(stage);
    for (a, b) in [
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 0),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ] {
        if let (Some(p), Some(q)) = (screen[a], screen[b]) {
            painter.line_segment([p, q], egui::Stroke::new(1.5, colour.linear_multiply(0.8)));
        }
    }
}

/// Hands the document what a session owes it, as one operation.
fn apply_pending<K: GeometryKernel + Default>(editor: &mut Editor<K>, state: &mut gizmo::Session) {
    match state.pending() {
        gizmo::Pending::Nothing => {}
        gizmo::Pending::Add(delta) => {
            run_handle(editor, state.handle, delta);
            state.settled();
        }
        gizmo::Pending::Replace { value, undo_first } => {
            if undo_first {
                execute_command(editor, Command::Undo);
            }
            run_handle(editor, state.handle, value);
            state.settled();
        }
    }
}

/// One handle, one command. Everything a manipulator does is something the
/// keyboard and the ribbon can already do, which is what keeps a drag undoable
/// and a status line honest.
fn run_handle<K: GeometryKernel + Default>(
    editor: &mut Editor<K>,
    handle: gizmo::Handle,
    amount: f64,
) {
    match handle {
        gizmo::Handle::Translate { axis } => {
            execute_command(editor, Command::TranslateSelection(axis * amount));
        }
        gizmo::Handle::Rotate { axis } => {
            execute_command(
                editor,
                Command::RotateSelection {
                    axis,
                    angle_deg: amount,
                },
            );
        }
        gizmo::Handle::PushPull { node, outward, .. } => {
            // The face *id* is deliberately not checked: a pull rebuilds the
            // solid and the editor re-selects the same face under a new number.
            // What has to still hold is that the pull is on the part the handle
            // came from.
            if editor.selected_face().map(|(n, _)| n) != Some(node) {
                editor.set_status("that face is no longer selected, so nothing was pulled");
                return;
            }
            let distance = if outward { amount } else { -amount };
            execute_command(editor, Command::PushPullFace(distance));
        }
        gizmo::Handle::Fillet | gizmo::Handle::Chamfer => {
            if amount <= 0.0 {
                editor.set_status("a blend needs a positive size — drag the other way");
                return;
            }
            // The edge the handle was drawn on, not every edge of the solid.
            // Until the kernel trait had `fillet_edges` these issued
            // `FilletRadius`, which blends the whole part — the handle stood on
            // one edge and the label said so, which was honest and was still
            // not what a user dragging it wants.
            if matches!(handle, gizmo::Handle::Fillet) {
                execute_command(editor, Command::FilletSelectedEdge(amount));
            } else {
                execute_command(editor, Command::ChamferSelectedEdge(amount));
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gizmo::{Handle, Mods, ScreenAxis, Session};
    use egui::pos2;
    use w3d_core::kernel::Vec3;
    use w3d_kernel_fake::FakeKernel;

    fn editor() -> Editor<FakeKernel> {
        let mut editor = Editor::new(FakeKernel::default());
        editor.run(Command::AddBox);
        editor
    }

    /// Ten screen points to the world unit, along +x.
    fn axis() -> ScreenAxis {
        ScreenAxis::new(pos2(100.0, 100.0), pos2(200.0, 100.0), 10.0).unwrap()
    }

    fn session(handle: Handle) -> Session {
        Session::new(handle, Vec3::ZERO, Vec3::X, axis(), pos2(100.0, 100.0))
    }

    #[test]
    fn a_move_handle_becomes_one_translation_of_what_is_selected() {
        let mut editor = editor();
        let id = editor.selection()[0];
        let before = editor.document().bounds(id).unwrap();

        let mut state = session(Handle::Translate { axis: Vec3::X });
        state.track(pos2(180.0, 100.0), Mods::default());
        apply_pending(&mut editor, &mut state);

        let after = editor.document().bounds(id).unwrap();
        assert!(
            (after.min.x - (before.min.x + 8.0)).abs() < 1.0e-9,
            "{after:?}"
        );
        assert!((after.min.y - before.min.y).abs() < 1.0e-9);
        assert_eq!(editor.document_mut().undo(), Some("Translate"));
        assert_eq!(editor.document().bounds(id).unwrap(), before);
    }

    #[test]
    fn a_second_reading_applies_the_difference_and_not_the_whole_of_it() {
        let mut editor = editor();
        let id = editor.selection()[0];
        let before = editor.document().bounds(id).unwrap();

        let mut state = session(Handle::Translate { axis: Vec3::X });
        state.track(pos2(130.0, 100.0), Mods::default());
        apply_pending(&mut editor, &mut state);
        // The number typed into the readout after the drag.
        state.value = 5.0;
        apply_pending(&mut editor, &mut state);

        let after = editor.document().bounds(id).unwrap();
        assert!(
            (after.min.x - (before.min.x + 5.0)).abs() < 1.0e-9,
            "the second reading was added to the first instead of replacing it: {after:?}"
        );
    }

    #[test]
    fn a_turn_handle_becomes_one_rotation() {
        let mut editor = editor();
        let mut state = session(Handle::Rotate { axis: Vec3::Z });
        state.value = 90.0;
        apply_pending(&mut editor, &mut state);

        assert!(editor.status().contains("rotated"), "{}", editor.status());
        assert_eq!(editor.document_mut().undo(), Some("Rotate"));
    }

    #[test]
    fn a_blend_that_the_backend_declines_says_so_and_changes_nothing() {
        let mut editor = editor();
        let id = editor.selection()[0];
        let before = editor.document().bounds(id).unwrap();

        let mut state = session(Handle::Fillet);
        state.value = 1.5;
        apply_pending(&mut editor, &mut state);

        // The fake kernel fillets; what matters here is that whichever answer
        // the backend gives reaches the status line rather than being dropped.
        assert!(!editor.status().is_empty());
        assert_eq!(editor.document().bounds(id).unwrap(), before);
    }

    #[test]
    fn a_blend_dragged_backwards_is_refused_rather_than_clamped_to_a_crumb() {
        let mut editor = editor();
        let mut state = session(Handle::Fillet);
        state.track(pos2(60.0, 100.0), Mods::default());
        apply_pending(&mut editor, &mut state);

        assert!(state.value < 0.0, "{}", state.value);
        assert!(
            editor.status().contains("positive"),
            "a backwards drag should be refused, not rounded up: {}",
            editor.status()
        );
    }

    #[test]
    fn a_session_that_applied_nothing_leaves_the_history_where_it_was() {
        let mut editor = editor();
        editor.document_mut().clear_history();

        let mut state = session(Handle::Translate { axis: Vec3::X });
        state.track(pos2(100.0, 140.0), Mods::default()); // across the axis: no travel along it
        apply_pending(&mut editor, &mut state);

        assert!(!state.touched_the_document());
        assert_eq!(editor.document_mut().undo(), None);
    }
}
