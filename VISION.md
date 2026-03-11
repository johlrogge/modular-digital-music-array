# VISION

## North Star

Move the music experience from phone to a dedicated "music thing" in the living room.

## Success Criteria

Professional music playback without being tied to equipment, enabling socializing during parties while maintaining quality.

## Philosophy / Values

### Test on Real Hardware First

Never assume chroot/QEMU behavior matches actual hardware. Real deployment on Raspberry Pi 5 reveals issues that virtual environments hide: service startup timing, package dependencies, network behavior, actual performance.

Rapid iteration on live hardware > perfect automation before validation.

### Prove Before Polish

Get the minimal viable implementation working on real hardware before pursuing golden images, automation, perfect UI, or advanced features. You can't optimize what doesn't work.

### Biases

- Features that enable real-world testing
- Work that compounds (immutable facts enable future evolution)
- Real hardware validation over simulated environments
- Minimum viable implementations over perfect solutions
