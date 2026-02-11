# Modular Distributed Music Array (MDMA) System Architecture Overview

## System Components

### MDMA-909 (Master Unit)
Primary control unit with full DJ capabilities and library management.

#### Hardware
- Raspberry Pi 5 (16GB RAM)
- 512GB NVMe Storage
- ifi zen 3 DAC
- Full control interface with display and performance controls

#### Software Components
- Complete music library
- DJ control interface
- Track analysis engine
- Master Distributed Musical Timing (DMT) protocol server
- Library management system
- Network coordination
- Cache management for 303 units

### MDMA-303 (Satellite Unit)
Optimized playback unit for synchronized multi-room audio.

#### Hardware
- Raspberry Pi CM4/5
- 32/64GB eMMC Storage
- ifi zen 3 DAC
- Basic control interface

#### Software Components
- Distributed Musical Timing (DMT) protocol client
- Local audio playback
- Smart caching system
- Basic transport controls
- Health monitoring
- Network synchronization

## Core Technologies

### DMT (Distributed Musical Timing)
Synchronization protocol ensuring precise musical timing across the system.

#### Features
- 960 PPQN resolution
- OSC-based communication
- Master/follower clock architecture
- Tempo management
- Beat phase alignment
- Network jitter compensation

### Network Architecture

#### Transfer Methods
1. Primary: NFS Mount
   - Direct playback capability
   - Low latency access
   - Efficient caching

2. Backup: HTTP
   - Robust fallback system
   - Range request support
   - Cache-friendly

#### Performance
Typical transfer speeds:
- Gigabit Ethernet: ~0.8s per track
- WiFi AC: ~4s per track
- WiFi N: ~16s per track

### Caching System

#### Strategy
1. Immediate Next Track
   - Priority transfer for queued tracks
   - Dynamic speed allocation

2. Set Caching
   - Pre-fetch capability for planned sets
   - ~10 tracks per GB cache
   - LRU cache management

3. Predictive Caching
   - Pattern analysis
   - Priority-based storage
   - Network usage optimization

## Software Stack

### Operating System
- NixOS base system
- Declarative configuration
- Immutable system design
- Robust package management

### Audio Infrastructure
- PipeWire audio server
- Low-latency processing
- Network streaming support
- Hardware compatibility

### Development Stack
- Primary implementation in Rust
- OSC for network communication
- React-based user interface
- Audio processing libraries (aubio, etc.)

## Operation Modes

### Master (909)
- Full library management
- DJ performance interface
- Set planning and analysis
- Network coordination
- Cache management

### Satellite (303)
- Synchronized playback
- Local cache management
- Basic transport controls
- Health monitoring
- Network sync maintenance

## Future Expansion

### Hardware Support
- Additional DAC options
- Alternative computing platforms
- Extended control interfaces

### Software Features
- Enhanced sync capabilities
- Advanced caching strategies
- Extended DJ tools
- Remote management