//! One logical GPU device: buffers, transfers, dispatch and accounting.
//!
//! Synchronization model: every node of a context submits to the **same queue**, and wgpu
//! executes submissions on a queue in order, so a consumer's dispatch observes its producer's
//! writes without extra fences. A host read waits for the queue (`map_async` + `poll(Wait)`).
//! Buffers of different contexts are never shared: crossing contexts is an explicit transfer.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mvf_engine::DeviceBuffer;
use serde::Serialize;
use wgpu::util::DeviceExt;

use crate::kernels::{Kernel, WORKGROUP};

/// How to open a context.
#[derive(Debug, Clone)]
pub struct GpuConfig {
    /// Most bytes of device buffers this context may hold at once (software budget; wgpu exposes no
    /// portable VRAM quota).
    pub memory_budget: u64,
    /// Accept a software (CPU) adapter. Off by default: a CPU rasterizer is not "real GPU
    /// execution".
    pub allow_software: bool,
}

impl Default for GpuConfig {
    fn default() -> Self {
        Self {
            memory_budget: 512 << 20,
            allow_software: false,
        }
    }
}

/// Transfer and dispatch counters.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuMetrics {
    /// Host→device transfers.
    pub uploads: u64,
    /// Bytes uploaded.
    pub upload_bytes: u64,
    /// Device→host transfers.
    pub downloads: u64,
    /// Bytes downloaded.
    pub download_bytes: u64,
    /// Device buffers handed from one node to another on this device (no copy).
    pub device_handoffs: u64,
    /// Kernel dispatches.
    pub dispatches: u64,
    /// Bytes of device buffers currently allocated.
    pub allocated: u64,
    /// Peak allocated bytes.
    pub peak_allocated: u64,
}

#[derive(Default)]
struct Counters {
    uploads: AtomicU64,
    upload_bytes: AtomicU64,
    downloads: AtomicU64,
    download_bytes: AtomicU64,
    device_handoffs: AtomicU64,
    dispatches: AtomicU64,
    allocated: AtomicU64,
    peak: AtomicU64,
}

pub(crate) struct Ctx {
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: wgpu::AdapterInfo,
    id: String,
    budget: u64,
    max_buffer: u64,
    max_groups: u32,
    pipelines: Mutex<HashMap<Kernel, Arc<wgpu::ComputePipeline>>>,
    counters: Counters,
}

/// A logical GPU device. Cheap to clone.
#[derive(Clone)]
pub struct GpuContext {
    pub(crate) ctx: Arc<Ctx>,
}

impl std::fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContext")
            .field("id", &self.ctx.id)
            .finish()
    }
}

static CONTEXTS: AtomicU64 = AtomicU64::new(0);

impl GpuContext {
    /// Opens the default high-performance adapter. Errors when no (hardware) adapter exists.
    pub fn new(config: &GpuConfig) -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            ..Default::default()
        }))
        .map_err(|e| format!("no GPU adapter: {e}"))?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu && !config.allow_software {
            return Err(format!(
                "adapter `{}` is a software rasterizer, not a GPU",
                info.name
            ));
        }
        let limits = adapter.limits();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("mvf-compute"),
            required_features: wgpu::Features::empty(),
            required_limits: limits.clone(),
            ..Default::default()
        }))
        .map_err(|e| format!("request_device: {e}"))?;
        let id = format!(
            "wgpu:{:?}:{}:{:04x}:{:04x}#{}",
            info.backend,
            info.name,
            info.vendor,
            info.device,
            CONTEXTS.fetch_add(1, Ordering::Relaxed)
        );
        Ok(Self {
            ctx: Arc::new(Ctx {
                device,
                queue,
                info,
                id,
                budget: config.memory_budget,
                max_buffer: limits
                    .max_storage_buffer_binding_size
                    .min(limits.max_buffer_size),
                max_groups: limits.max_compute_workgroups_per_dimension,
                pipelines: Mutex::new(HashMap::new()),
                counters: Counters::default(),
            }),
        })
    }

    /// Adapter information (name, backend, driver, device type).
    pub fn info(&self) -> &wgpu::AdapterInfo {
        &self.ctx.info
    }

    /// Stable id of this logical device.
    pub fn id(&self) -> &str {
        &self.ctx.id
    }

    /// Counters so far.
    pub fn metrics(&self) -> GpuMetrics {
        let c = &self.ctx.counters;
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        GpuMetrics {
            uploads: l(&c.uploads),
            upload_bytes: l(&c.upload_bytes),
            downloads: l(&c.downloads),
            download_bytes: l(&c.download_bytes),
            device_handoffs: l(&c.device_handoffs),
            dispatches: l(&c.dispatches),
            allocated: l(&c.allocated),
            peak_allocated: l(&c.peak),
        }
    }

    pub(crate) fn note_handoff(&self) {
        self.ctx
            .counters
            .device_handoffs
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Allocates a zeroed device buffer of `len` logical bytes (padded to whole words).
    pub fn alloc(&self, len: usize) -> Result<GpuBuffer, String> {
        let padded = (len.max(4) as u64).div_ceil(4) * 4;
        if padded > self.ctx.max_buffer {
            return Err(format!(
                "buffer of {padded} bytes exceeds the device limit of {}",
                self.ctx.max_buffer
            ));
        }
        let c = &self.ctx.counters;
        let now = c.allocated.fetch_add(padded, Ordering::SeqCst) + padded;
        if now > self.ctx.budget {
            c.allocated.fetch_sub(padded, Ordering::SeqCst);
            return Err(format!(
                "device memory budget exhausted: {now} > {} bytes",
                self.ctx.budget
            ));
        }
        c.peak.fetch_max(now, Ordering::Relaxed);
        let buffer = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mvf-payload"),
            size: padded,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(GpuBuffer {
            inner: Arc::new(BufferInner {
                ctx: Arc::clone(&self.ctx),
                buffer,
                len,
                padded,
            }),
        })
    }

    /// Explicit host→device transfer (counted).
    pub fn upload(&self, bytes: &[u8]) -> Result<GpuBuffer, String> {
        let buf = self.alloc(bytes.len())?;
        let mut padded = bytes.to_vec();
        padded.resize(buf.inner.padded as usize, 0);
        self.ctx.queue.write_buffer(&buf.inner.buffer, 0, &padded);
        let c = &self.ctx.counters;
        c.uploads.fetch_add(1, Ordering::Relaxed);
        c.upload_bytes
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(buf)
    }

    fn pipeline(&self, kernel: Kernel) -> Arc<wgpu::ComputePipeline> {
        let mut cache = self.ctx.pipelines.lock().unwrap_or_else(|p| p.into_inner());
        Arc::clone(cache.entry(kernel).or_insert_with(|| {
            let module = self
                .ctx
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("mvf-kernel"),
                    source: wgpu::ShaderSource::Wgsl(kernel.source().into()),
                });
            Arc::new(
                self.ctx
                    .device
                    .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                        label: Some("mvf-kernel"),
                        layout: None,
                        module: &module,
                        entry_point: Some("main"),
                        compilation_options: Default::default(),
                        cache: None,
                    }),
            )
        }))
    }

    /// Runs `kernel` with `input` → `output` over `elements` logical elements. With `groups`
    /// the dispatch size is fixed (reductions); otherwise one thread per element (2-D if needed).
    pub fn dispatch(
        &self,
        kernel: Kernel,
        input: &GpuBuffer,
        output: &GpuBuffer,
        elements: u32,
        groups: Option<u32>,
    ) -> Result<(), String> {
        let needed = groups.unwrap_or_else(|| elements.div_ceil(WORKGROUP).max(1));
        let (x, y) = if needed <= self.ctx.max_groups {
            (needed, 1)
        } else {
            let y = needed.div_ceil(self.ctx.max_groups);
            (needed.div_ceil(y), y)
        };
        let params: [u32; 4] = [elements, x * WORKGROUP, 0, 0];
        let mut raw = Vec::with_capacity(16);
        for p in params {
            raw.extend_from_slice(&p.to_le_bytes());
        }
        let uniform = self
            .ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mvf-params"),
                contents: &raw,
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let pipeline = self.pipeline(kernel);
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mvf-bind"),
                layout: &pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: input.inner.buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: output.inner.buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: uniform.as_entire_binding(),
                    },
                ],
            });
        let mut encoder = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mvf-dispatch"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mvf-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(x, y, 1);
        }
        self.ctx.queue.submit([encoder.finish()]);
        self.ctx.counters.dispatches.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Waits until all submitted work has finished.
    pub fn wait(&self) -> Result<(), String> {
        self.ctx
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .map(|_| ())
            .map_err(|e| format!("device poll: {e}"))
    }
}

pub(crate) struct BufferInner {
    ctx: Arc<Ctx>,
    buffer: wgpu::Buffer,
    len: usize,
    padded: u64,
}

impl Drop for BufferInner {
    fn drop(&mut self) {
        self.ctx
            .counters
            .allocated
            .fetch_sub(self.padded, Ordering::SeqCst);
        self.buffer.destroy();
    }
}

/// A device-resident payload buffer. Cloning shares it (same device memory).
#[derive(Clone)]
pub struct GpuBuffer {
    inner: Arc<BufferInner>,
}

impl std::fmt::Debug for GpuBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuBuffer")
            .field("device", &self.inner.ctx.id)
            .field("len", &self.inner.len)
            .finish()
    }
}

impl GpuBuffer {
    /// Whether this buffer belongs to `ctx` (same logical device).
    pub fn on(&self, ctx: &GpuContext) -> bool {
        Arc::ptr_eq(&self.inner.ctx, &ctx.ctx)
    }

    /// Explicit device→host transfer of the logical bytes (counted).
    pub fn read(&self) -> Result<Vec<u8>, String> {
        let ctx = &self.inner.ctx;
        let staging = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mvf-staging"),
            size: self.inner.padded,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mvf-download"),
            });
        encoder.copy_buffer_to_buffer(&self.inner.buffer, 0, &staging, 0, self.inner.padded);
        ctx.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        staging.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        ctx.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .map_err(|e| format!("device poll: {e}"))?;
        rx.recv()
            .map_err(|_| "map callback dropped".to_string())?
            .map_err(|e| format!("map_async: {e}"))?;
        let bytes = staging
            .slice(..)
            .get_mapped_range()
            .map_err(|e| format!("mapped range: {e}"))?[..self.inner.len]
            .to_vec();
        staging.unmap();
        ctx.counters.downloads.fetch_add(1, Ordering::Relaxed);
        ctx.counters
            .download_bytes
            .fetch_add(self.inner.len as u64, Ordering::Relaxed);
        Ok(bytes)
    }

    /// Logical length.
    pub fn byte_len(&self) -> usize {
        self.inner.len
    }
}

impl DeviceBuffer for GpuBuffer {
    fn device_id(&self) -> &str {
        &self.inner.ctx.id
    }

    fn len(&self) -> usize {
        self.inner.len
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn download(&self) -> Result<Vec<u8>, String> {
        self.read()
    }
}
