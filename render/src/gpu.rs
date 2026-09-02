//! Getting a device, and being honest about what it can do.
//!
//! The whole reason this module is separate from the renderer is that the
//! answers here are *reported*, not assumed. `STACK.md` says the WebGL2
//! fallback has no compute shaders, and the cost of finding that out at
//! runtime — after building a pipeline on it — is a feature that degrades
//! obscurely instead of visibly.

use core::fmt;

/// Whether a real GPU is behind the adapter, as far as it will say.
///
/// Read from `AdapterInfo::device_type`, which is a *report* and not always an
/// answer, so `Unknown` is a third state rather than a polite word for
/// hardware. Two cases produce it and neither is rare:
///
///   - **WebGPU in a browser.** The API does not tell a page what is behind
///     its adapter, so wgpu has nothing to map and says `Other`. This is the
///     case `web/`'s loader exists to cover: the last word there belongs to
///     whether a frame came out, not to what was reported.
///   - **A paravirtualised device.** `VirtualGpu` may have silicon behind it
///     or a rasteriser on the host, and the adapter cannot tell which.
///
/// `Software` is therefore a claim only where the driver made one: wgpu's GL
/// backend recognising llvmpipe or SwiftShader by name, or a Vulkan ICD saying
/// `PHYSICAL_DEVICE_TYPE_CPU`, which is what lavapipe does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Acceleration {
    /// A discrete or integrated GPU.
    Hardware,
    /// A CPU rasteriser, said so by the driver.
    Software,
    /// Nobody would say. Not evidence either way — see above.
    Unknown,
}

impl fmt::Display for Acceleration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Hardware => "hardware",
            Self::Software => "software (CPU rasteriser)",
            Self::Unknown => "unreported",
        })
    }
}

/// Which API actually answered, and what it is capable of.
///
/// Constructed only by [`Gpu::open`]; every field is read from the adapter or
/// from the device it granted, rather than inferred from `cfg!`. A build for
/// the web can end up on WebGPU or on WebGL2 and the difference is not
/// knowable until it has happened.
///
/// Which of the two answered matters, and the fields say which: what the
/// *adapter* advertises is not what the *device* was given, because
/// [`Gpu::open`] asks for less than the adapter offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// `Vulkan`, `Metal`, `Dx12`, `BrowserWebGpu` or `Gl`, in wgpu's words.
    pub backend: wgpu::Backend,
    /// What the adapter calls itself. For a log line, never for branching.
    pub adapter: String,
    /// What kind of device the driver says it is. Branch through
    /// [`Capabilities::acceleration`] rather than on this directly: the
    /// mapping from wgpu's five variants onto three answers is where the
    /// honesty about `Other` lives.
    pub device_type: wgpu::DeviceType,
    /// Whether compute shaders exist at all.
    ///
    /// False on WebGL2, and that is the fallback the loader may land on. Every
    /// GPU-side thing this project has planned — BVH build, culling, silhouette
    /// extraction — is compute, so this flag decides whether they run on the
    /// GPU or degrade to the CPU. It does not decide whether *rendering*
    /// works.
    pub compute: bool,
    /// Whether the adapter can index a storage buffer from a vertex shader.
    /// The instance-transform path wants it; WebGL2 does not have it either.
    pub vertex_storage_buffers: bool,
    /// Largest single buffer, in bytes. A tessellated import is checked
    /// against this before it is uploaded, because the failure mode otherwise
    /// is a device loss rather than an error.
    ///
    /// **Read from the device, not from the adapter**, and the difference is
    /// the whole point: [`Gpu::open`] asks for `downlevel_webgl2_defaults`, so
    /// a device on an adapter offering gigabytes is still granted the
    /// downlevel maximum and rejects anything above it. Reporting the
    /// adapter's number here would make the check pass a mesh the device then
    /// refuses — the exact failure the check exists to prevent.
    pub max_buffer_size: u64,
    /// Largest square viewport this device will render into. From the device
    /// too, though `using_resolution` means it matches the adapter's.
    pub max_texture_dimension_2d: u32,
}

impl Capabilities {
    /// Whether a GPU is doing the rasterising, as far as anyone will say.
    pub fn acceleration(&self) -> Acceleration {
        match self.device_type {
            wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu => {
                Acceleration::Hardware
            }
            wgpu::DeviceType::Cpu => Acceleration::Software,
            wgpu::DeviceType::VirtualGpu | wgpu::DeviceType::Other => Acceleration::Unknown,
        }
    }

    /// The line to show when the driver has admitted there is no GPU here.
    ///
    /// `None` covers both hardware and `Unknown`, because a warning shown on
    /// every browser that declines to answer is a warning nobody reads. What
    /// is not knowable is left to the loader's frame check, not guessed at
    /// here.
    pub fn software_rendering(&self) -> Option<String> {
        (self.acceleration() == Acceleration::Software).then(|| {
            format!(
                "{} is a CPU rasteriser, not a GPU; every frame is drawn on \
                 the processor and no timing here says anything about \
                 hardware.",
                self.adapter
            )
        })
    }

    /// The one-line summary a loader should show a user when it degrades.
    ///
    /// Named after the rule in `STACK.md`: a fallback that is not announced is
    /// indistinguishable from a bug report about performance six months later.
    pub fn degradation(&self) -> Option<String> {
        (!self.compute).then(|| {
            format!(
                "{} has no compute shaders; culling, BVH build and silhouette \
                 extraction run on the CPU.",
                self.backend
            )
        })
    }
}

impl fmt::Display for Capabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} on {} ({}) — compute {}, vertex storage buffers {}, max buffer {} MiB",
            self.backend,
            self.adapter,
            self.acceleration(),
            if self.compute { "yes" } else { "NO" },
            if self.vertex_storage_buffers {
                "yes"
            } else {
                "NO"
            },
            self.max_buffer_size / (1024 * 1024),
        )
    }
}

#[derive(Debug)]
pub enum GpuError {
    /// No adapter at all. On the web this is "neither WebGPU nor WebGL2",
    /// which is a message for a user, not a panic.
    NoAdapter,
    /// An adapter exists and refused the limits asked of it.
    Device(String),
}

impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAdapter => {
                f.write_str("no graphics adapter: neither WebGPU nor WebGL2 is available here")
            }
            Self::Device(e) => write!(f, "the adapter refused a device: {e}"),
        }
    }
}

impl core::error::Error for GpuError {}

/// A device, its queue, and what it turned out to be able to do.
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// Kept because a surface has to be asked what formats *it* supports, and
    /// only the adapter can answer. Nothing else should reach for it.
    pub adapter: wgpu::Adapter,
    pub capabilities: Capabilities,
}

impl Gpu {
    /// Opens whatever is there, preferring the fastest adapter, and reports
    /// what it got.
    ///
    /// `compatible_surface` is `None` for offscreen work — which is what the
    /// tests use, and what a headless render for a thumbnail would use.
    ///
    /// The limits asked for are `downlevel_webgl2_defaults`, deliberately: the
    /// weakest target in the matrix is the one every other target must also
    /// satisfy, so a pipeline that works in a test works on WebGL2. Anything
    /// wanting more asks for it explicitly, and then has to handle not getting
    /// it.
    pub async fn open(
        instance: &wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Self, GpuError> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface,
                ..Default::default()
            })
            .await
            .map_err(|_| GpuError::NoAdapter)?;

        let info = adapter.get_info();
        let downlevel = adapter.get_downlevel_capabilities();
        let limits = adapter.limits();

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("w3d"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(limits),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
                ..Default::default()
            })
            .await
            .map_err(|e| GpuError::Device(e.to_string()))?;

        // After `request_device`, not before: the limits that matter to a
        // caller are the ones this device was *granted*, which are the ones
        // asked for above and not the ones the adapter advertised.
        let granted = device.limits();
        let capabilities = Capabilities {
            backend: info.backend,
            adapter: info.name.clone(),
            device_type: info.device_type,
            compute: downlevel
                .flags
                .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS),
            vertex_storage_buffers: downlevel
                .flags
                .contains(wgpu::DownlevelFlags::VERTEX_STORAGE),
            max_buffer_size: granted.max_buffer_size,
            max_texture_dimension_2d: granted.max_texture_dimension_2d,
        };

        Ok(Self {
            device,
            queue,
            adapter,
            capabilities,
        })
    }

    /// An instance over every backend this build was compiled with, having
    /// first asked the browser whether WebGPU really works.
    ///
    /// `new_instance_with_webgpu_detection` is doing something specific and
    /// worth naming: `navigator.gpu` existing is *not* the same as WebGPU
    /// working, and a build that trusts the object drops to no adapter at all
    /// rather than to WebGL2. This is the graphics half of the two-variant
    /// dispatch `web/` owes for threads.
    ///
    /// Async because that probe is async on the web. On native it resolves
    /// immediately.
    pub async fn instance() -> wgpu::Instance {
        Self::instance_for(wgpu::Backends::from_env().unwrap_or_else(wgpu::Backends::all)).await
    }

    /// The same, restricted to `backends`.
    ///
    /// This exists because the detection above is necessary and **not
    /// sufficient**. A browser can pass every check it makes — `navigator.gpu`
    /// present, an adapter returned, a device created, generous limits
    /// reported — and still rasterise nothing at all. That is not
    /// hypothetical: it is what headless Chromium does in a container with no
    /// working GPU, and the failure is a black canvas with no error anywhere.
    ///
    /// So the last word belongs to whoever can look at the result. `web/`'s
    /// loader draws a frame, checks the canvas is not one flat colour, and
    /// comes back through here with `Backends::GL` when it is.
    pub async fn instance_for(backends: wgpu::Backends) -> wgpu::Instance {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = backends;
        wgpu::util::new_instance_with_webgpu_detection(desc).await
    }
}
