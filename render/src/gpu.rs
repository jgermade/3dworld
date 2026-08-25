//! Getting a device, and being honest about what it can do.
//!
//! The whole reason this module is separate from the renderer is that the
//! answers here are *reported*, not assumed. `STACK.md` says the WebGL2
//! fallback has no compute shaders, and the cost of finding that out at
//! runtime — after building a pipeline on it — is a feature that degrades
//! obscurely instead of visibly.

use core::fmt;

/// Which API actually answered, and what it is capable of.
///
/// Constructed only by [`Gpu::open`]; every field is read from the adapter
/// rather than inferred from `cfg!`. A build for the web can end up on WebGPU
/// or on WebGL2 and the difference is not knowable until it has happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// `Vulkan`, `Metal`, `Dx12`, `BrowserWebGpu` or `Gl`, in wgpu's words.
    pub backend: wgpu::Backend,
    /// What the adapter calls itself. For a log line, never for branching.
    pub adapter: String,
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
    pub max_buffer_size: u64,
    /// Largest square viewport this device will render into.
    pub max_texture_dimension_2d: u32,
}

impl Capabilities {
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
            "{} on {} — compute {}, vertex storage buffers {}, max buffer {} MiB",
            self.backend,
            self.adapter,
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

        let capabilities = Capabilities {
            backend: info.backend,
            adapter: info.name.clone(),
            compute: downlevel
                .flags
                .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS),
            vertex_storage_buffers: downlevel
                .flags
                .contains(wgpu::DownlevelFlags::VERTEX_STORAGE),
            max_buffer_size: limits.max_buffer_size,
            max_texture_dimension_2d: limits.max_texture_dimension_2d,
        };

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

        Ok(Self {
            device,
            queue,
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
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::from_env().unwrap_or_else(wgpu::Backends::all);
        wgpu::util::new_instance_with_webgpu_detection(desc).await
    }
}
