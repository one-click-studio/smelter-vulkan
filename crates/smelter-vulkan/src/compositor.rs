use anyhow::Result;
use std::time::Duration;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use smelter_core::{
    Pipeline, PipelineOptions, PipelineWgpuOptions, PipelineWhipWhepServerOptions,
    graphics_context::{GraphicsContext, GraphicsContextOptions},
};
use smelter_render::{Framerate, RenderingMode};
use smelter_render::web_renderer::ChromiumContext;
use tokio::runtime::Runtime;

use smelter_core::{
    PipelineOutputEndCondition, ProtocolInputOptions, ProtocolOutputOptions,
    QueueInputOptions, RegisterInputOptions, RegisterOutputOptions,
    RegisterOutputVideoOptions, RegisterRawDataOutputOptions,
    codecs::{VideoEncoderOptions, VulkanH264EncoderOptions},
    protocols::{DeckLinkInputOptions, Mp4OutputOptions, RawDataOutputOptions, RawDataOutputVideoOptions, RawDataOutputReceiver},
};

use smelter_render::{
    InputId, OutputId, Resolution,
    scene::{
        Component, InputStreamComponent, RescalerComponent,
        Position, RescaleMode, HorizontalAlign, VerticalAlign,
        BorderRadius, RGBAColor,
    },
};

use crate::external_memory::{BridgeTextureExport, create_bridge_texture_export};

/// Resolution for MP4 recording outputs (4K)
pub const RECORDING_RESOLUTION: (usize, usize) = (3840, 2160);

/// Resolution for window preview / bridge texture (can be lower for performance)
pub const PREVIEW_RESOLUTION: (usize, usize) = (1920, 1080);

const FRAME_RATE: u32 = 30;
const AUDIO_SAMPLE_RATE: u32 = 48000;

pub struct Compositor {
    _graphics_context: GraphicsContext,
    pipeline: Arc<Mutex<Pipeline>>,
    decklink_inputs: Vec<InputId>,
    bridge_texture: Option<BridgeTextureExport>,
}

use std::os::fd::RawFd;

pub struct CompositorContext {
    pub bridge_memory_fd: RawFd,
    pub resolution: (u32, u32),
}

impl Compositor {
    /// Create a new compositor and return the compositor context with bridge texture FD
    /// The window manager will create its own WGPU instance and import the bridge texture
    pub fn new() -> Result<(Self, CompositorContext)> {
        // Initialize graphics context via Smelter
        let graphics_context = GraphicsContext::new(GraphicsContextOptions {
            force_gpu: false,
            features: wgpu::Features::empty(),
            limits: wgpu::Limits::default(),
            compatible_surface: None, // No surface at initialization
            libvulkan_path: None,
        })
        .map_err(|e| anyhow::anyhow!("Failed to create graphics context: {}", e))?;

        // Set up device error handler for debugging
        graphics_context.device.on_uncaptured_error(Box::new(|error| {
            tracing::error!("WGPU Device Error: {:?}", error);
            panic!("WGPU device error detected: {:?}", error);
        }));

        // Create bridge texture with external memory for sharing with window
        tracing::info!("Creating bridge texture with external memory at {}x{}...",
            PREVIEW_RESOLUTION.0, PREVIEW_RESOLUTION.1);
        let bridge_texture = create_bridge_texture_export(
            &graphics_context.device,
            (PREVIEW_RESOLUTION.0 as u32, PREVIEW_RESOLUTION.1 as u32)
        )
            .map_err(|e| anyhow::anyhow!("Failed to create bridge texture: {}", e))?;

        let bridge_memory_fd = bridge_texture.external_handle.memory_fd;
        tracing::info!("Bridge texture created with FD: {}", bridge_memory_fd);

        // Create ChromiumContext for web rendering
        let framerate = Framerate { num: FRAME_RATE, den: 1 };
        let chromium_context = ChromiumContext::new(framerate, true)
            .map_err(|e| anyhow::anyhow!("Failed to create Chromium context: {}", e))?;

        // Create and start pipeline
        let pipeline_options = PipelineOptions {
            stream_fallback_timeout: Duration::from_millis(500),
            default_buffer_duration: Duration::from_millis(100),
            load_system_fonts: false,
            run_late_scheduled_events: true,
            never_drop_output_frames: false,
            ahead_of_time_processing: false,
            output_framerate: framerate,
            mixing_sample_rate: AUDIO_SAMPLE_RATE,
            download_root: PathBuf::from("/tmp/smelter-downloads").into(),
            rendering_mode: RenderingMode::GpuOptimized,
            wgpu_options: PipelineWgpuOptions::Context(graphics_context.clone()),
            tokio_rt: Some(Arc::new(Runtime::new().map_err(|e| {
                anyhow::anyhow!("Failed to create Tokio runtime: {}", e)
            })?)),
            chromium_context: Some(chromium_context),
            whip_whep_server: PipelineWhipWhepServerOptions::Disable,
            whip_whep_stun_servers: Arc::new(vec![]),
        };

        let pipeline = Pipeline::new(pipeline_options)
            .map_err(|e| anyhow::anyhow!("Failed to create pipeline: {}", e))?;
        let pipeline = Arc::new(Mutex::new(pipeline));

        Pipeline::start(&pipeline);

        // Register DeckLink input (Linux only)
        let decklinks = decklink::get_decklinks().unwrap_or_else(|e| {
            tracing::warn!("Failed to get DeckLink devices: {}", e);
            Vec::new()
        });
        tracing::info!("Found {} DeckLink device(s)", decklinks.len());
        let decklink_inputs = decklinks
            .iter()
            .filter_map(|decklink| {
                // Retrieving the persistent_id is mandatory,
                // it is the only way to later specify which device to use as an input
                let attr = match decklink.profile_attributes() {
                    Ok(attr) => attr,
                    Err(_) => return None,
                };
                let persistent_id =
                    match attr.get_integer(decklink::IntegerAttributeId::PersistentID) {
                        Ok(Some(id)) => id as u32,
                        _ => return None,
                    };
                Some(persistent_id)
            })
            .filter_map(|id| {
                let input_options =
                    ProtocolInputOptions::DeckLink(DeckLinkInputOptions {
                        subdevice_index: None,
                        display_name: None,
                        persistent_id: Some(id.clone()),
                        enable_audio: false,
                        pixel_format: Some(decklink::PixelFormat::Format8BitYUV),
                    });
                let options = RegisterInputOptions {
                    input_options,
                    queue_options: QueueInputOptions { required: false, offset: None },
                };

                let decklink_input = InputId(Arc::from(format!("decklink_input_{}", id)));
                match Pipeline::register_input(&pipeline, decklink_input.clone(), options)
                {
                    Ok(_) => {
                        tracing::info!(
                            "Registered DeckLink input with persistent ID {}",
                            id
                        );
                        Some(decklink_input)
                    }
                    Err(e) => {
                        tracing::error!(
                            "Error registering DeckLink input with persistent ID {}: {}",
                            id,
                            e
                        );
                        None
                    }
                }
            })
            .collect::<Vec<_>>();

        let compositor = Self {
            _graphics_context: graphics_context,
            pipeline,
            decklink_inputs,
            bridge_texture: Some(bridge_texture),
        };

        let context = CompositorContext {
            bridge_memory_fd,
            resolution: (PREVIEW_RESOLUTION.0 as u32, PREVIEW_RESOLUTION.1 as u32),
        };

        Ok((compositor, context))
    }

    pub fn start_record(
        &self,
        path: PathBuf,
        output_id: OutputId,
        input_id: InputId,
    ) -> Result<()> {
        // Configure MP4 output with H.264 video and AAC audio
        let output_options = RegisterOutputOptions {
            output_options: ProtocolOutputOptions::Mp4(Mp4OutputOptions {
                output_path: path.clone(),
                video: Some(VideoEncoderOptions::VulkanH264(VulkanH264EncoderOptions {
                    resolution: Resolution { width: RECORDING_RESOLUTION.0, height: RECORDING_RESOLUTION.1 },
                    bitrate: None,
                })),
                audio: None,
            }),
            video: Some(RegisterOutputVideoOptions {
                initial: Component::InputStream(InputStreamComponent {
                    id: None,
                    input_id,
                }),
                end_condition: PipelineOutputEndCondition::AnyInput,
            }),
            audio: None,
        };

        // Register the recording output
        Pipeline::register_output(&self.pipeline, output_id.clone(), output_options)
            .map_err(|e| anyhow::anyhow!("Failed to register recording output: {}", e))?;

        tracing::info!("Recording {:?} at {:?}", output_id, path);

        Ok(())
    }

    /// Start recording outputs and return the output IDs
    /// Caller should sleep for desired duration, then call stop_recording()
    pub fn start_recording_outputs(&self, path: PathBuf, max_decklinks: usize) -> Result<Vec<OutputId>> {
        let num_to_record = std::cmp::min(max_decklinks, self.decklink_inputs.len());
        tracing::info!("Starting recording with {} of {} DeckLink input(s)", num_to_record, self.decklink_inputs.len());

        if self.decklink_inputs.is_empty() {
            tracing::warn!("No DeckLink inputs registered - nothing to record!");
            return Ok(Vec::new());
        }

        let mut output_ids = Vec::new();
        for input_id in self.decklink_inputs.iter().take(num_to_record).cloned() {
            let output_id =
                OutputId(Arc::from(format!("recording_output_{}", input_id.0)));
            self.start_record(
                path.join(format!("recording_{}.mp4", input_id.0)),
                output_id.clone(),
                input_id,
            )?;
            output_ids.push(output_id);
        }

        Ok(output_ids)
    }

    /// Stop recording by unregistering the given outputs
    pub fn stop_recording_outputs(&self, output_ids: Vec<OutputId>) -> Result<()> {
        for output_id in output_ids.iter() {
            self.pipeline.lock().unwrap().unregister_output(output_id).map_err(|e| {
                anyhow::anyhow!("Failed to unregister recording output: {}", e)
            })?;
        }

        std::thread::sleep(Duration::from_millis(500));
        tracing::info!("Recording completed");
        Ok(())
    }

    /// Get the first DeckLink input ID if available
    pub fn first_decklink_input(&self) -> Option<InputId> {
        self.decklink_inputs.first().cloned()
    }

    /// Register a raw data output with bounded(1) channels for window preview
    ///
    /// This provides tighter backpressure than the default bounded(100), coordinating
    /// frame production and consumption similar to how vk-video encoders work.
    pub fn register_window_preview_output(
        &self,
        output_id: OutputId,
        input_id: InputId,
    ) -> Result<RawDataOutputReceiver> {
        let register_options = RegisterRawDataOutputOptions {
            output_options: RawDataOutputOptions {
                video: Some(RawDataOutputVideoOptions {
                    resolution: Resolution {
                        width: PREVIEW_RESOLUTION.0,
                        height: PREVIEW_RESOLUTION.1,
                    },
                }),
                audio: None,
            },
            video: Some(RegisterOutputVideoOptions {
                initial: Component::Rescaler(RescalerComponent {
                    id: None,
                    child: Box::new(Component::InputStream(InputStreamComponent {
                        id: None,
                        input_id,
                    })),
                    position: Position::Static {
                        width: Some(PREVIEW_RESOLUTION.0 as f32),
                        height: Some(PREVIEW_RESOLUTION.1 as f32),
                    },
                    transition: None,
                    mode: RescaleMode::Fill,
                    horizontal_align: HorizontalAlign::Center,
                    vertical_align: VerticalAlign::Center,
                    border_radius: BorderRadius {
                        top_left: 0.0,
                        top_right: 0.0,
                        bottom_right: 0.0,
                        bottom_left: 0.0,
                    },
                    border_width: 0.0,
                    border_color: RGBAColor(0, 0, 0, 0),
                    box_shadow: vec![],
                }),
                end_condition: PipelineOutputEndCondition::Never,
            }),
            audio: None,
        };

        // Register raw data output (will use bounded(1) after we patch smelter-core)
        let receiver = Pipeline::register_raw_data_output(
            &self.pipeline,
            output_id.clone(),
            register_options,
        )?;

        tracing::info!("Registered window preview output: {:?}", output_id);

        Ok(receiver)
    }

    /// Copy a frame from Smelter's output to the bridge texture
    /// This should be called on the compositor side after receiving a frame
    pub fn copy_frame_to_bridge(&self, frame_texture: &wgpu::Texture) -> Result<()> {
        let Some(ref bridge_texture) = self.bridge_texture else {
            anyhow::bail!("Bridge texture not initialized");
        };

        // Get the actual size of the incoming frame texture
        let frame_size = frame_texture.size();
        let bridge_size = bridge_texture.wgpu_texture.size();

        // Verify the frame size matches the bridge texture
        if frame_size.width != bridge_size.width || frame_size.height != bridge_size.height {
            tracing::debug!(
                "Skipping frame with size {}x{} (bridge texture is {}x{})",
                frame_size.width,
                frame_size.height,
                bridge_size.width,
                bridge_size.height
            );
            return Ok(()); // Skip frames that don't match
        }

        // Create command encoder
        let mut encoder = self._graphics_context.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor {
                label: Some("Bridge Copy Encoder"),
            },
        );

        // Copy from Smelter's frame texture to bridge texture
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: frame_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &bridge_texture.wgpu_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            frame_size,
        );

        // Submit the copy command
        self._graphics_context.queue.submit(Some(encoder.finish()));

        // Force GPU to complete the copy before returning
        let _ = self._graphics_context.device.poll(wgpu::MaintainBase::Wait);

        Ok(())
    }

    /// Get reference to the bridge texture for advanced operations
    #[allow(dead_code)]
    pub fn bridge_texture(&self) -> Option<&wgpu::Texture> {
        self.bridge_texture.as_ref().map(|b| &b.wgpu_texture)
    }
}

