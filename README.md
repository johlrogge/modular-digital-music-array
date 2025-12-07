# Modular Distributed Music Architecture (MDMA)

A distributed DJ system for Raspberry Pi 5, enabling professional music playback and mixing without being tied to equipment.

## 🎯 Project Vision

Move the music experience from phone to a dedicated "music thing" in the living room. Professional playback without being tied to equipment, enabling socializing during parties while maintaining quality.

## 🚧 Status: Milestone 1 - The Installable Player

**Current Progress:** ~15% complete

- ✅ Beacon binary cross-compiled for ARM64
- ✅ CI/CD pipeline operational
- 🔄 SD card creation (next step)
- ⏸️ Music sync (after SD works)
- ⏸️ Audio playback (after sync works)

## 🚀 Quick Start

### Building Beacon Locally

```bash
# Cross-compile for Raspberry Pi 5
just beacon-native

# Test the full CI pipeline locally
just ci-simulate
```

### CI/CD

GitHub Actions automatically builds ARM64 binaries on every push to `master`.

- [View workflow runs](https://github.com/johlrogge/modular-digital-music-array/actions)
- [Download latest beacon binary](https://github.com/johlrogge/modular-digital-music-array/actions)

## 📦 System Components

### Current

- **Beacon** - Provisioning and configuration server with web interface
  - Built in Rust
  - 4.6MB stripped binary
  - Handles SD card setup and system configuration

### Planned (Future Milestones)

- **MDMA-909** - Main processing unit with full DJ capabilities
- **MDMA-303** - Satellite playback nodes for multi-room audio
- **MDMA-101** - Browser/controller with jog wheel interface

## 🎵 Why MDMA?

The acronym is a playful nod to electronic music culture - techno and house DJs will appreciate the humor. The system helps you maintain that party vibe without being tied to the decks!

**Full name:** Modular Distributed Music Architecture

## 🛠️ Technology Stack

- **Language:** Rust (type-safe, cross-platform)
- **OS:** Void Linux on Raspberry Pi 5
- **Storage:** NVMe drives via M.2 HAT
- **Network:** mDNS for service discovery
- **Build:** Justfile-based CI/CD (test locally, run in GitHub Actions)

## 📚 Documentation

- [Build Pipeline Docs](docs/build-pipeline.md)
- [Local CI Testing](docs/LOCAL_CI_TESTING.md)
- [Scope & Milestones](docs/milestone_1_2_scope_reduction.md)

## 🤝 Contributing

This is currently a personal project in active development. Contributions, ideas, and feedback are welcome once Milestone 1 is complete!

## 📄 License

MIT License - See [LICENSE](LICENSE) for details

## 🙏 Acknowledgments

Built with inspiration from:
- Pioneer CDJ series (hardware interface evolution)
- Traktor/Serato (software DJ platforms)
- The global techno and house music community

---

**Next milestone:** Boot a Raspberry Pi from SD card and see the beacon interface at `http://welcome-to-mdma.local` 🚀
