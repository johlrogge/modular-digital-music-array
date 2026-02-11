# LSD (Live Sync Deck) - CDJ Controller Project Summary

## Project Overview

Development of a CDJ-style controller for the MDMA system, implementing the DMT (Distributed Musical Timing) protocol. The controller will provide professional DJ hardware interface patterns in software form.

## Product Line Concept

**LSD** = **L**ive **S**ync **D**eck
- **LSD-350**: Entry level Live Sync Deck
- **LSD-2000**: Professional standard  
- **LSD-3000**: Flagship with full visuals and reality-bending features

Tagline: *"Sync deeper. Mix further. Experience music."*

## Technical Architecture

### Hardware Platform
- Raspberry Pi 5 for standalone mode (full playback engine)
- Raspberry Pi 4 sufficient for controller-only mode
- Memory LCD displays (SHARP Memory-in-Pixel technology)
  - Ultra-low power, always-on displays
  - Crisp readability in any lighting
  - Millisecond refresh (much faster than e-ink)
  - Optional backlight available

### Two Operating Modes
1. **Controller Mode**: Pure controller sending DMT commands to remote 909/mixer
2. **Standalone Mode**: Full audio engine + library on device (same playback engine as 909)

### Core Hardware Controls (Physical)
- **Jog Wheel**: Touch-sensitive with multiple modes (Vinyl, Nudge, Search)
- **Transport**: Play/Pause, Cue, dedicated Loop controls  
- **Pitch Control**: Fader with multiple ranges (±6%, ±10%, ±16%)
- **Hot Cues**: 4-8 dedicated buttons for instant access points
- **Browse Controls**: Library navigation and track loading

### Software Architecture
- **DMT Protocol Client**: Connects to 909 master or operates standalone
- **Track Analysis**: BPM detection, beat grid, key detection
- **Audio Engine**: Local playback with effects processing (shared with 909)
- **Library Interface**: Local storage + network library access

## Development Plan

### Phase 1: TUI Interface (Current Focus)
- Create `lsd-tui` using ratatui
- Start with basic track list display and navigation
- Implement music browsing and queuing
- Connect to existing 909 via nng (using current enum system)
- Can be tested over SSH for remote development

### Phase 2: Bevy UI (Future)
- Migrate to Bevy for real-time audio visualization
- Add waveform display, beat grids
- Smooth jog wheel feedback and animations
- Hardware integration preparation

### Hardware Integration
- New type patterns for all controls:
  ```rust
  pub struct JogPosition(i32);     // Encoder ticks
  pub struct PitchPercent(f32);    // -16.0 to +16.0 
  pub struct HotCueIndex(u8);      // 0-7
  pub struct BeatPosition(u32);    // 960 PPQN resolution
  ```

## Design Philosophy

Following Pioneer CDJ evolution principles:
- Maintain tactile control priority for essential functions
- Evolutionary rather than revolutionary interface changes
- Visual hierarchy: critical data prominent, secondary info layered
- Professional environment optimization (dark, high-energy conditions)
- Redundancy and reliability for critical functions

## Network Communication

- Uses existing nng-based enum system initially
- Deck/mixer separation via nng
- DMT protocol extensions as needed
- No external dependencies yet

## Next Steps

1. Create basic TUI with hardcoded track list
2. Add arrow key navigation
3. Implement basic transport controls (play/pause/cue)
4. Connect to 909 music library over network
5. Add queue management
6. Expand DMT protocol for CDJ-specific commands

## Additional Applications

- Remote control capability for simple song switching
- Testing interface for DMT protocol development
- Foundation for hardware CDJ implementation