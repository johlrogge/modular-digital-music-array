# MDMA Audio Architecture

## Overview

The MDMA (Modular Distributed Music Array) audio system is designed with a clear separation between two worlds:

1. **The Tokio World** - Asynchronous, non-real-time operations like file loading and seeking
2. **The Real-time Audio World** - Processing that must happen without interruption

These two worlds are connected by ringbuffers, which serve as safe boundaries for transferring audio data while maintaining real-time performance.

## Key Components

### Tokio World

- **Source (FlacSource)**: Loads and decodes audio data from files asynchronously
- **SegmentedBuffer**: Stores decoded audio data in fixed-size chunks
- **Buffer Management Task**: Background Tokio task that manages loading and prefetching
- **Track State**: Maintains position, volume, and other track parameters

### Real-time Audio World

- **Per-Track Effects**:
  - EQ: Frequency filtering
  - Echo/Delay: Time-based effects
  - Pitch/Tempo: Pitch shifting and time stretching

- **Mixer**:
  - Mix Processing: Combines audio from multiple tracks (possibly using Rayon for parallel processing)
  - Channel Faders & Crossfader: Volume control for individual tracks and transitioning

- **Master Effects**:
  - Master EQ: Final frequency adjustment
  - Master Volume: Overall volume control
  - Limiter: Prevents clipping

- **Audio Output**: Final interface to PipeWire/PulseAudio

## Key Boundaries

The most critical boundary is between the SegmentedBuffer and the Per-Track Effects. This boundary:

1. Decouples asynchronous loading from real-time processing
2. Allows the real-time audio system to continue functioning even if Tokio is temporarily busy
3. Provides a buffer to smooth out timing differences between loading and playback

## Implementation Plan Using Mikado Method

### Phase 1: Tokio/Real-time Boundary (Current Focus)

1. Add ringbuffer to Track struct
2. Modify Track constructor to initialize the ringbuffer
3. Create buffer fill task to transfer data from SegmentedBuffer to ringbuffer
4. Update get_next_samples to read from ringbuffer instead of SegmentedBuffer
5. Add proper handling for ringbuffer underruns
6. Update seeking mechanism to handle the ringbuffer

### Phase 2: Effects and Mixer Refinement

1. Implement basic per-track effects framework
2. Update mixer to process effects chains
3. Improve parallel processing with Rayon
4. Add master effects chain

### Phase 3: Multi-room Distribution

1. Implement network distribution protocol
2. Add synchronization mechanism between nodes
3. Create room-specific processing and configuration

## Notes on Real-time Safety

- All real-time components must avoid:
  - Memory allocation/deallocation
  - Locks that might block (prefer lock-free structures)
  - System calls or I/O operations
  - Anything that could cause unpredictable latency

- The ringbuffer serves as both a data transfer mechanism and a timing buffer to accommodate small variations in processing time