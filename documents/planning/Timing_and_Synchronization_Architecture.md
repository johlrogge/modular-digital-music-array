# Timing and Synchronization Architecture

## Overview
The system uses a distributed musical clock with high-level sync commands instead of raw audio streaming. This enables precise musical timing, creative DJ features, and efficient network usage while maintaining flexibility for future enhancements.

## Core Components

### Distributed Clock
- Uses OSC (Open Sound Control) for network communication
- Master/follower architecture for time synchronization
- Musical time representation (measures, beats, ticks)
- 960 PPQN (Pulses Per Quarter Note) resolution
- Handles tempo changes and drift compensation

### Tempo Management
- Real-time BPM detection using aubio library
- Smooth tempo ramping capabilities
- Both manual and automatic tempo control
- Beat phase detection and alignment
- Configurable tempo transition curves

### Command Distribution
- High-level musical commands instead of audio streaming
- Each node maintains local audio files
- Commands include:
  - Track loading and positioning
  - Playback control
  - Volume and EQ changes
  - Effect parameters
  - Crossfade instructions

## Key Features

### DJ-Specific Functionality
- Manual tempo control for beatmatching
- Automatic tempo sync between tracks
- Beat grid detection and alignment
- Recording and editing of DJ sessions
- Multiple crossfade types (linear, exponential, S-curve)

### Performance Benefits
- Reduced network bandwidth requirements
- Lower latency than audio streaming
- More robust to network issues
- Precise musical timing
- Scalable to multiple rooms/zones

### Creative Possibilities
- High-level mixing control
- Effect automation
- Complex transitions
- Session recording and editing
- Remote collaboration potential

## Technical Details

### Network Protocol
```
OSC Messages:
/clock/tick    - Current musical time
/clock/tempo   - Tempo changes
/clock/command - Playback commands
```

### Timing Resolution
- Base clock: 960 PPQN
- Tempo range: 20-400 BPM
- Timing precision: ~0.5ms
- Network jitter tolerance: ~10ms

### Future Expansion Options

#### SuperCollider Integration
- Could be added later for sample-accurate timing
- Would provide sub-millisecond precision
- Useful for synthesis and effects
- Higher resource requirements

#### Additional Features
- Beat slicing and looping
- Real-time tempo detection
- Advanced beat grid editing
- Multi-zone sync groups
- Remote DJ handoffs

## Implementation Notes

### Current Approach
- Starting with OSC-based sync
- Focus on reliability and simplicity
- Built-in tempo management
- Extensible command system

### Planned Enhancements
- Enhanced drift compensation
- More sophisticated tempo curves
- Beat phase alignment
- Session recording format
- Remote control protocol

## Usage Examples

```rust
// Basic tempo change
tempo_manager.ramp_tempo(126.5, 4.0);  // To 126.5 BPM over 4 beats

// Load and analyze track
let bpm = tempo_manager.load_track_with_tempo("track.mp3").await?;

// Schedule playback
clock.schedule_at(target_tick, || {
    play_track("track.mp3");
});
```

## Design Decisions

### Why OSC Over Raw Audio
1. Lower bandwidth requirements
2. More robust to network issues
3. Enables high-level control
4. Better scalability
5. Simpler implementation

### Why Not SuperCollider (Initially)
1. Additional dependency
2. Higher complexity
3. Resource overhead
4. Current precision sufficient
5. Can add later if needed
