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
    RegisterOutputVideoOptions,
    codecs::{VideoEncoderOptions, VulkanH264EncoderOptions},
    protocols::{DeckLinkInputOptions, Mp4OutputOptions},
};

use smelter_render::{
    InputId, OutputId, Resolution,
    scene::{Component, InputStreamComponent},
};

use crate::window::WgpuContext;

pub const RESOLUTION: (usize, usize) = (3840, 2160);
const FRAME_RATE: u32 = 30;
const AUDIO_SAMPLE_RATE: u32 = 48000;

pub struct Compositor {
    _graphics_context: GraphicsContext,
    pipeline: Arc<Mutex<Pipeline>>,
    decklink_inputs: Vec<InputId>,
}

impl Compositor {
    /// Create a new compositor and return both the compositor and the WGPU context
    /// The WGPU context can be shared with the window manager
    pub fn new() -> Result<(Self, WgpuContext)> {
        // Initialize graphics context via Smelter
        let graphics_context = GraphicsContext::new(GraphicsContextOptions {
            force_gpu: false,
            features: wgpu::Features::empty(),
            limits: wgpu::Limits::default(),
            compatible_surface: None, // No surface at initialization
            libvulkan_path: None,
        })
        .map_err(|e| anyhow::anyhow!("Failed to create graphics context: {}", e))?;

        // Extract WGPU context for sharing with window manager
        let wgpu_context = WgpuContext {
            instance: graphics_context.instance.clone(),
            adapter: graphics_context.adapter.clone(),
            device: graphics_context.device.clone(),
            queue: graphics_context.queue.clone(),
        };

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
        };

        Ok((compositor, wgpu_context))
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
                    resolution: Resolution { width: RESOLUTION.0, height: RESOLUTION.1 },
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

    pub fn start_recording(&self, path: PathBuf, duration: Duration) -> Result<()> {
        tracing::info!("Starting recording with {} DeckLink input(s)", self.decklink_inputs.len());

        if self.decklink_inputs.is_empty() {
            tracing::warn!("No DeckLink inputs registered - nothing to record!");
            return Ok(());
        }

        let mut output_ids = Vec::new();
        for input_id in self.decklink_inputs.clone() {
            let output_id =
                OutputId(Arc::from(format!("recording_output_{}", input_id.0)));
            self.start_record(
                path.join(format!("recording_{}.mp4", input_id.0)),
                output_id.clone(),
                input_id,
            )?;
            output_ids.push(output_id);
        }

        tracing::info!("Recording for {:?}", duration);

        // Wait for the specified duration
        std::thread::sleep(duration);

        // Stop recording by unregistering outputs
        for output_id in output_ids.iter() {
            self.pipeline.lock().unwrap().unregister_output(output_id).map_err(|e| {
                anyhow::anyhow!("Failed to unregister recording output: {}", e)
            })?;
        }

        std::thread::sleep(Duration::from_millis(500));
        tracing::info!("Recording completed");
        Ok(())
    }
}
