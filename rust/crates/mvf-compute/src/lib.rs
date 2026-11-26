//! # mvf-compute — direct accelerator compute backend
//!
//! Runs `backend: compute, api: wgpu` artifacts (WGSL kernels) on a real GPU through wgpu
//! (Metal on macOS, Vulkan/DX12 elsewhere). Outputs stay in device memory as
//! [`mvf_engine::PayloadData::Device`] payloads; a compute node on the **same logical device**
//! receives the buffer handle with no copy. Everything else is an explicit, counted transfer:
//! host → device on entry, device → host only where a host node (or a compact result) needs bytes.
//! A node whose contract requires zero copy refuses an input that lives on another device.
//!
//! Isolation `none` (in-process, trusted kernels); cooperative cancellation (checked before each
//! dispatch — GPU work in flight cannot be pre-empted).

pub mod context;
pub mod kernels;

use std::sync::Arc;

use mvf_contract::{
    Artifact, BackendCapabilities, BackendKind, Cancellation, ComputeApi, IsolationLevel,
    MemoryDomain, Replacement, Target, ZeroCopy,
};
use mvf_engine::{Descriptor, ElementType, MediaType};
use mvf_engine::{
    DeviceBuffer, ExecContext, Execution, ExecutionBackend, Frame, Inputs, InstantiateRequest,
    NodeError, NodeErrorKind, NodeHandle, Payload, Value,
};

pub use context::{GpuBuffer, GpuConfig, GpuContext, GpuMetrics};
pub use kernels::Kernel;

/// The compute backend for one GPU context.
#[derive(Debug, Clone)]
pub struct ComputeBackend {
    gpu: GpuContext,
}

impl ComputeBackend {
    /// A backend on `gpu`.
    pub fn new(gpu: GpuContext) -> Self {
        Self { gpu }
    }

    /// Opens the default GPU, or `None` when the host has none.
    pub fn detect(config: &GpuConfig) -> Option<Self> {
        GpuContext::new(config).ok().map(Self::new)
    }

    /// The GPU context.
    pub fn gpu(&self) -> &GpuContext {
        &self.gpu
    }
}

impl ExecutionBackend for ComputeBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            kind: BackendKind::Compute,
            isolation: IsolationLevel::None,
            targets: vec![Target::Gpu],
            zero_copy_domains: vec![MemoryDomain::Device],
            cancellation: Cancellation::Cooperative,
            enforceable_termination: false,
            enforces_memory_limit: false,
            enforces_cpu_limit: false,
            enforces_pids_limit: false,
            replacement: vec![Replacement::Stateless],
            grants: Default::default(),
            private_arenas: false,
        }
    }

    fn instantiate(
        &self,
        request: &InstantiateRequest<'_>,
        artifact: &Artifact,
    ) -> Result<Box<dyn NodeHandle>, NodeError> {
        let Artifact::Compute {
            api: ComputeApi::Wgpu,
            kernel,
            entry_point,
        } = artifact
        else {
            return Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "compute backend needs a wgpu artifact",
            ));
        };
        if entry_point != "main" {
            return Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "registered kernels use entry point `main`",
            ));
        }
        let kernel = Kernel::by_name(kernel).ok_or_else(|| {
            NodeError::new(
                NodeErrorKind::Unsupported,
                format!("no registered kernel `{kernel}`"),
            )
        })?;
        Ok(Box::new(ComputeNode::new(
            self.gpu.clone(),
            kernel,
            request.contract.spec.memory.zero_copy == ZeroCopy::Required,
        )))
    }
}

/// One GPU node generation.
#[derive(Debug)]
pub struct ComputeNode {
    gpu: GpuContext,
    kernel: Kernel,
    zero_copy_required: bool,
}

fn resource(e: String) -> NodeError {
    NodeError::new(NodeErrorKind::Resource, e)
}

impl ComputeNode {
    /// A node running `kernel` on `gpu`.
    pub fn new(gpu: GpuContext, kernel: Kernel, zero_copy_required: bool) -> Self {
        Self {
            gpu,
            kernel,
            zero_copy_required,
        }
    }

    /// Brings the input onto this node's device: same device → the handle itself (counted as a
    /// device hand-off, no copy); otherwise an explicit transfer, refused when zero copy is
    /// required.
    fn acquire(&self, payload: &Payload) -> Result<GpuBuffer, NodeError> {
        if let Some(device) = payload.device_buffer() {
            if let Some(buf) = device.as_any().downcast_ref::<GpuBuffer>()
                && buf.on(&self.gpu)
            {
                self.gpu.note_handoff();
                return Ok(buf.clone());
            }
            if self.zero_copy_required {
                return Err(NodeError::new(
                    NodeErrorKind::Unsupported,
                    format!(
                        "zero copy required, but the input lives on device `{}` and this node runs on `{}`",
                        device.device_id(),
                        self.gpu.id()
                    ),
                ));
            }
            let bytes = device.download().map_err(resource)?;
            return self.gpu.upload(&bytes).map_err(resource);
        }
        let bytes = payload.try_bytes().map_err(resource)?;
        self.gpu.upload(bytes).map_err(resource)
    }
}

fn input_payload(inputs: &Inputs) -> Result<(&Payload, Option<&Frame>), NodeError> {
    for (_, v) in inputs.iter() {
        match v {
            Value::Frame(f) => return Ok((&f.payload, Some(f))),
            Value::Tensor(t) => return Ok((t, None)),
            _ => {}
        }
    }
    Err(NodeError::failed("no frame or tensor input"))
}

impl NodeHandle for ComputeNode {
    fn execute(&mut self, ctx: &ExecContext<'_>, inputs: &Inputs) -> Result<Execution, NodeError> {
        if ctx.cancel.is_cancelled() {
            return Err(NodeError::new(
                NodeErrorKind::Cancelled,
                "cancelled before dispatch",
            ));
        }
        let (payload, frame) = input_payload(inputs)?;
        let input = self.acquire(payload)?;
        let len = input.byte_len();
        let as_u32 = |n: usize| u32::try_from(n).map_err(|_| resource("payload too large".into()));
        match self.kernel {
            Kernel::InvertU8 => {
                let out = self.gpu.alloc(len).map_err(resource)?;
                self.gpu
                    .dispatch(
                        Kernel::InvertU8,
                        &input,
                        &out,
                        as_u32(len.div_ceil(4))?,
                        None,
                    )
                    .map_err(resource)?;
                let payload = Payload::device(
                    payload.descriptor.clone(),
                    Arc::new(out) as Arc<dyn DeviceBuffer>,
                )
                .map_err(NodeError::failed)?;
                let value = match frame {
                    Some(f) => Value::Frame(Frame {
                        meta: f.meta.clone(),
                        payload,
                    }),
                    None => Value::Tensor(payload),
                };
                Ok(Execution::Produced(
                    mvf_engine::Outputs::new().with("frame", value),
                ))
            }
            Kernel::NormalizeU8F32 => {
                let out = self.gpu.alloc(len * 4).map_err(resource)?;
                self.gpu
                    .dispatch(Kernel::NormalizeU8F32, &input, &out, as_u32(len)?, None)
                    .map_err(resource)?;
                let descriptor = Descriptor {
                    media: MediaType::Tensor,
                    element: ElementType::F32,
                    shape: payload.descriptor.shape.clone(),
                    epoch: 0,
                };
                let tensor = Payload::device(descriptor, Arc::new(out) as Arc<dyn DeviceBuffer>)
                    .map_err(NodeError::failed)?;
                Ok(Execution::Produced(
                    mvf_engine::Outputs::new().with("tensor", Value::Tensor(tensor)),
                ))
            }
            Kernel::MeanF32 => {
                let count = len / 4;
                let partial = self
                    .gpu
                    .alloc(kernels::MEAN_PARTIALS as usize * 4)
                    .map_err(resource)?;
                self.gpu
                    .dispatch(
                        Kernel::MeanF32,
                        &input,
                        &partial,
                        as_u32(count)?,
                        Some(kernels::MEAN_PARTIALS),
                    )
                    .map_err(resource)?;
                // Compact result: 64 partial sums (256 bytes) cross to the host, not the tensor.
                let bytes = partial.read().map_err(resource)?;
                let sum: f64 = bytes
                    .chunks_exact(4)
                    .map(|c| f64::from(f32::from_le_bytes([c[0], c[1], c[2], c[3]])))
                    .sum();
                let mean = if count == 0 { 0.0 } else { sum / count as f64 };
                Ok(Execution::Produced(mvf_engine::builtins::mean_outputs(
                    mean, count,
                )))
            }
        }
    }
}
