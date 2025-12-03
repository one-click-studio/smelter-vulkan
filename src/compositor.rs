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
    InputBufferOptions, PipelineOutputEndCondition, ProtocolInputOptions, ProtocolOutputOptions,
    QueueInputOptions, RegisterInputOptions, RegisterOutputOptions, RegisterOutputVideoOptions,
    codecs::{VideoDecoderOptions, VideoEncoderOptions, VulkanH264EncoderOptions},
    protocols::{Mp4InputOptions, Mp4InputSource, Mp4InputVideoDecoders, Mp4OutputOptions},
};

use smelter_render::{
    InputId, OutputId, Resolution,
    scene::{Component, InputStreamComponent},
};

pub const RESOLUTION: (usize, usize) = (1920, 1080);

pub struct Compositor {
    _graphics_context: GraphicsContext,
    pipeline: Arc<Mutex<Pipeline>>,
    input_id: InputId,
}

impl Compositor {
    pub fn new(input_path: PathBuf) -> Result<Self> {
        // Initialize graphics context
        let graphics_context = GraphicsContext::new(GraphicsContextOptions {
            device_id: None,
            driver_name: None,
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
            default_buffer_duration: Duration::from_millis(80),
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

        // Register MP4 input
        let input_id = InputId(Arc::from("mp4_input"));
        let input_options = RegisterInputOptions {
            input_options: ProtocolInputOptions::Mp4(Mp4InputOptions {
                source: Mp4InputSource::File(input_path.into()),
                should_loop: true,
                video_decoders: Mp4InputVideoDecoders {
                    h264: Some(VideoDecoderOptions::VulkanH264),
                },
                buffer: InputBufferOptions::Const(None),
            }),
            queue_options: QueueInputOptions {
                required: true,
                offset: Some(Duration::ZERO),
            },
        };
        Pipeline::register_input(&pipeline, input_id.clone(), input_options)
            .map_err(|e| anyhow::anyhow!("Failed to register MP4 input: {}", e))?;
        tracing::info!("Registered MP4 input");

        Ok(Self {
            _graphics_context: graphics_context,
            pipeline,
            input_id,
        })
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
                    resolution: Resolution {
                        width: RESOLUTION.0,
                        height: RESOLUTION.1,
                    },
                    bitrate: None,
                })),
                audio: None,
                raw_options: vec![],
            }),
            video: Some(RegisterOutputVideoOptions {
                initial: Component::InputStream(InputStreamComponent { id: None, input_id }),
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
        tracing::info!("Starting recording");

        let output_id = OutputId(Arc::from("recording_output"));
        self.start_record(
            path.join("recording.mp4"),
            output_id.clone(),
            self.input_id.clone(),
        )?;

        tracing::info!("Recording for {:?}", duration);

        // Wait for the specified duration
        std::thread::sleep(duration);

        // Stop recording by unregistering output
        self.pipeline
            .lock()
            .unwrap()
            .unregister_output(&output_id)
            .map_err(|e| anyhow::anyhow!("Failed to unregister recording output: {}", e))?;

        std::thread::sleep(Duration::from_millis(500));
        tracing::info!("Recording completed");
        Ok(())
    }
}
