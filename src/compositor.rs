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
    PipelineOutputEndCondition, QueueInputOptions, RegisterInputOptions,
    RegisterOutputAudioOptions, RegisterOutputOptions, RegisterOutputVideoOptions,
    codecs::{
        AudioEncoderOptions, FdkAacEncoderOptions, FfmpegH264EncoderOptions,
        FfmpegH264EncoderPreset, OutputPixelFormat, VideoEncoderOptions,
    },
    protocols::{
        DeckLinkInputOptions, Mp4OutputOptions, ProtocolInputOptions, ProtocolOutputOptions,
    },
};

use smelter_render::{
    InputId, OutputId, Resolution,
    scene::{Component, InputStreamComponent},
};

pub const RESOLUTION_FHD: (usize, usize) = (1920, 1080);

pub struct Compositor {
    _graphics_context: GraphicsContext,
    #[cfg(target_os = "linux")]
    pipeline: Arc<Mutex<Pipeline>>,
    #[cfg(target_os = "linux")]
    decklink_input: InputId,
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
        decklinks
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
            .for_each(|id| {
                let decklink_input_id = InputId::from(&id);
                let input_options = RegisterInputOptions {
                    input_options: ProtocolInputOptions::DeckLink(DeckLinkInputOptions {
                        display_name: None, // Auto-detect first available DeckLink device
                        subdevice_index: None,
                        persistent_id: Some(id.clone()),
                        enable_audio: false,
                        pixel_format: None, // Auto-detect
                    }),
                    queue_options: QueueInputOptions {
                        required: true,
                        offset: Some(Duration::ZERO),
                    },
                };

                match Pipeline::register_input(&pipeline, decklink_input_id, input_options)
                    .map_err(|e| anyhow::anyhow!("Failed to register DeckLink input: {}", e))
                {
                    Ok(_) => {
                        tracing::info!("Registered DeckLink input with persistent ID {}", id);
                    }
                    Err(e) => {
                        tracing::error!(
                            "Error registering DeckLink input with persistent ID {}: {}",
                            id,
                            e
                        );
                    }
                }
            });

        Ok(Self {
            _graphics_context: graphics_context,
            pipeline,
            decklink_input,
        })
    }

    pub fn record_main_output(&self, _path: PathBuf, _duration: Duration) -> Result<()> {
        let output_id = OutputId(Arc::from("recording_output"));

        // Configure MP4 output with H.264 video and AAC audio
        let output_options = RegisterOutputOptions {
            output_options: ProtocolOutputOptions::Mp4(Mp4OutputOptions {
                output_path: _path.clone(),
                video: Some(VideoEncoderOptions::FfmpegH264(FfmpegH264EncoderOptions {
                    preset: FfmpegH264EncoderPreset::Fast,
                    resolution: Resolution {
                        width: RESOLUTION_FHD.0,
                        height: RESOLUTION_FHD.1,
                    },
                    pixel_format: OutputPixelFormat::YUV420P,
                    raw_options: vec![],
                })),
                audio: Some(AudioEncoderOptions::FdkAac(FdkAacEncoderOptions {
                    channels: AudioChannels::Stereo,
                    sample_rate: 48000,
                })),
            }),
            video: Some(RegisterOutputVideoOptions {
                initial: Component::InputStream(InputStreamComponent {
                    id: None,
                    input_id: self.decklink_input.clone(),
                }),
                end_condition: PipelineOutputEndCondition::AnyInput,
            }),
            audio: Some(RegisterOutputAudioOptions {
                initial: AudioMixerConfig {
                    inputs: vec![AudioMixerInputConfig {
                        input_id: self.decklink_input.clone(),
                        volume: 1.0,
                    }],
                },
                mixing_strategy: AudioMixingStrategy::SumClip,
                channels: AudioChannels::Stereo,
                end_condition: PipelineOutputEndCondition::AnyInput,
            }),
        };

        // Register the recording output
        Pipeline::register_output(&self.pipeline, output_id.clone(), output_options)
            .map_err(|e| anyhow::anyhow!("Failed to register recording output: {}", e))?;

        tracing::info!("Recording to {:?} for {:?}", _path, _duration);

        // Wait for the specified duration
        std::thread::sleep(_duration);

        // Stop recording by unregistering the output
        self.pipeline
            .lock()
            .unwrap()
            .unregister_output(&output_id)
            .map_err(|e| anyhow::anyhow!("Failed to unregister recording output: {}", e))?;

        tracing::info!("Recording completed");

        Ok(())
    }
}
