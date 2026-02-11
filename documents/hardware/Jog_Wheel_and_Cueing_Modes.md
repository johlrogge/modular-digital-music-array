# Jog Wheel and Cueing Modes

## Jog Wheel Modes

### Basic Modes
1. **Vinyl Mode (CDJ/Classic)**
   - Touch-sensitive top plate simulates vinyl control
   - Outer ring for pitch bend/nudge
   - Provides traditional turntable-style manipulation
   - Used for scratching and precise cueing
   - Rotation speed directly correlates to playback speed

2. **Nudge Mode (CDJ)**
   - Entire wheel acts as pitch bend
   - No touch sensitivity required
   - Used for subtle tempo adjustments
   - Good for beatmatching without scratching
   - Less precise than Vinyl mode for cueing

3. **Search Mode**
   - Fast track navigation while paused
   - Usually activated by dedicated button
   - Often increases sensitivity for quick scanning
   - Some systems vary speed based on rotation speed
   - Can include audio preview (needle search)

### Advanced Modes

4. **Static Mode**
   - Fixed resistance regardless of playback state
   - Consistent feel during scratching
   - Preferred by some scratch DJs
   - Available on high-end controllers
   - Often customizable resistance

5. **Browse Mode**
   - Library/playlist navigation
   - Often combined with push-to-select
   - Can include preview features
   - Sometimes has acceleration for large libraries
   - May integrate with waveform zooming

### Special Features

6. **Dual-Zone Operation**
   - Inner zone for scratching/precise control
   - Outer zone for pitch bend/track search
   - Can be independently configured
   - Sometimes has separate sensitivity settings
   - May have different modes per zone

7. **Tension Adjust**
   - Software-adjustable mechanical resistance
   - Can simulate different vinyl weights
   - Customizable start/stop times
   - May include haptic feedback
   - Available on premium hardware

## Preview and Cueing Modes

### Basic Preview Modes

1. **Standard Cue**
   - Basic headphone preview
   - Full track monitoring
   - Independent volume control
   - Can be split or master/cue mix
   - Standard on all DJ systems

2. **Split Cue**
   - Mono master in one ear
   - Mono cue in other ear
   - Helps with beatmatching
   - Clear separation of signals
   - Popular in club environments

### Advanced Preview Features

3. **Beat Jump Preview**
   - Preview X beats ahead
   - Commonly 16, 32, or 64 beats
   - Helps plan transitions
   - Can be quantized to phrases
   - Available on Pioneer CDJ-3000

4. **Phrase Preview**
   - Jump to next musical phrase
   - Usually 8 or 16 bar sections
   - Intelligent analysis required
   - Good for structure overview
   - Found on high-end hardware

### Smart Preview Features

5. **Smart Cue**
   - AI-assisted next point suggestion
   - Based on track analysis
   - Shows potential mix points
   - Energy level matching
   - Emerging technology feature

6. **Loop Preview**
   - Preview with auto-loop engaged
   - Set loop length in beats
   - Good for testing mix compatibility
   - Can be quantized to grid
   - Common on modern hardware

7. **Multi-Point Preview**
   - Multiple cue points stored
   - Quick comparison of sections
   - Often color-coded
   - Can include loop points
   - Standard on professional gear

### Preview Mixing Modes

8. **Master Mix Preview**
   - Hear future mix in headphones
   - Test compatibility before live
   - Adjustable mix ratio
   - Can include effects preview
   - Advanced feature on pro gear

9. **Parallel Preview**
   - Preview next track while current plays
   - Independent tempo control
   - Beatgrid alignment tools
   - Phase meters included
   - Found on high-end mixers

## Implementation Considerations

### Jog Wheel Requirements
- High-resolution encoder (600+ PPR)
- Touch sensitivity for vinyl mode
- Consistent latency handling
- Acceleration curve mapping
- Multiple mode support
- State management for mode switching

### Preview System Requirements
- Low-latency audio routing
- Independent sample rate handling
- Multiple audio device support
- Buffer management for previews
- Clear UI state indication
- Flexible routing matrix
- Efficient memory management for look-ahead

### Software Architecture
- Mode state management
- Latency compensation
- Touch event handling
- Audio buffer management
- Preview routing system
- Multiple output streams
- Thread-safe operation