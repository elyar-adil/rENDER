//! GPU presentation through a wgpu/D3D12 swapchain.
//!
//! The compositor worker composes a page surface on the CPU and hands it to
//! [`GpuPresenter`] through the [`PlatformPresenter`] contract. The presenter
//! uploads the frame into a texture, draws one fullscreen triangle through a
//! wgpu render pipeline, and presents the window swapchain. That keeps the
//! presentation on the same worker thread as the scroll composition while
//! moving the final blit and scan-out off the CPU.
//!
//! The softbuffer presenter remains the fallback when no adapter/surface is
//! available, so a machine without a usable D3D12 device still runs the shell.

use std::sync::Arc;
use std::time::{Duration, Instant};

use render_core::paint::Surface;
use winit::window::Window;

use crate::compositor::PlatformPresenter;

const SHADER: &str = r"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(0) @binding(0) var frame_texture: texture_2d<f32>;
@group(0) @binding(1) var frame_sampler: sampler;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );
    var output: VertexOutput;
    output.position = vec4<f32>(positions[index], 0.0, 1.0);
    output.uv = uvs[index];
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(frame_texture, frame_sampler, input.uv);
}
";

/// wgpu requires buffer-copy rows to start on a 256-byte boundary.
const COPY_BYTES_PER_ROW_ALIGNMENT: usize = 256;

/// A wgpu swapchain presenter for the browser window.
pub struct GpuPresenter {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    frame_format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    base: Vec<u32>,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    adapter_name: String,
    backend: String,
    presented_frames: u64,
    skipped_frames: u64,
}

impl GpuPresenter {
    /// Creates the GPU surface on the current thread.
    ///
    /// The surface, device and queue are `Send`, so the compositor worker can
    /// take ownership of the presenter and present on its own thread. On a
    /// machine with no usable D3D12 adapter this returns an error and the shell
    /// falls back to its softbuffer presenter.
    ///
    /// # Errors
    ///
    /// Returns a human-readable error when the window surface, adapter, device
    /// or pipeline cannot be created.
    pub fn new(window: Arc<Window>) -> Result<Self, String> {
        let size = window.inner_size();
        let backends = if cfg!(target_os = "windows") {
            wgpu::Backends::DX12
        } else {
            wgpu::Backends::PRIMARY
        };
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = instance
            .create_surface(window)
            .map_err(|error| format!("create wgpu surface: {error}"))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .map_err(|error| format!("request GPU adapter: {error}"))?;
        let adapter_info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("rENDER GPU presenter"),
            ..Default::default()
        }))
        .map_err(|error| format!("request GPU device: {error}"))?;
        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .or_else(|| capabilities.formats.first().copied())
            .ok_or_else(|| "the GPU surface exposes no texture format".to_owned())?;
        if size.width == 0 || size.height == 0 {
            return Err(format!(
                "the window has no GPU-presentable size ({}x{})",
                size.width, size.height
            ));
        }
        let mut config = surface
            .get_default_config(&adapter, size.width, size.height)
            .ok_or_else(|| "the GPU surface has no default configuration".to_owned())?;
        config.format = format;
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&device, &config);

        // Match the sampling format to the swapchain. When the surface is
        // sRGB, sampling an sRGB texture and writing to the sRGB target keeps
        // the compositor's byte values unchanged after the round trip.
        let frame_format = if config.format.is_srgb() {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        let resources = create_render_resources(
            &device,
            config.format,
            frame_format,
            config.width,
            config.height,
        );
        Ok(Self {
            device,
            queue,
            surface,
            config,
            frame_format,
            pipeline: resources.pipeline,
            bind_group_layout: resources.bind_group_layout,
            sampler: resources.sampler,
            texture: resources.texture,
            bind_group: resources.bind_group,
            base: Vec::new(),
            rgba: Vec::new(),
            width: 0,
            height: 0,
            adapter_name: adapter_info.name.clone(),
            backend: format!("{:?}", adapter_info.backend),
            presented_frames: 0,
            skipped_frames: 0,
        })
    }

    /// The adapter name and backend, as one line for the shell log.
    #[must_use]
    pub fn adapter_info(&self) -> String {
        format!("{} ({})", self.adapter_name, self.backend)
    }

    /// The name reported by the wgpu adapter.
    #[must_use]
    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    /// The wgpu backend selected for the adapter, such as `Dx12`.
    #[must_use]
    pub fn backend(&self) -> &str {
        &self.backend
    }

    /// Number of frames this presenter actually handed to the swapchain.
    ///
    /// A minimized or occluded window can skip a frame without failing the
    /// compositor, so the benchmark uses this counter as its success check.
    #[must_use]
    pub const fn presented_frames(&self) -> u64 {
        self.presented_frames
    }

    /// Number of frames skipped because the swapchain timed out or the window
    /// was occluded.
    #[must_use]
    pub const fn skipped_frames(&self) -> u64 {
        self.skipped_frames
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<(), String> {
        if width == 0 || height == 0 {
            return Err(format!("cannot size the GPU surface to {width}x{height}"));
        }
        if self.config.width == width && self.config.height == height {
            return Ok(());
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        let (texture, bind_group) = create_frame_texture(
            &self.device,
            &self.bind_group_layout,
            &self.sampler,
            self.frame_format,
            width,
            height,
        );
        self.texture = texture;
        self.bind_group = bind_group;
        Ok(())
    }

    /// Acquires the next swapchain texture, reconfiguring once if the surface
    /// reports that it is outdated. `None` means the window is not drawable
    /// right now (minimized or occluded).
    fn acquire_frame(&self) -> Result<Option<wgpu::SurfaceTexture>, String> {
        let mut reconfigured = false;
        loop {
            match self.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(frame)
                | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => return Ok(Some(frame)),
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                    if reconfigured {
                        return Err(
                            "GPU swapchain was still outdated after reconfiguration".to_owned()
                        );
                    }
                    self.surface.configure(&self.device, &self.config);
                    reconfigured = true;
                }
                wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                    return Ok(None);
                }
                wgpu::CurrentSurfaceTexture::Validation => {
                    return Err("GPU swapchain validation error".to_owned());
                }
            }
        }
    }

    fn upload_and_present(&mut self) -> Result<(), String> {
        let width = self.width;
        let height = self.height;
        if width == 0 || height == 0 || self.base.len() != frame_pixels(width, height)? {
            return Err("GPU base frame and surface sizes differ".to_owned());
        }
        fill_rgba(&self.base, width, height, &mut self.rgba)?;
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &self.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(u32::try_from(rgba_row_bytes(width)?).map_err(|_| {
                    "the GPU row stride does not fit in a texture copy layout".to_owned()
                })?),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let Some(frame) = self.acquire_frame()? else {
            // A minimized window may not hand out a drawable every frame.
            // Skipping is not a present failure: the next scroll frame
            // acquires a fresh texture.
            self.skipped_frames = self.skipped_frames.saturating_add(1);
            return Ok(());
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rENDER GPU presenter encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rENDER GPU presenter pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| format!("wait for the GPU frame: {error}"))?;
        self.presented_frames = self.presented_frames.saturating_add(1);
        Ok(())
    }
}

/// Pixel count of a full-window frame, with overflow reported as an error.
fn frame_pixels(width: u32, height: u32) -> Result<usize, String> {
    usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or_else(|| "the GPU frame dimensions overflow".to_owned())
}

/// Bytes in one GPU upload row, padded to wgpu's copy alignment.
fn rgba_row_bytes(width: u32) -> Result<usize, String> {
    let unpadded = usize::try_from(width)
        .map_err(|_| "the GPU frame width does not fit in memory".to_owned())?
        .checked_mul(4)
        .ok_or_else(|| "the GPU frame width overflows its row stride".to_owned())?;
    unpadded
        .checked_next_multiple_of(COPY_BYTES_PER_ROW_ALIGNMENT)
        .ok_or_else(|| "the GPU frame row stride overflows".to_owned())
}

fn channel(pixel: u32, shift: u32) -> u8 {
    u8::try_from((pixel >> shift) & 0xff).expect("a masked color channel always fits in u8")
}

/// Converts the compositor's packed ARGB frame into wgpu's padded RGBA8 rows.
///
/// The alpha byte is always opaque because the browser window frame is fully
/// composed. Padding bytes stay zero so a reused buffer cannot leak a previous
/// frame's pixels into the upload.
fn fill_rgba(frame: &[u32], width: u32, height: u32, rgba: &mut Vec<u8>) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err(format!("cannot upload an empty {width}x{height} frame"));
    }
    let expected = frame_pixels(width, height)?;
    if frame.len() != expected {
        return Err(format!(
            "the GPU frame has {} pixels but {width}x{height} needs {expected}",
            frame.len()
        ));
    }
    let width = usize::try_from(width)
        .map_err(|_| "the GPU frame width does not fit in memory".to_owned())?;
    let height = usize::try_from(height)
        .map_err(|_| "the GPU frame height does not fit in memory".to_owned())?;
    let row_bytes = rgba_row_bytes(
        u32::try_from(width).map_err(|_| "the GPU frame width does not fit in u32".to_owned())?,
    )?;
    let total = row_bytes
        .checked_mul(height)
        .ok_or_else(|| "the GPU upload buffer overflows memory".to_owned())?;
    rgba.clear();
    rgba.resize(total, 0);
    let row_pixels = width * 4;
    for (row, source) in frame.chunks_exact(width).enumerate() {
        let start = row * row_bytes;
        let destination = &mut rgba[start..start + row_pixels];
        for (pixel, output) in source.iter().zip(destination.chunks_exact_mut(4)) {
            output[0] = channel(*pixel, 16);
            output[1] = channel(*pixel, 8);
            output[2] = channel(*pixel, 0);
            output[3] = 0xff;
        }
    }
    Ok(())
}

struct RenderResources {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

fn create_render_resources(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    frame_format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> RenderResources {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("rENDER GPU presenter shader"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("rENDER GPU presenter bindings"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("rENDER GPU presenter pipeline layout"),
        bind_group_layouts: &[Some(&bind_group_layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("rENDER GPU presenter pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("rENDER GPU presenter sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let (texture, bind_group) = create_frame_texture(
        device,
        &bind_group_layout,
        &sampler,
        frame_format,
        width,
        height,
    );
    RenderResources {
        pipeline,
        bind_group_layout,
        sampler,
        texture,
        bind_group,
    }
}

fn create_frame_texture(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::BindGroup) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rENDER GPU presenter frame"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rENDER GPU presenter bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    (texture, bind_group)
}

impl PlatformPresenter for GpuPresenter {
    fn present_full(&mut self, frame: &[u32], width: u32, height: u32) -> Result<(), String> {
        let expected = frame_pixels(width, height)?;
        if frame.len() != expected {
            return Err(format!(
                "the window frame has {} pixels but {width}x{height} needs {expected}",
                frame.len()
            ));
        }
        self.resize(width, height)?;
        self.base.clear();
        self.base.extend_from_slice(frame);
        self.width = width;
        self.height = height;
        self.upload_and_present()
    }

    fn present_page(
        &mut self,
        page: &Surface,
        origin_x: u32,
        origin_y: u32,
    ) -> Result<Duration, String> {
        let started = Instant::now();
        if self.width == 0 || self.height == 0 || self.base.is_empty() {
            return Err("no full window frame has been presented yet".to_owned());
        }
        let page_width = page.width() as usize;
        let copy_width = page_width.min((self.width.saturating_sub(origin_x)) as usize);
        let copy_height =
            (page.height() as usize).min((self.height.saturating_sub(origin_y)) as usize);
        let pixels = page.pixels();
        for row in 0..copy_height {
            let source_start = row * page_width;
            let destination_start =
                (origin_y as usize + row) * self.width as usize + origin_x as usize;
            for column in 0..copy_width {
                let color = pixels[source_start + column];
                self.base[destination_start + column] = (u32::from(color.red) << 16)
                    | (u32::from(color.green) << 8)
                    | u32::from(color.blue);
            }
        }
        self.upload_and_present()?;
        Ok(started.elapsed())
    }
}

#[cfg(test)]
mod tests {
    use super::{COPY_BYTES_PER_ROW_ALIGNMENT, SHADER, fill_rgba, rgba_row_bytes};

    /// The shader is validated by naga during module creation in the live
    /// path; this cheap test catches an accidental edit before a GPU run.
    #[test]
    fn shader_source_has_both_entry_points() {
        assert!(SHADER.contains("fn vs_main"));
        assert!(SHADER.contains("fn fs_main"));
        assert!(SHADER.contains("textureSample"));
    }

    /// wgpu rejects a texture upload whose rows are not 256-byte aligned, so
    /// the conversion helper must pad every row, including the last one.
    #[test]
    fn rgba_rows_use_the_copy_alignment() {
        assert_eq!(rgba_row_bytes(1), Ok(COPY_BYTES_PER_ROW_ALIGNMENT));
        assert_eq!(rgba_row_bytes(64), Ok(COPY_BYTES_PER_ROW_ALIGNMENT));
        assert_eq!(rgba_row_bytes(65), Ok(COPY_BYTES_PER_ROW_ALIGNMENT * 2));
        assert_eq!(rgba_row_bytes(0), Ok(0));
    }

    /// The upload must reorder the compositor's ARGB words into RGBA bytes,
    /// force opaque alpha, and leave padding clear.
    #[test]
    fn argb_frame_converts_to_padded_rgba8() {
        let frame = [0x0011_2233, 0x00a0_b0c0];
        let mut rgba = vec![0xaa; 1024];
        fill_rgba(&frame, 2, 1, &mut rgba).expect("two pixels fit a two-column row");
        assert_eq!(
            rgba.len(),
            COPY_BYTES_PER_ROW_ALIGNMENT,
            "one two-pixel row still occupies a full aligned row"
        );
        assert_eq!(
            &rgba[..8],
            &[0x11, 0x22, 0x33, 0xff, 0xa0, 0xb0, 0xc0, 0xff]
        );
        assert!(
            rgba[8..].iter().all(|byte| *byte == 0),
            "row padding must be zero even when the output buffer is reused"
        );
    }

    #[test]
    fn frame_conversion_rejects_a_mismatched_surface() {
        let mut rgba = Vec::new();
        let error = fill_rgba(&[0x0011_2233], 2, 2, &mut rgba).expect_err("short frame");
        assert!(error.contains("needs 4"));
    }
}
