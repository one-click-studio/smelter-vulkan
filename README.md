# Smelter-vulkan

Reproduction project for a Vulkan video encoding crash on NVIDIA GPUs.

## Usage

```bash
cargo run
```

## The Crash

When running multiple record/stop cycles, the 9th recording attempt fails:

```
ERROR vk_video::wrappers::debug: [GENERAL][NVIDIA] The maximum number of video encode sessions for this device class has been reached!
Error: Failed to register recording output: Output initialization error while registering output for stream "recording_output_8_0".
Segmentation fault (core dumped)
```

This suggests encode sessions are not being properly released when recordings stop.

## Parameters

Adjust constants in `src/main.rs`:

- `RECORDING_DURATION` - how long each recording lasts
- `NUM_RECORDINGS` - parallel recordings per cycle
- `NUM_CYCLES` - total record/stop cycles to perform
- `PAUSE_BETWEEN_CYCLES` - wait time between cycles
