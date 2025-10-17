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
use tokio::runtime::Runtime;

use smelter_core::{
    AudioChannels, AudioMixerConfig, AudioMixerInputConfig, AudioMixingStrategy,
    PipelineOutputEndCondition, ProtocolInputOptions, ProtocolOutputOptions,
    QueueInputOptions, RegisterInputOptions, RegisterOutputAudioOptions,
    RegisterOutputOptions, RegisterOutputVideoOptions,
    codecs::{
        AudioEncoderOptions, FdkAacEncoderOptions, FfmpegH264EncoderOptions,
        FfmpegH264EncoderPreset, OutputPixelFormat, VideoEncoderOptions,
    },
    protocols::{DeckLinkInputOptions, Mp4OutputOptions},
};

use smelter_render::{
    InputId, OutputId, Resolution,
    scene::{Component, InputStreamComponent},
};

pub const RESOLUTION: (usize, usize) = (1920, 1080);

pub struct Compositor {
    _graphics_context: GraphicsContext,
    pipeline: Arc<Mutex<Pipeline>>,
    decklink_inputs: Vec<InputId>,
}

impl Compositor {
    pub fn new() -> Result<Self> {
        // Initialize graphics context
        let graphics_context = GraphicsContext::new(GraphicsContextOptions {
            force_gpu: false,
            features: wgpu::Features::empty(),
            limits: wgpu::Limits::default(),
            compatible_surface: None,
            libvulkan_path: None,
        })
        .map_err(|e| anyhow::anyhow!("Failed to create graphics context: {}", e))?;

        // Create and start pipeline
        let pipeline_options = PipelineOptions {
            stream_fallback_timeout: Duration::from_secs(5),
            default_buffer_duration: Duration::from_millis(80), // ~5 frames at 60fps
            load_system_fonts: true,
            run_late_scheduled_events: false,
            never_drop_output_frames: false,
            ahead_of_time_processing: false,
            output_framerate: Framerate { num: 30, den: 1 },
            mixing_sample_rate: 48000,
            download_root: PathBuf::from("/tmp/smelter-downloads").into(),
            rendering_mode: RenderingMode::GpuOptimized,
            wgpu_options: PipelineWgpuOptions::Context(graphics_context.clone()),
            tokio_rt: Some(Arc::new(Runtime::new().map_err(|e| {
                anyhow::anyhow!("Failed to create Tokio runtime: {}", e)
            })?)),
            chromium_context: None,
            whip_whep_server: PipelineWhipWhepServerOptions::Disable,
            whip_whep_stun_servers: Arc::new(vec![]),
        };

        let pipeline = Pipeline::new(pipeline_options)
            .map_err(|e| anyhow::anyhow!("Failed to create pipeline: {}", e))?;
        let pipeline = Arc::new(Mutex::new(pipeline));

        Pipeline::start(&pipeline);

        // Register DeckLink input (Linux only)
        let decklinks = decklink::get_decklinks().unwrap_or_else(|_| Vec::new());
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
                let input_options = ProtocolInputOptions::DeckLink(
                    smelter_core::protocols::DeckLinkInputOptions {
                        subdevice_index: None,
                        display_name: None,
                        persistent_id: Some(id.clone()),
                        enable_audio: false,
                        pixel_format: Some(decklink::PixelFormat::Format8BitYUV),
                    },
                );
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

        Ok(Self { _graphics_context: graphics_context, pipeline, decklink_inputs })
    }

    pub fn record_main_output(&self, _path: PathBuf, _duration: Duration) -> Result<()> {
        let output_id = OutputId(Arc::from("recording_output"));

        // Configure MP4 output with H.264 video and AAC audio
        let output_options = RegisterOutputOptions {
            output_options: ProtocolOutputOptions::Mp4(Mp4OutputOptions {
                output_path: _path.clone(),
                video: Some(VideoEncoderOptions::FfmpegH264(FfmpegH264EncoderOptions {
                    preset: FfmpegH264EncoderPreset::Fast,
                    resolution: Resolution { width: RESOLUTION.0, height: RESOLUTION.1 },
                    pixel_format: OutputPixelFormat::YUV420P,
                    raw_options: vec![],
                })),
                audio: None,
            }),
            video: Some(RegisterOutputVideoOptions {
                initial: Component::InputStream(InputStreamComponent {
                    id: None,
                    input_id: self.decklink_inputs[0].clone(),
                }),
                end_condition: PipelineOutputEndCondition::AnyInput,
            }),
            audio: None,
        };

        // Register the recording output
        Pipeline::register_output(&self.pipeline, output_id.clone(), output_options)
            .map_err(|e| anyhow::anyhow!("Failed to register recording output: {}", e))?;

        tracing::info!("Recording to {:?} for {:?}", _path, _duration);

        // Wait for the specified duration
        std::thread::sleep(_duration);

        // Stop recording by unregistering the output
        self.pipeline.lock().unwrap().unregister_output(&output_id).map_err(|e| {
            anyhow::anyhow!("Failed to unregister recording output: {}", e)
        })?;

        tracing::info!("Recording completed");

        Ok(())
    }
}
