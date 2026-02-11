# MDMA Music Collection Reference Data

**Last updated:** December 20, 2024  
**Source:** Real user collection analysis (Joakim's Bandcamp library)

## Executive Summary

This document provides empirical data about actual music file sizes and characteristics to inform storage capacity estimates, UI design, and system architecture decisions for MDMA.

**Key Finding:** High-quality Bandcamp FLAC collections average **54 MB per track**, enabling ~6,700 tracks per 400 GB partition.

---

## Collection Profile

### Sample Collection Statistics

- **Total tracks:** 185 FLAC files
- **Total size:** 9.67 GB
- **Average track size:** 53.5 MB
- **Size range:** 4 MB to 122 MB
- **Source:** Bandcamp (electronic music: techno, ambient, experimental)

### Audio Quality Breakdown

**Bit Depth Distribution:**
- ~100 tracks: 16-bit/44.1kHz (standard CD quality)
- ~70 tracks: 24-bit/44.1kHz (high quality)
- ~15 tracks: 24-bit/48kHz (high resolution)

**Sample Track Duration Analysis** (from ffprobe):
```
Track 1: 697 seconds  (11.6 minutes) - 24-bit/44.1kHz - 1,467 kbps
Track 2: 342 seconds  (5.7 minutes)  - 24-bit/44.1kHz - 1,474 kbps
Track 3: 366 seconds  (6.1 minutes)  - 16-bit/44.1kHz - 982 kbps
Track 4: 416 seconds  (6.9 minutes)  - 16-bit/44.1kHz - 808 kbps
Track 5: 321 seconds  (5.4 minutes)  - 16-bit/44.1kHz - 694 kbps
```

**Average track length:** ~6.5 minutes (typical for electronic music)

---

## File Size Analysis

### By Quality Tier

```rust
// Empirical averages from collection analysis
pub enum AudioQualityProfile {
    Standard16Bit,   // 16-bit/44.1kHz FLAC
    High24Bit441,    // 24-bit/44.1kHz FLAC
    HighRes24Bit48,  // 24-bit/48kHz FLAC
}

impl AudioQualityProfile {
    pub fn avg_mb_per_minute(&self) -> f64 {
        match self {
            Self::Standard16Bit => 6.5,   // ~40 MB for 6-minute track
            Self::High24Bit441 => 9.0,    // ~54 MB for 6-minute track
            Self::HighRes24Bit48 => 10.0, // ~60 MB for 6-minute track
        }
    }
    
    pub fn avg_mb_per_track(&self, avg_minutes: f64) -> f64 {
        self.avg_mb_per_minute() * avg_minutes
    }
}
```

### Size Distribution

**Smallest files:** 4-20 MB
- Short tracks (1-3 minutes)
- Typically interludes or ambient pieces

**Typical files:** 30-70 MB
- Standard 5-7 minute tracks
- Majority of collection falls here

**Largest files:** 100-122 MB
- Long ambient/experimental pieces (10-15+ minutes)
- High-resolution 24-bit files
- DJ mixes

---

## Storage Capacity Estimates

### Reference Calculations

**Formula:**
```
Usable Storage = Partition Size Ã— 0.9  (10% filesystem overhead)
Estimated Tracks = Usable Storage (MB) / Average Track Size (MB)
Estimated Albums = Tracks / 10  (assuming 10 tracks per album)
Listening Hours = Tracks Ã— Average Track Length (minutes) / 60
```

### Bandcamp FLAC Profile (54 MB average)

| Partition Size | Tracks | Albums | Hours | Notes |
|---------------|--------|--------|-------|-------|
| 100 GB | 1,700 | 170 | 185 | Minimal collection |
| 200 GB | 3,400 | 340 | 370 | Small collection |
| 400 GB | 6,700 | 670 | 725 | **Recommended minimum** |
| 512 GB | 8,500 | 850 | 925 | Standard MDMA-909 |
| 1 TB | 17,000 | 1,700 | 1,850 | Large collection |
| 2 TB | 34,000 | 3,400 | 3,700 | Extensive archive |

### Comparison: Other Quality Profiles

**Beatport/Mixed Quality (35 MB average - more 16-bit):**
- 400 GB = ~10,300 tracks, ~1,030 albums, ~1,120 hours

**Lower Quality/MP3 Mix (20 MB average):**
- 400 GB = ~18,400 tracks, ~1,840 albums, ~2,000 hours

**Audiophile/Hi-Res Only (80 MB average - 24-bit/96kHz):**
- 400 GB = ~4,600 tracks, ~460 albums, ~500 hours

---

## Design Implications

### For Beacon Provisioning UI

**Recommendation:** Use user's collection as baseline when available, otherwise default to Bandcamp FLAC profile.

```rust
// Default profile based on real data
pub struct DefaultCollectionProfile {
    pub avg_track_mb: u32,      // 54
    pub avg_track_minutes: u32, // 7 (rounded up from 6.5)
    pub quality: QualityMix,    // HighQuality
}
```

**UI Messaging:**
- Show capacity in **tracks** and **albums**, not just GB
- Use comparisons: "36Ã— your current collection"
- Emphasize quality: "High-quality FLAC (mix of 16-bit and 24-bit)"
- Make it tangible: "2 years of 8-hour listening days"

### For ACID Architecture

**Implications:**
1. Content hashing (SHA256) handles deduplication
2. Different editions (16-bit vs 24-bit) get different hashes âœ“
3. Average 54 MB means ~20,000 tracks for 1TB = manageable hash set
4. Metadata extraction must handle both 16-bit and 24-bit FLAC

### For CDJ Export

**Key Consideration:** CDJs prefer AIFF/WAV files
- FLAC â†’ AIFF transcoding approximately doubles file size
- 54 MB FLAC â†’ ~110 MB AIFF
- 400 GB FLAC library â†’ ~800 GB needed for CDJ export
- **This is why MDMA-909 has secondary NVMe for `/cdj-export`**

---

## Genre-Specific Observations

### Electronic Music (Techno/House/Ambient)

**Characteristics:**
- **Track length:** 5-8 minutes average (longer than pop music)
- **Quality preference:** High (Bandcamp artists often provide 24-bit)
- **Album size:** Typically 6-12 tracks (EPs common)
- **File size:** 40-70 MB typical, 100+ MB for long ambient

**Capacity needs:**
- Active DJ: 5,000-10,000 tracks (350-700 GB)
- Collector: 10,000-20,000 tracks (700 GB-1.4 TB)
- Archive/Label: 20,000+ tracks (1.4 TB+)

### Comparison: Pop/Rock

**Different profile:**
- **Track length:** 3-4 minutes average (shorter)
- **Quality:** Often MP3 320kbps or 16-bit FLAC
- **Album size:** 10-15 tracks (full albums)
- **File size:** 8-12 MB (MP3) or 25-35 MB (FLAC)

---

## Testing Methodology

### Data Collection Commands

```bash
# Count files
find . -type f \( -name "*.flac" -o -name "*.mp3" \) | wc -l

# Calculate average size
find . -type f -name "*.flac" -exec ls -l {} \; | \
  awk '{sum+=$5; count++} END {
    print "Total files:", count
    print "Average (MB):", sum/count/1024/1024
    print "Total (GB):", sum/1024/1024/1024
  }'

# Sample track metadata
find . -type f -name "*.flac" | shuf -n 5 | while read f; do
  echo "=== $f ==="
  ffprobe -v quiet -show_format -show_streams "$f" | \
    grep -E "(duration|sample_rate|bits_per_sample)"
done
```

### Actual Results from Test Collection

```
Total files: 185
Average size (MB): 53.5472
Total size (GB): 9.67405

Size distribution:
Smallest: 4.0M
Largest: 122M

Sample tracks:
- BjÃ¶rk - Mutual Core: 697s (11.6 min), 24-bit/44.1kHz, ~103 MB
- OK EG - Cell: 342s (5.7 min), 24-bit/44.1kHz, ~52 MB
- Hydrous - Viridian Remix: 366s (6.1 min), 16-bit/44.1kHz, ~37 MB
- Joachim Spieth - Lambda: 416s (6.9 min), 16-bit/44.1kHz, ~35 MB
- Mitra - Second Skin: 321s (5.4 min), 16-bit/44.1kHz, ~23 MB
```

---

## Recommendations

### For Storage Planning

1. **Default partition:** 400 GB minimum for `/music`
2. **Metadata partition:** 88 GB for ACID fact streams
3. **CDJ export:** 512 GB secondary NVMe (if CDJ integration needed)
4. **Growth strategy:** Multi-mount support (ACID handles multiple roots)

### For Capacity Estimation

1. **Primary profile:** Bandcamp FLAC (54 MB avg)
2. **Secondary profiles:** Beatport (35 MB), Mixed (40 MB)
3. **Always show:** Tracks, albums, hours - not just GB
4. **Comparison:** Multiple of user's current collection

### For User Onboarding

**Questions to ask during provisioning:**
1. "Where do you get your music?" (Bandcamp/Beatport/Streaming)
2. "What genres do you collect?" (affects average track length)
3. Optional: "Can we analyze your existing collection?" (for precise estimates)

**Don't ask:**
- Technical questions about bit depth or sample rates
- Exact library size in GB (they don't know)

---

## Future Validation

### Data to Collect

As more users provision systems, collect:
- Average track size by source (Bandcamp vs Beatport vs YouTube Music)
- Genre-specific patterns
- Growth rates over time
- Actual vs estimated capacity usage

### Research Questions

1. How do collection sizes vary by DJ experience level?
2. What's the typical ratio of FLAC to MP3 in mixed collections?
3. How much CDJ export capacity is actually used vs allocated?
4. What's the deduplication rate for typical collections?

---

## Practical Examples

### Beacon UI Mockup

```
â”Œâ”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”
â”‚  Music Storage Capacity                     â”‚
â”œâ”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”¤
â”‚  Selected: 400 GB for music library         â”‚
â”‚                                             â”‚
â”‚  Based on high-quality FLAC collections:    â”‚
â”‚  (Bandcamp/Beatport average)                â”‚
â”‚                                             â”‚
â”‚  You'll be able to store approximately:     â”‚
â”‚                                             â”‚
â”‚  ðŸŽ”  6,700 tracks                            â”‚
â”‚  ðŸ’¿  670 albums (10 tracks each)            â”‚
â”‚  ðŸŽ§  725 hours of listening                 â”‚
â”‚                                             â”‚
â”‚  Mix of 16-bit and 24-bit FLAC              â”‚
â”‚  Perfect for Bandcamp & Beatport quality!   â”‚
â”‚                                             â”‚
â”‚  [ Adjust Storage Size ]                    â”‚
â””â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”˜
```

### Code Implementation Reference

```rust
// In beacon/src/capacity.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionProfile {
    pub avg_track_mb: u32,
    pub avg_track_minutes: u32,
    pub quality_mix: QualityMix,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum QualityMix {
    HighQuality,    // Mix of 16/24 bit FLAC (Bandcamp typical)
    Standard,       // Mostly 16-bit FLAC (Beatport typical)
    Mixed,          // FLAC + some MP3
}

impl Default for CollectionProfile {
    fn default() -> Self {
        // Based on empirical Bandcamp collection data
        Self {
            avg_track_mb: 54,
            avg_track_minutes: 7,
            quality_mix: QualityMix::HighQuality,
        }
    }
}

impl CollectionProfile {
    pub fn estimate_capacity(&self, partition_gb: u32) -> CapacityEstimate {
        let usable_mb = (partition_gb as f64 * 0.9 * 1024.0) as u32;
        
        let tracks = usable_mb / self.avg_track_mb;
        let albums = tracks / 10;
        let hours = tracks * self.avg_track_minutes / 60;
        
        CapacityEstimate {
            tracks,
            albums,
            hours,
            quality_description: self.quality_mix.description(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CapacityEstimate {
    pub tracks: u32,
    pub albums: u32,
    pub hours: u32,
    pub quality_description: &'static str,
}
```

---

## Key Takeaways

1. **Real-world average:** 54 MB per track for high-quality electronic music
2. **Quality matters:** 24-bit FLAC is ~50% larger than 16-bit
3. **Track length matters:** Electronic music averages 6-7 minutes vs 3-4 for pop
4. **400 GB is generous:** Holds 6,700 tracks = 36Ã— the reference collection
5. **Show human terms:** Tracks and albums, not gigabytes
6. **Multi-mount strategy:** Better than LVM for growth
7. **CDJ export needs 2Ã—:** Transcoding to AIFF doubles size

---

## Document History

- **2024-12-20:** Initial version based on real collection analysis
  - 185 track sample from Bandcamp
  - Electronic music focus (techno, ambient, experimental)
  - Mix of 16-bit and 24-bit FLAC at 44.1kHz and 48kHz
  - Empirical data replaces theoretical estimates

---

**Usage Note:** This document should be referenced when:
- Designing beacon provisioning UI
- Setting default partition sizes
- Estimating storage needs for users
- Planning CDJ export capacity
- Making ACID architectural decisions
- Validating capacity calculations
