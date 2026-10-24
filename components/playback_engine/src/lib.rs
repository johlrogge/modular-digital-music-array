pub mod audio_config;
mod error;
mod mixer;
mod playhead;
mod track;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
};

pub use audio_config::{load_audio_config, save_audio_config, AudioOutputConfig};
pub use audio_decoder::{AudioSource, DecoderError, Source};
pub use audio_output::pipewire_devices::{self, AudioSink};
use audio_output::pipewire_output::{OutputShared, DEFAULT_CHANNELS};
pub use audio_output::{AudioOutputError, PipewireOutput};
pub use audio_types::{AudioSegment, DecodedSegment, SegmentIndex, SEGMENT_SIZE};
pub use error::PlaybackError;
use mixer::{Mixer, MixerSnapshot, MixerStatus, Slot};
use playback_primitives::Db;
pub use playback_primitives::Volume;
use playhead::{Playhead, Segment};
use ringbuf::{HeapConsumer, HeapRb};
use tracing::info;
pub use track::Track;

use audio_output::pipewire_devices::list_sinks;

/// Track ring buffer size in f32 samples. Stereo: divide by 2 for frames;
/// divide by sample_rate for seconds of pre-decoded audio.
/// 131_072 = ~1.49 s at 44.1 kHz stereo, ~0.34 s at 192 kHz stereo.
const TRACK_BUFFER_SAMPLES: usize = 131_072;

/// Mixer ring buffer size in f32 samples.
/// 262_144 = ~2.97 s at 44.1 kHz stereo, ~0.68 s at 192 kHz stereo.
const MIXER_BUFFER_SAMPLES: usize = 262_144;

/// Engine-assigned track identifier (non-zero; 0 means "none" in the mixer).
pub type TrackId = u64;

/// A track owned by the engine plus what is needed to account its position.
struct TrackEntry {
    track: Track,
    path: PathBuf,
    /// Output sample rate (frames per second) the track is decoded to.
    rate: u32,
}

/// Where a preloaded track's audio currently is.
enum PreloadSlot {
    /// Registered with the mixer as its next track.
    Registered,
    /// Decoded at a rate other than the output's, so not registered with the
    /// mixer; the consumer waits here until the track is loaded
    /// (drain-then-reopen).
    Parked(HeapConsumer<f32>),
}

/// The track queued to follow the audible one.
struct Preload {
    id: TrackId,
    slot: PreloadSlot,
}

impl Preload {
    fn is_parked(&self) -> bool {
        matches!(self.slot, PreloadSlot::Parked(_))
    }

    /// Take the parked consumer (leaving the slot `Registered`, as the caller
    /// is about to register it). `None` if it was not parked.
    fn take_parked(&mut self) -> Option<HeapConsumer<f32>> {
        match std::mem::replace(&mut self.slot, PreloadSlot::Registered) {
            PreloadSlot::Parked(consumer) => Some(consumer),
            PreloadSlot::Registered => None,
        }
    }
}

/// A track opened for decoding together with the consumer of its ring.
struct OpenedTrack {
    track: Track,
    consumer: HeapConsumer<f32>,
    /// Channel count of the source (interleaving of its samples).
    channels: u32,
    source_rate: u32,
    /// Rate the track is decoded to (the output rate it needs).
    rate: u32,
}

/// Open `path` and start its (stopped) decoder, decoding at the source's
/// native rate (`fallback_rate` only if the source reports none).
fn open_track(path: &Path, fallback_rate: u32) -> Result<OpenedTrack, PlaybackError> {
    let source = AudioSource::new(path)?;
    let source_rate = source.sample_rate();
    let channels = u32::from(source.audio_channels()).max(1);
    let rate = select_target_rate(source_rate, fallback_rate);
    let (producer, consumer) = HeapRb::<f32>::new(TRACK_BUFFER_SAMPLES).split();
    // The decoder resamples source_rate -> rate if they differ.
    let track = Track::new(source, producer, source_rate, rate)?;
    Ok(OpenedTrack {
        track,
        consumer,
        channels,
        source_rate,
        rate,
    })
}

/// The mixer slot for `track`, whose ring `consumer` is given.
fn make_slot(id: TrackId, consumer: HeapConsumer<f32>, track: &Track) -> Slot {
    Slot {
        id,
        consumer,
        input_complete: track.input_complete(),
    }
}

/// One consistent reading of the engine's observable state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineStatus {
    /// The track being heard right now (switches at the exact output sample
    /// of a gapless boundary).
    pub audible: Option<TrackId>,
    /// The track that will play next: the one the mixer already promoted but
    /// that is not audible yet if any (committed, can no longer be
    /// replaced), otherwise the registered or parked preload.
    pub next: Option<TrackId>,
    /// The audible track was fully played out and nothing follows it
    /// gaplessly; see [`PlaybackEngine::status`].
    pub finished: bool,
    /// Audible position (from samples the output consumed), `None` if
    /// nothing is loaded.
    pub position_ms: Option<u64>,
    /// Duration of the audible track; `Some(0)` if unknown, `None` if
    /// nothing is loaded.
    pub duration_ms: Option<u64>,
    /// The output is emitting silence instead of consuming the mixer ring.
    pub paused: bool,
}

/// What a reader saw in one go: playhead (with mixer promotions folded in and
/// the audible switch applied), the consumed counter used for that, and the
/// mixer snapshot used for that.
struct Observed {
    playhead: Playhead,
    consumed: u64,
    mixer: MixerSnapshot,
}

pub struct PlaybackEngine {
    /// Every live track (audible, pending, preload, and ones the mixer still
    /// references), keyed by id. Pruned by [`PlaybackEngine::reconcile`]. The
    /// engine is the single owner of track lifetime: see
    /// [`PlaybackEngine::is_live`].
    tracks: HashMap<TrackId, TrackEntry>,
    playhead: Playhead,
    preload: Option<Preload>,
    next_track_id: TrackId,
    mixer_status: Arc<MixerStatus>,
    audio_output: Option<PipewireOutput>,
    mixer_consumer: Option<HeapConsumer<f32>>,
    current_sample_rate: Option<u32>,
    command_sender: mpsc::Sender<MixerCommand>,
    mix_task: Option<std::thread::JoinHandle<()>>,
    /// Engine-owned state shared with the output callback; survives output
    /// re-creation. `paused`: while set the callback emits silence and keeps
    /// the mixer ring untouched (but still services flushes). `consumed_total`
    /// drives position accounting; `gain` is the linear volume.
    shared: OutputShared,
    mix_thread_running: Arc<AtomicBool>,
    config_path: PathBuf,
    audio_config: AudioOutputConfig,
}

enum MixerCommand {
    /// Replace the current track (drops any pending next).
    RegisterTrack { slot: Slot },
    /// Preload the track that follows the current one gaplessly.
    RegisterNext { slot: Slot },
    /// Drop the pending next if it still has this id (ignored if already promoted).
    ClearNext { id: u64 },
    /// Drop current and next without replacing them.
    /// After this, the mixer stops pulling samples until a RegisterTrack arrives.
    ///
    /// The mix thread clears both slots and then signals `done`, so
    /// the sender knows no further track samples will reach the mixer ring.
    ClearTrack { done: mpsc::SyncSender<()> },
    /// Drop only the current track; the pending next is kept (and not
    /// promoted). Acked like `ClearTrack`.
    DetachCurrent { done: mpsc::SyncSender<()> },
    /// Make the pending next (if it has this id) the current track. Replies
    /// with the mixer's `produced_total` at that instant, or `None` if the id
    /// is not the pending next.
    PromoteNext {
        id: u64,
        done: mpsc::SyncSender<Option<u64>>,
    },
}

/// One iteration of the mix thread: apply pending commands, then mix.
/// Non-blocking (the mixer only moves what fits in the output ring).
fn mix_step(mixer: &mut Mixer, temp_buffer: &mut [f32], commands: &mpsc::Receiver<MixerCommand>) {
    while let Ok(cmd) = commands.try_recv() {
        match cmd {
            MixerCommand::RegisterTrack { slot } => {
                tracing::debug!("MIX THREAD: Registering new track");
                mixer.register_current(slot);
            }
            MixerCommand::RegisterNext { slot } => {
                tracing::debug!("MIX THREAD: Registering next track");
                mixer.register_next(slot);
            }
            MixerCommand::ClearNext { id } => {
                mixer.clear_next(id);
            }
            MixerCommand::ClearTrack { done } => {
                tracing::debug!("MIX THREAD: Clearing track slots");
                mixer.clear_all();
                let _ = done.send(());
            }
            MixerCommand::DetachCurrent { done } => {
                tracing::debug!("MIX THREAD: Detaching current track");
                mixer.clear_current();
                let _ = done.send(());
            }
            MixerCommand::PromoteNext { id, done } => {
                tracing::info!("MIX THREAD: Promoting preloaded track {id}");
                let _ = done.send(mixer.promote_next(id));
            }
        }
    }
    let l = temp_buffer.len();
    if let Err(e) = mixer.mix(temp_buffer, l) {
        tracing::error!("MIX THREAD: Error mixing: {}", e);
    }
}

/// Mix-thread body: step, sleep, until `running` is cleared.
fn run_mix_loop(
    mut mixer: Mixer,
    commands: mpsc::Receiver<MixerCommand>,
    running: Arc<AtomicBool>,
) {
    let mut temp_buffer = vec![0.0f32; 1920 * 2];
    while running.load(Ordering::Acquire) {
        mix_step(&mut mixer, &mut temp_buffer, &commands);
        std::thread::sleep(std::time::Duration::from_micros(500));
    }
}

/// How long to wait for the mix thread to acknowledge a command.
const MIX_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(50);

/// Compute the PipeWire output rate for a track.
///
/// Passes the source's native sample rate through directly so no resampling
/// is required. Falls back to `fallback_rate` only when `source_rate` is 0
/// (defensive; Symphonia should never produce this, but we guard against it).
/// If PipeWire rejects an unusual rate that is a real error surfaced upward —
/// we do not silently mask it with a clamp.
pub fn select_target_rate(source_rate: u32, fallback_rate: u32) -> u32 {
    if source_rate == 0 {
        fallback_rate
    } else {
        source_rate
    }
}

impl PlaybackEngine {
    pub fn new(config_path: PathBuf) -> Result<Self, PlaybackError> {
        let audio_config = load_audio_config(&config_path)?;

        let (command_sender, command_receiver) = std::sync::mpsc::channel();

        // Create ringbuffer for mixer output.
        let mixer_rb = HeapRb::<f32>::new(MIXER_BUFFER_SAMPLES);
        let (mixer_producer, mixer_consumer) = mixer_rb.split();

        let mixer = Mixer::new(mixer_producer);
        let mixer_status = mixer.status();

        let mix_thread_running = Arc::new(AtomicBool::new(true));
        let running = Arc::clone(&mix_thread_running);

        // Start the mix thread with command receiver
        let mix_task = std::thread::spawn(move || {
            tracing::info!("MIX THREAD: Started, will process audio");
            run_mix_loop(mixer, command_receiver, running);
            tracing::info!("MIX THREAD: Exiting cleanly");
        });

        Ok(Self {
            tracks: HashMap::new(),
            playhead: Playhead::default(),
            preload: None,
            next_track_id: 1,
            mixer_status,
            audio_output: None,
            mixer_consumer: Some(mixer_consumer),
            current_sample_rate: None,
            command_sender,
            mix_task: Some(mix_task),
            shared: OutputShared::new(Arc::new(AtomicBool::new(true))),
            mix_thread_running,
            config_path,
            audio_config,
        })
    }

    /// Signal the mix thread to stop and wait for it to join.
    pub fn shutdown(&mut self) {
        self.mix_thread_running.store(false, Ordering::Release);
        if let Some(handle) = self.mix_task.take() {
            let _ = handle.join();
        }
    }

    /// Enumerate available audio output sinks via pw-dump.
    pub fn list_outputs(&self) -> Result<Vec<AudioSink>, PlaybackError> {
        list_sinks().map_err(PlaybackError::AudioOutput)
    }

    /// Return the current audio output configuration.
    pub fn get_output(&self) -> &AudioOutputConfig {
        &self.audio_config
    }

    /// Select a named audio output device.
    ///
    /// Looks up the device in the live sink list to obtain its maximum sample rate,
    /// persists the new config, then hot-swaps the PipeWire output: if one is
    /// currently active its consumer is recovered via `shutdown()` and a new output
    /// is created immediately on the new device.
    pub fn set_output(&mut self, device_name: String) -> Result<AudioOutputConfig, PlaybackError> {
        let sinks = list_sinks()?;
        let sink = sinks
            .iter()
            .find(|s| s.name == device_name)
            .ok_or_else(|| {
                PlaybackError::AudioOutput(audio_output::AudioOutputError::AudioDevice(format!(
                    "Audio device not found: {device_name}"
                )))
            })?;

        let new_config = AudioOutputConfig {
            device_name: Some(device_name.clone()),
            sample_rate: sink.max_sample_rate,
        };

        save_audio_config(&self.config_path, &new_config)?;

        if let Some(current_rate) = self.current_sample_rate {
            if current_rate != sink.max_sample_rate {
                tracing::info!(
                    "Sample rate change from {}Hz to {}Hz takes effect on next track load",
                    current_rate,
                    sink.max_sample_rate
                );
            }
        }

        let recovered_consumer = if let Some(old_output) = self.audio_output.take() {
            old_output.shutdown()
        } else {
            None
        };

        self.audio_config = new_config.clone();
        // The old output (if any) is gone; until a new one exists there is
        // no output rate. Preload's registered-vs-parked decision reads this.
        self.current_sample_rate = None;

        if let Some(consumer) = recovered_consumer {
            tracing::info!(
                "Hot-swapping PipeWire output to device {:?} at {}Hz",
                new_config.device_name,
                new_config.sample_rate
            );
            let audio_output = PipewireOutput::new(
                consumer,
                self.shared.clone(),
                new_config.sample_rate,
                Some(device_name.as_str()),
            )
            .map_err(|e| {
                PlaybackError::AudioOutput(audio_output::AudioOutputError::AudioDevice(format!(
                    "PipeWire error: {}",
                    e
                )))
            })?;
            self.audio_output = Some(audio_output);
            self.current_sample_rate = Some(new_config.sample_rate);
        }

        Ok(new_config)
    }

    fn alloc_track_id(&mut self) -> TrackId {
        let id = self.next_track_id;
        self.next_track_id += 1;
        id
    }

    /// Make sure an output exists at `target_rate`, re-creating it (and
    /// recovering the mixer consumer from the old one) when the rate differs.
    fn ensure_output(&mut self, target_rate: u32) -> Result<(), PlaybackError> {
        let need_new_output = match self.current_sample_rate {
            None => true,
            Some(current) => current != target_rate,
        };
        if !need_new_output {
            return Ok(());
        }
        let target_device = self.audio_config.device_name.clone();

        // Recover the consumer from the old output (if any) so we can
        // hand it to the new one without touching the mix thread.
        if let Some(old_output) = self.audio_output.take() {
            if let Some(consumer) = old_output.shutdown() {
                self.mixer_consumer = Some(consumer);
            } else {
                tracing::warn!(
                    "ensure_output: failed to recover consumer from old PipeWire output"
                );
            }
        }

        if self.mixer_consumer.is_none() {
            tracing::error!(
                "ensure_output: no mixer consumer available and no audio output — audio will be silent"
            );
        }
        if let Some(consumer) = self.mixer_consumer.take() {
            info!(
                "Creating PipeWire output at {}Hz (device: {:?})",
                target_rate, target_device
            );
            let audio_output = PipewireOutput::new(
                consumer,
                self.shared.clone(),
                target_rate,
                target_device.as_deref(),
            )
            .map_err(|e| {
                PlaybackError::AudioOutput(audio_output::AudioOutputError::AudioDevice(format!(
                    "PipeWire error: {}",
                    e
                )))
            })?;
            self.audio_output = Some(audio_output);
            self.current_sample_rate = Some(target_rate);
        }
        Ok(())
    }

    /// Observe the engine as of now: the consumed counter is read first, then
    /// one mixer snapshot; promotions in it are folded into a copy of the
    /// playhead and the audible switch applied against the consumed count.
    ///
    /// Reading `consumed_total` before the mixer snapshot matters: samples
    /// past a promotion boundary are pushed to the output ring only after the
    /// promotion record was published, so a boundary the output has already
    /// consumed is always visible in the snapshot taken afterwards.
    fn observe(&self) -> Observed {
        let consumed = self.shared.consumed_total.load(Ordering::Acquire);
        let mixer = self.mixer_status.snapshot();
        let mut ph = self.playhead.clone();
        if let Some(p) = mixer.last_promotion {
            if p.promoted_id != ph.last_promotion_id() {
                ph.note_promotion_seen(p.promoted_id);
                if let Some(e) = self.tracks.get(&p.promoted_id) {
                    ph.schedule(Segment {
                        id: p.promoted_id,
                        start_sample: p.boundary_sample,
                        base_ms: 0,
                        output_channels: DEFAULT_CHANNELS,
                        rate: e.rate,
                    });
                }
            }
        }
        ph.resolve(consumed);
        Observed {
            playhead: ph,
            consumed,
            mixer,
        }
    }

    /// Start the playhead on a new segment of the output stream, recording
    /// the mixer's current promotion as already seen (see
    /// [`Playhead::start`]).
    fn start_playhead(&mut self, id: TrackId, start_sample: u64, base_ms: u64, rate: u32) {
        let seen = self
            .mixer_status
            .last_promotion()
            .map_or(0, |p| p.promoted_id);
        self.playhead.start(
            Segment {
                id,
                start_sample,
                base_ms,
                output_channels: DEFAULT_CHANNELS,
                rate,
            },
            seen,
        );
    }

    /// The track that plays after the audible one: the pending (promoted,
    /// not yet audible) track if there is one, else the registered/parked
    /// preload.
    fn next_of(&self, obs: &Observed) -> Option<TrackId> {
        obs.playhead.pending_id().or_else(|| {
            self.preload
                .as_ref()
                .map(|p| p.id)
                .filter(|id| !obs.playhead.references(*id))
        })
    }

    /// Fold mixer state into the engine: observe gapless promotions, switch
    /// the audible track at its boundary, and drop tracks nothing references
    /// any more. Call periodically (e.g. from the Loaded poll); every mutating
    /// method calls it first.
    ///
    /// A track is only dropped once neither the playhead, the preload slot
    /// nor the mixer (current/next) references it, so a `ClearNext` that lost
    /// the race against a promotion never kills the track now playing.
    pub fn reconcile(&mut self) {
        let obs = self.observe();
        self.playhead = obs.playhead;
        if self
            .preload
            .as_ref()
            .is_some_and(|p| self.playhead.references(p.id))
        {
            // Promoted: it is the playing track now, not a preload.
            self.preload = None;
        }
        let preload_id = self.preload.as_ref().map(|p| p.id);
        let ph = &self.playhead;
        self.tracks.retain(|id, _| {
            ph.references(*id) || Some(*id) == preload_id || obs.mixer.references(*id)
        });
    }

    /// Drop every track and everything buffered (mixer slots, mixer ring,
    /// in-flight output). Includes any preload.
    fn drop_everything(&mut self) -> Result<(), PlaybackError> {
        self.tracks.clear();
        self.preload = None;
        self.playhead.clear();
        // Note: this blocks the calling thread for the ClearTrack ack (<= 50 ms)
        // and the output flush (ring flush <= 100 ms plus PipeWire flush
        // <= 100 ms, normally ~1 quantum each). Acceptable
        // because the mdma-audio server handles requests sequentially.
        self.discard_buffered_audio()
    }

    /// Open `path`, (re)create the output if its rate differs, and register
    /// the new track as the mixer's current track. Drops all previous tracks
    /// and any preload first. With `seek_to` (interleaved sample index at the
    /// source rate) the track starts there.
    fn start_fresh(&mut self, path: &Path, seek_to: Option<usize>) -> Result<(), PlaybackError> {
        self.drop_everything()?;

        let OpenedTrack {
            mut track,
            consumer,
            channels,
            source_rate,
            rate,
        } = open_track(path, self.audio_config.sample_rate)?;

        self.ensure_output(rate)?;

        let mut base_ms = 0;
        if let Some(position) = seek_to {
            track.seek(position)?;
            base_ms = position as u64 / u64::from(channels) * 1000 / u64::from(source_rate.max(1));
        }
        tracing::info!("Track is ready for playback");

        let id = self.alloc_track_id();
        let slot = make_slot(id, consumer, &track);
        // The mixer is cleared and idle, so `produced_total` is exactly where
        // this track's audio will start in the output stream.
        let start_sample = self.mixer_status.produced_total();
        self.tracks.insert(
            id,
            TrackEntry {
                track,
                path: path.to_path_buf(),
                rate,
            },
        );
        self.start_playhead(id, start_sample, base_ms, rate);
        self.command_sender
            .send(MixerCommand::RegisterTrack { slot })
            .map_err(|_| PlaybackError::TaskCancelled)?;
        Ok(())
    }

    /// Load `path` as the current track, immediately (any buffered audio and
    /// any preload are cut). If `path` is the preloaded track, its already
    /// filled buffer is reused (instant skip). The track starts paused;
    /// `play()` unpauses.
    pub async fn load_track(&mut self, path: &Path) -> Result<(), PlaybackError> {
        tracing::info!("Starting track load");
        self.reconcile();

        // A newly loaded track starts stopped; `play()` unpauses.
        self.shared.paused.store(true, Ordering::Release);
        if let Some(id) = self.audible_track_id() {
            if let Some(e) = self.tracks.get_mut(&id) {
                tracing::info!("Unloading existing track");
                e.track.stop();
            }
        }

        if self.try_cut_to_preload(path)? {
            tracing::info!("Loaded preloaded track {:?}", path);
            return Ok(());
        }

        self.start_fresh(path, None)?;
        tracing::info!("Loaded track from {:?}", path);
        Ok(())
    }

    /// Make the preloaded track current if `path` is that track. `Ok(false)`
    /// means "not applicable, load normally".
    fn try_cut_to_preload(&mut self, path: &Path) -> Result<bool, PlaybackError> {
        let Some(pre) = self.preload.as_ref() else {
            return Ok(false);
        };
        let id = pre.id;
        let parked = pre.is_parked();
        let Some(entry) = self.tracks.get(&id) else {
            return Ok(false);
        };
        if entry.path != path {
            return Ok(false);
        }
        let rate = entry.rate;

        let start_sample = if parked {
            // Rate differs from the output: cut everything, re-open the
            // output at the track's rate, register its filled consumer.
            self.discard_buffered_audio()?;
            self.ensure_output(rate)?;
            let Some(consumer) = self.preload.as_mut().and_then(Preload::take_parked) else {
                return Ok(false);
            };
            let Some(entry) = self.tracks.get(&id) else {
                return Ok(false);
            };
            let slot = make_slot(id, consumer, &entry.track);
            let start = self.mixer_status.produced_total();
            self.command_sender
                .send(MixerCommand::RegisterTrack { slot })
                .map_err(|_| PlaybackError::TaskCancelled)?;
            start
        } else {
            // Registered as the mixer's next: cut the current track and its
            // buffered audio (detach -> ring flush ack -> PipeWire flush ack),
            // then promote next. Nothing is produced between the detach and
            // the promote, and both flushes are complete before it, so they
            // cannot eat the new track's audio.
            self.detach_current_audio()?;
            let (tx, rx) = mpsc::sync_channel(1);
            self.command_sender
                .send(MixerCommand::PromoteNext { id, done: tx })
                .map_err(|_| PlaybackError::TaskCancelled)?;
            match rx.recv_timeout(MIX_ACK_TIMEOUT) {
                Ok(Some(start)) => start,
                _ => {
                    // Already promoted (it is playing) or mix thread stalled:
                    // fall back to a normal load.
                    tracing::warn!("load_track: preload not promotable, loading normally");
                    return Ok(false);
                }
            }
        };

        self.preload = None;
        self.start_playhead(id, start_sample, 0, rate);
        self.reconcile();
        Ok(true)
    }

    /// Decode `path` ahead of time so it can follow the audible track
    /// gaplessly. It is decoded at its native rate and fills its own ring
    /// (~1.5 s). If that rate equals the output's it is registered with the
    /// mixer (sample-accurate switch); otherwise it is *parked* and the
    /// caller falls back to drain-then-`load_track`. Replaces any existing
    /// preload. Requires a loaded track.
    pub fn preload_track(&mut self, path: &Path) -> Result<TrackId, PlaybackError> {
        self.reconcile();
        if self.playhead.audible().is_none() {
            return Err(PlaybackError::NoTrackLoaded);
        }
        self.clear_preload();

        let OpenedTrack {
            mut track,
            consumer,
            rate,
            ..
        } = open_track(path, self.audio_config.sample_rate)?;
        // Fill the ring now; the decoder blocks when it is full. Independent
        // of the pause flag.
        track.play();

        let id = self.alloc_track_id();
        self.tracks.insert(
            id,
            TrackEntry {
                track,
                path: path.to_path_buf(),
                rate,
            },
        );

        let slot = if self.current_sample_rate == Some(rate) {
            let slot = make_slot(id, consumer, &self.tracks[&id].track);
            self.command_sender
                .send(MixerCommand::RegisterNext { slot })
                .map_err(|_| PlaybackError::TaskCancelled)?;
            PreloadSlot::Registered
        } else {
            tracing::info!(
                "Preload at {}Hz differs from output {:?}Hz: parked",
                rate,
                self.current_sample_rate
            );
            PreloadSlot::Parked(consumer)
        };
        self.preload = Some(Preload { id, slot });
        Ok(id)
    }

    /// Drop the preload, if any. If the mixer promoted it first it is the
    /// playing track and is left alone (see [`PlaybackEngine::reconcile`]).
    ///
    /// Known, accepted residual race: if the mix thread has not yet processed
    /// `RegisterNext(B)` when this reconciles, B is in no keep-set and its
    /// decoder is dropped. If the mixer then promotes B before it handles
    /// `ClearNext`, B plays from a dead decoder. This needs a stall of the
    /// 0.5 ms mix loop plus a queue edit at the exact track end. A structural
    /// fix would keep ids with in-flight commands alive until acked.
    pub fn clear_preload(&mut self) {
        self.reconcile();
        if let Some(pre) = self.preload.take() {
            if !pre.is_parked() {
                let _ = self
                    .command_sender
                    .send(MixerCommand::ClearNext { id: pre.id });
            }
            // The Track itself is dropped by `reconcile` once the mixer no
            // longer references it.
            self.reconcile();
        }
    }

    /// Set the output gain. Applied (ramped) in the output callback.
    pub fn set_volume(&mut self, volume: Volume) -> Result<(), PlaybackError> {
        tracing::debug!("Setting volume to {}dB", volume.raw());
        self.shared
            .gain
            .store(volume.to_linear().to_bits(), Ordering::Release);
        Ok(())
    }

    pub fn play(&mut self) -> Result<(), PlaybackError> {
        let id = self.audible_track_id().ok_or_else(|| {
            tracing::error!("No track loaded");
            PlaybackError::NoTrackLoaded
        })?;
        if let Some(e) = self.tracks.get_mut(&id) {
            tracing::debug!("Setting track to playing state");
            e.track.play();
        }
        self.shared.paused.store(false, Ordering::Release);
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), PlaybackError> {
        let id = self.audible_track_id().ok_or_else(|| {
            tracing::error!("No track loaded");
            PlaybackError::NoTrackLoaded
        })?;
        if let Some(e) = self.tracks.get_mut(&id) {
            tracing::info!("Stopping track");
            e.track.stop();
        }
        self.shared.paused.store(true, Ordering::Release);
        Ok(())
    }

    pub fn unload_track(&mut self) -> Result<(), PlaybackError> {
        if self.tracks.is_empty() {
            tracing::info!("No track to unload");
            return Ok(());
        }
        self.shared.paused.store(true, Ordering::Release);
        self.drop_everything()?;
        tracing::info!("Unloaded track");
        Ok(())
    }

    /// Whether the output callback is currently emitting silence instead of
    /// consuming the mixer ring.
    pub fn is_paused(&self) -> bool {
        self.shared.paused.load(Ordering::Acquire)
    }

    /// Clear the mixer's track consumer so no old samples are produced, wait
    /// for the mix thread to acknowledge (so nothing more can reach the mixer
    /// ring), then flush the mixer output ring and PipeWire's in-flight
    /// buffers, returning only once both flushes completed (so a track
    /// registered/promoted afterwards cannot lose its first quantum to a late
    /// flush). The output callback keeps running while paused, so the flush
    /// is acknowledged promptly (normally ~1 quantum, bounded at 100 ms each).
    /// This reduces skip latency from ~3 s (full ring drain) to ~50 ms.
    ///
    /// ClearTrack is sent even without an output; only the flush needs one.
    fn discard_buffered_audio(&self) -> Result<(), PlaybackError> {
        self.cut_and_flush(|done| MixerCommand::ClearTrack { done })
    }

    /// Like [`Self::discard_buffered_audio`] but keeps the pending next.
    fn detach_current_audio(&self) -> Result<(), PlaybackError> {
        self.cut_and_flush(|done| MixerCommand::DetachCurrent { done })
    }

    fn cut_and_flush(
        &self,
        make: impl FnOnce(mpsc::SyncSender<()>) -> MixerCommand,
    ) -> Result<(), PlaybackError> {
        let (done_tx, done_rx) = mpsc::sync_channel::<()>(1);
        self.command_sender
            .send(make(done_tx))
            .map_err(|_| PlaybackError::TaskCancelled)?;
        if done_rx.recv_timeout(MIX_ACK_TIMEOUT).is_err() {
            tracing::warn!(
                "cut_and_flush: mix thread did not acknowledge within {:?}",
                MIX_ACK_TIMEOUT
            );
        }
        if let Some(ref output) = self.audio_output {
            output.flush();
        }
        Ok(())
    }

    /// Id of the track currently being heard (switches at the exact output
    /// sample of a gapless boundary), or `None` if nothing is loaded.
    pub fn audible_track_id(&self) -> Option<TrackId> {
        self.observe().playhead.audible().map(|s| s.id)
    }

    /// Id of the track that plays after the audible one: the promoted (mixer
    /// switched to it, boundary not yet heard) track if any, else the
    /// registered or parked preload. See [`EngineStatus::next`].
    pub fn next_track_id(&self) -> Option<TrackId> {
        self.next_of(&self.observe())
    }

    /// Whether the engine still owns `id` (audible, promoted, preloaded, or
    /// still referenced by the mixer). The engine is the single owner of track
    /// lifetime: callers keeping per-track data must drop it exactly when this
    /// turns false, never based on `audible`/`next` alone (a track is live
    /// but neither of those for a moment around a commit).
    pub fn is_live(&self, id: TrackId) -> bool {
        self.tracks.contains_key(&id)
    }

    /// Everything about the engine's current state from ONE observation
    /// (one consumed-counter read and one mixer snapshot), so the fields are
    /// mutually consistent. Call [`PlaybackEngine::reconcile`] first to also
    /// drop dead tracks.
    ///
    /// `finished` is true only when the audible track has been fully *played
    /// out* and nothing follows it gaplessly:
    /// - the audible track is `Finished`: the decoder hit EOF and every
    ///   sample has been handed to the mixer (the track ring is empty),
    /// - no promotion is awaiting its boundary and no next is registered with
    ///   the mixer, and
    /// - the audio output is drained: the mixer ring and the in-flight
    ///   PipeWire quantum have been played.
    ///
    /// A *parked* preload is not registered, so it does not hold this back:
    /// the caller then `load_track`s it (drain-then-reopen).
    ///
    /// Note: the drained signal is produced by the output's process
    /// callback, so a paused stream freezes it; while frozen
    /// the result keeps its last value. With no output (PipeWire failed to
    /// start, already logged as an error) the output is treated as drained.
    ///
    /// `position_ms` is derived from samples the output has consumed, so it
    /// does not run ahead by the buffer and freezes while paused.
    pub fn status(&self) -> EngineStatus {
        let obs = self.observe();
        let audible_segment = obs.playhead.audible();
        let entry = audible_segment.and_then(|s| self.tracks.get(&s.id));
        let track_finished = entry.is_some_and(|e| e.track.is_finished());
        let drained = self.audio_output.as_ref().map(|o| o.is_drained());
        EngineStatus {
            audible: audible_segment.map(|s| s.id),
            next: self.next_of(&obs),
            finished: finished_decision(
                track_finished,
                obs.playhead.has_pending(),
                obs.mixer.next_id != 0,
                drained,
            ),
            position_ms: audible_segment.map(|s| s.position_ms(obs.consumed)),
            duration_ms: entry.map(|e| e.track.duration_ms()),
            paused: self.is_paused(),
        }
    }

    /// Seek the audible track to `position` (interleaved sample index at the
    /// track's source rate). Stale audio is cut, so nothing from before the
    /// seek is heard; the track is left stopped (call `play()`), and any
    /// preload is dropped (re-issue `preload_track` afterwards).
    pub fn seek(&mut self, position: usize) -> Result<(), PlaybackError> {
        self.reconcile();
        let path = self
            .audible_track_id()
            .and_then(|id| self.tracks.get(&id))
            .map(|e| e.path.clone())
            .ok_or_else(|| {
                tracing::error!("No track loaded");
                PlaybackError::NoTrackLoaded
            })?;
        tracing::info!("Seeking to position {}", position);
        self.start_fresh(&path, Some(position))
    }
}

/// Whether the engine may report the audible track as finished. `drained` is
/// `None` when there is no audio output, which counts as drained.
fn finished_decision(
    track_finished: bool,
    promotion_pending: bool,
    next_registered: bool,
    drained: Option<bool>,
) -> bool {
    track_finished && !promotion_pending && !next_registered && drained.unwrap_or(true)
}

impl Drop for PlaybackEngine {
    fn drop(&mut self) {
        self.mix_thread_running.store(false, Ordering::Release);
        if let Some(handle) = self.mix_task.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn track_finished_requires_finished_and_drained() {
        assert!(!finished_decision(false, false, false, Some(false)));
        assert!(!finished_decision(false, false, false, Some(true)));
        assert!(!finished_decision(true, false, false, Some(false)));
        assert!(finished_decision(true, false, false, Some(true)));
    }

    #[test]
    fn track_finished_without_output_counts_as_drained() {
        assert!(finished_decision(true, false, false, None));
        assert!(!finished_decision(false, false, false, None));
    }

    #[test]
    fn track_not_finished_while_next_registered_or_promotion_pending() {
        assert!(!finished_decision(true, false, true, Some(true)));
        assert!(!finished_decision(true, true, false, Some(true)));
    }

    fn engine_with_tempdir() -> (PlaybackEngine, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("audio_config.json");
        let engine = PlaybackEngine::new(config_path).expect("engine created");
        (engine, tmp)
    }

    impl PlaybackEngine {
        /// Insert a test track (never registered with the mixer).
        fn insert_test_track(&mut self) -> TrackId {
            let id = self.alloc_track_id();
            self.tracks.insert(
                id,
                TrackEntry {
                    track: Track::new_test().expect("test track"),
                    path: PathBuf::from(format!("/test/{id}")),
                    rate: 48_000,
                },
            );
            id
        }

        /// A test track that is the audible one.
        fn insert_audible_test_track(&mut self) -> TrackId {
            let id = self.insert_test_track();
            self.start_playhead(id, 0, 0, 48_000);
            id
        }
    }

    fn wait_until(mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !cond() {
            assert!(Instant::now() < deadline, "timed out waiting for condition");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// A mixer slot pre-filled with `count` samples.
    fn filled_slot(id: u64, count: usize, complete: bool) -> Slot {
        let (mut producer, consumer) = HeapRb::<f32>::new(count + 64).split();
        for _ in 0..count {
            producer.push(0.5).ok();
        }
        Slot {
            id,
            consumer,
            input_complete: Arc::new(AtomicBool::new(complete)),
        }
    }

    /// Verify that the mix thread stops cleanly when shutdown() is called.
    #[test]
    fn shutdown_stops_mix_thread() {
        let (mut engine, _tmp) = engine_with_tempdir();
        assert!(engine.mix_task.is_some());
        engine.shutdown();
        assert!(engine.mix_task.is_none());
    }

    /// Verify that a missing config file results in the default config being used.
    #[test]
    fn new_with_missing_config_uses_defaults() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("nonexistent.json");
        let engine = PlaybackEngine::new(config_path).expect("engine created");
        let config = engine.get_output();
        assert_eq!(config.device_name, None);
        assert_eq!(config.sample_rate, 192_000);
    }

    /// Verify that a pre-existing config file is loaded on construction.
    #[test]
    fn new_loads_existing_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("audio_config.json");
        let saved = AudioOutputConfig {
            device_name: Some("test-device".to_string()),
            sample_rate: 48_000,
        };
        save_audio_config(&config_path, &saved).expect("save");

        let engine = PlaybackEngine::new(config_path).expect("engine created");
        assert_eq!(engine.get_output(), &saved);
    }

    /// Verify that when mixer_consumer is None and audio_output is None,
    /// the engine starts with a consumer available (the normal path), and that
    /// taking the consumer leaves it as None (simulating the post-hot-swap state
    /// where audio_output is Some and mixer_consumer is None).
    #[test]
    fn mixer_consumer_is_some_after_new_and_none_after_take() {
        let (mut engine, _tmp) = engine_with_tempdir();
        // Fresh engine: consumer present, no output yet
        assert!(
            engine.mixer_consumer.is_some(),
            "mixer_consumer should be Some after new()"
        );
        assert!(
            engine.audio_output.is_none(),
            "audio_output should be None after new()"
        );

        // Manually drain it to represent the post-spawn state.
        let _ = engine.mixer_consumer.take();
        assert!(
            engine.mixer_consumer.is_none(),
            "mixer_consumer should be None after take()"
        );
    }

    /// play() with no track loaded returns NoTrackLoaded error.
    #[test]
    fn play_with_no_track_returns_error() {
        let (mut engine, _tmp) = engine_with_tempdir();
        let result = engine.play();
        assert!(
            matches!(result, Err(PlaybackError::NoTrackLoaded)),
            "expected NoTrackLoaded, got {:?}",
            result
        );
    }

    /// stop() with no track loaded returns NoTrackLoaded error.
    #[test]
    fn stop_with_no_track_returns_error() {
        let (mut engine, _tmp) = engine_with_tempdir();
        let result = engine.stop();
        assert!(
            matches!(result, Err(PlaybackError::NoTrackLoaded)),
            "expected NoTrackLoaded, got {:?}",
            result
        );
    }

    /// unload_track() with no track loaded returns Ok (no-op).
    #[test]
    fn unload_track_with_no_track_is_ok() {
        let (mut engine, _tmp) = engine_with_tempdir();
        let result = engine.unload_track();
        assert!(result.is_ok(), "expected Ok, got {:?}", result);
    }

    #[test]
    fn engine_starts_paused() {
        let (engine, _tmp) = engine_with_tempdir();
        assert!(engine.is_paused());
    }

    #[test]
    fn play_clears_and_stop_sets_paused_flag() {
        let (mut engine, _tmp) = engine_with_tempdir();
        engine.insert_audible_test_track();
        engine.play().unwrap();
        assert!(!engine.is_paused(), "play must unpause");
        engine.stop().unwrap();
        assert!(engine.is_paused(), "stop must pause");
    }

    #[test]
    fn unload_leaves_engine_paused() {
        let (mut engine, _tmp) = engine_with_tempdir();
        engine.insert_audible_test_track();
        engine.play().unwrap();
        engine.unload_track().unwrap();
        assert!(engine.tracks.is_empty());
        assert!(engine.audible_track_id().is_none());
        assert!(engine.is_paused());
    }

    #[test]
    fn set_volume_stores_linear_gain() {
        let (mut engine, _tmp) = engine_with_tempdir();
        engine.set_volume(Volume::new(-20.0).unwrap()).unwrap();
        let gain = f32::from_bits(engine.shared.gain.load(Ordering::Acquire));
        assert!((gain - 0.1).abs() < 1e-6, "gain {gain}");
        engine.set_volume(Volume::UNITY).unwrap();
        let gain = f32::from_bits(engine.shared.gain.load(Ordering::Acquire));
        assert!((gain - 1.0).abs() < 1e-6);
    }

    #[test]
    fn position_follows_consumed_samples_and_freezes_without_consumption() {
        let (engine, _tmp) = {
            let (mut e, t) = engine_with_tempdir();
            e.insert_audible_test_track();
            (e, t)
        };
        assert_eq!(engine.status().position_ms, Some(0));
        // 48 kHz stereo: 96_000 samples = 1 s.
        engine
            .shared
            .consumed_total
            .store(96_000, Ordering::Release);
        assert_eq!(engine.status().position_ms, Some(1000));
        assert_eq!(
            engine.status().position_ms,
            Some(1000),
            "frozen without consumption"
        );
    }

    #[test]
    fn preload_requires_a_loaded_track() {
        let (mut engine, _tmp) = engine_with_tempdir();
        let r = engine.preload_track(Path::new("/nonexistent.flac"));
        assert!(matches!(r, Err(PlaybackError::NoTrackLoaded)));
    }

    #[test]
    fn replacing_a_parked_preload_drops_the_old_track() {
        let (mut engine, _tmp) = engine_with_tempdir();
        engine.insert_audible_test_track();
        let old = engine.insert_test_track();
        engine.preload = Some(Preload {
            id: old,
            slot: PreloadSlot::Parked(HeapRb::<f32>::new(8).split().1),
        });
        assert_eq!(engine.next_track_id(), Some(old));

        engine.clear_preload();

        assert_eq!(engine.next_track_id(), None);
        assert!(!engine.tracks.contains_key(&old), "old preload dropped");
        assert_eq!(engine.tracks.len(), 1, "audible track untouched");
    }

    #[test]
    fn unload_drops_the_preload_too() {
        let (mut engine, _tmp) = engine_with_tempdir();
        engine.insert_audible_test_track();
        let pre = engine.insert_test_track();
        engine.preload = Some(Preload {
            id: pre,
            slot: PreloadSlot::Parked(HeapRb::<f32>::new(8).split().1),
        });
        engine.unload_track().unwrap();
        assert!(engine.preload.is_none());
        assert!(engine.tracks.is_empty());
    }

    /// The mixer promotes the registered next; a ClearNext that arrives
    /// afterwards must not kill the track that is now playing, and the
    /// audible track switches exactly at the boundary.
    #[test]
    fn clear_next_after_promotion_is_harmless() {
        let (mut engine, _tmp) = engine_with_tempdir();
        let a = engine.insert_audible_test_track();
        let b = engine.insert_test_track();
        // A: 100 samples, complete. B: 50 samples.
        engine
            .command_sender
            .send(MixerCommand::RegisterTrack {
                slot: filled_slot(a, 100, true),
            })
            .unwrap();
        engine
            .command_sender
            .send(MixerCommand::RegisterNext {
                slot: filled_slot(b, 50, false),
            })
            .unwrap();
        engine.preload = Some(Preload {
            id: b,
            slot: PreloadSlot::Registered,
        });

        // Wait for the mix thread to promote B.
        let status = Arc::clone(&engine.mixer_status);
        wait_until(|| status.current_id() == b);
        assert_eq!(
            status.last_promotion().map(|p| p.boundary_sample),
            Some(100)
        );

        // Boundary not yet consumed: A still audible, nothing is finished.
        assert_eq!(engine.audible_track_id(), Some(a));
        assert!(!engine.status().finished);

        // The late ClearNext.
        engine.clear_preload();
        assert!(engine.tracks.contains_key(&b), "playing track must survive");
        assert_eq!(status.current_id(), b);
        assert_eq!(
            engine.next_track_id(),
            Some(b),
            "promoted B is committed: still the next track"
        );

        // Output consumes up to the boundary: B becomes audible, A is dropped.
        engine.shared.consumed_total.store(100, Ordering::Release);
        engine.reconcile();
        assert_eq!(engine.audible_track_id(), Some(b));
        assert!(!engine.tracks.contains_key(&a));
    }

    /// B1: after the mixer promoted B, B is the track the source plays next
    /// and stays live (its metadata owner must not drop it) until it has
    /// become audible; only then is A dropped and `next` empty.
    #[test]
    fn promoted_track_is_reported_next_and_live_until_audible() {
        let (mut engine, _tmp) = engine_with_tempdir();
        let a = engine.insert_audible_test_track();
        let b = engine.insert_test_track();
        engine
            .command_sender
            .send(MixerCommand::RegisterTrack {
                slot: filled_slot(a, 100, true),
            })
            .unwrap();
        engine
            .command_sender
            .send(MixerCommand::RegisterNext {
                slot: filled_slot(b, 50, false),
            })
            .unwrap();
        engine.preload = Some(Preload {
            id: b,
            slot: PreloadSlot::Registered,
        });
        assert_eq!(engine.next_track_id(), Some(b), "registered preload");

        let mixer_status = Arc::clone(&engine.mixer_status);
        wait_until(|| mixer_status.snapshot().current_id == b);

        // The Loaded poll: reconcile, then read one status.
        engine.reconcile();
        let st = engine.status();
        assert_eq!(st.audible, Some(a));
        assert_eq!(st.next, Some(b), "committed track is the one played next");
        assert!(!st.finished);
        assert!(engine.is_live(a) && engine.is_live(b));

        // Boundary consumed: B audible, A gone, nothing next.
        engine.shared.consumed_total.store(100, Ordering::Release);
        engine.reconcile();
        let st = engine.status();
        assert_eq!(st.audible, Some(b));
        assert_eq!(st.next, None);
        assert!(!engine.is_live(a));
        assert!(engine.is_live(b));
    }

    #[test]
    fn status_is_one_consistent_read() {
        let (mut engine, _tmp) = engine_with_tempdir();
        let a = engine.insert_audible_test_track();
        // 48 kHz stereo: 96_000 samples = 1 s.
        engine
            .shared
            .consumed_total
            .store(96_000, Ordering::Release);
        let st = engine.status();
        assert_eq!(st.audible, Some(a));
        assert_eq!(st.position_ms, Some(1000));
        assert_eq!(st.duration_ms, Some(0), "unknown duration is Some(0)");
        assert!(st.paused);
        assert_eq!(
            engine.status().position_ms,
            Some(1000),
            "frozen without consumption"
        );
    }

    #[test]
    fn status_without_track_is_empty() {
        let (engine, _tmp) = engine_with_tempdir();
        let st = engine.status();
        assert_eq!(st.audible, None);
        assert_eq!(st.next, None);
        assert!(!st.finished);
        assert_eq!(st.position_ms, None);
        assert_eq!(st.duration_ms, None);
        assert!(st.paused);
    }

    /// The reuse path (cut to a preloaded track): after DetachCurrent is acked
    /// and the ring flushed, only the promoted track's samples reach the ring.
    #[test]
    fn detach_flush_promote_delivers_only_the_promoted_tracks_samples() {
        let (mixer_producer, mut mixer_output) = HeapRb::<f32>::new(4096).split();
        let (cmd_tx, cmd_rx) = mpsc::channel::<MixerCommand>();
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);
        let thread = std::thread::spawn(move || {
            run_mix_loop(mixer::Mixer::new(mixer_producer), cmd_rx, running_clone);
        });

        // A has plenty of audio (1.0); B (0.5) is registered as next.
        let (mut a_producer, a_consumer) = HeapRb::<f32>::new(8192).split();
        for _ in 0..2048 {
            a_producer.push(1.0).ok();
        }
        cmd_tx
            .send(MixerCommand::RegisterTrack {
                slot: Slot {
                    id: 1,
                    consumer: a_consumer,
                    input_complete: Arc::new(AtomicBool::new(false)),
                },
            })
            .unwrap();
        cmd_tx
            .send(MixerCommand::RegisterNext {
                slot: filled_slot(2, 300, true),
            })
            .unwrap();
        // Let A's audio reach the ring.
        std::thread::sleep(Duration::from_millis(20));

        let (detach_tx, detach_rx) = mpsc::sync_channel::<()>(1);
        cmd_tx
            .send(MixerCommand::DetachCurrent { done: detach_tx })
            .unwrap();
        detach_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        // The output flush: discard everything A produced.
        let mut sink = vec![0.0f32; 4096];
        mixer_output.pop_slice(&mut sink);
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(
            mixer_output.len(),
            0,
            "nothing flows between detach and promote"
        );

        let (promote_tx, promote_rx) = mpsc::sync_channel::<Option<u64>>(1);
        cmd_tx
            .send(MixerCommand::PromoteNext {
                id: 2,
                done: promote_tx,
            })
            .unwrap();
        let start = promote_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .expect("B is the pending next");
        assert!(start > 0, "A's samples were produced before the cut");

        let deadline = Instant::now() + Duration::from_secs(2);
        while mixer_output.len() < 300 {
            assert!(Instant::now() < deadline, "B's samples never arrived");
            std::thread::sleep(Duration::from_millis(1));
        }
        std::thread::sleep(Duration::from_millis(10));
        let got = {
            let mut v = vec![0.0f32; mixer_output.len()];
            let n = mixer_output.pop_slice(&mut v);
            v.truncate(n);
            v
        };

        running.store(false, Ordering::Release);
        thread.join().unwrap();
        assert_eq!(got.len(), 300);
        assert!(
            got.iter().all(|s| *s == 0.5),
            "only B's samples (0.5) reach the ring"
        );
    }

    /// select_target_rate passes source rate through without clamping (no 192 kHz ceiling).
    #[test]
    fn select_target_rate_passes_through_high_rate() {
        assert_eq!(select_target_rate(384_000, 192_000), 384_000);
    }

    /// select_target_rate passes through source rate when below default.
    #[test]
    fn select_target_rate_passes_through_44100() {
        assert_eq!(select_target_rate(44_100, 192_000), 44_100);
    }

    /// select_target_rate passes through 96 kHz (common hi-res rate).
    #[test]
    fn select_target_rate_passes_through_96000() {
        assert_eq!(select_target_rate(96_000, 192_000), 96_000);
    }

    /// select_target_rate falls back to default when source rate is zero.
    #[test]
    fn select_target_rate_zero_source_falls_back_to_max() {
        assert_eq!(select_target_rate(0, 192_000), 192_000);
    }

    /// select_target_rate passes through exact default rate.
    #[test]
    fn select_target_rate_at_ceiling_is_unchanged() {
        assert_eq!(select_target_rate(192_000, 192_000), 192_000);
    }

    /// With a full mixer ring (the old blocking-push scenario), an acked
    /// ClearTrack followed by a drain leaves the ring empty: the mixer holds
    /// no already-popped samples that could land after the flush.
    #[test]
    fn full_ring_then_acked_clear_leaks_nothing() {
        use ringbuf::HeapRb;
        use std::time::Duration;

        let (mixer_producer, mut mixer_output) = HeapRb::<f32>::new(256).split();
        let (cmd_tx, cmd_rx) = mpsc::channel::<MixerCommand>();
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);
        let thread = std::thread::spawn(move || {
            run_mix_loop(mixer::Mixer::new(mixer_producer), cmd_rx, running_clone);
        });

        let (mut track_producer, track_consumer) = HeapRb::<f32>::new(4096).split();
        for _ in 0..2048 {
            track_producer.push(1.0f32).ok();
        }
        cmd_tx
            .send(MixerCommand::RegisterTrack {
                slot: Slot {
                    id: 1,
                    consumer: track_consumer,
                    input_complete: Arc::new(AtomicBool::new(false)),
                },
            })
            .unwrap();
        // Let the output ring fill up.
        std::thread::sleep(Duration::from_millis(20));

        let (done_tx, done_rx) = mpsc::sync_channel::<()>(1);
        cmd_tx
            .send(MixerCommand::ClearTrack { done: done_tx })
            .unwrap();
        done_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        // "Flush": discard everything in the ring.
        let mut drain = vec![0.0f32; 256];
        mixer_output.pop_slice(&mut drain);
        std::thread::sleep(Duration::from_millis(10));
        let leaked = mixer_output.len();

        running.store(false, Ordering::Release);
        thread.join().unwrap();
        assert_eq!(leaked, 0, "no samples may reach the ring after the ack");
    }

    /// After sending ClearTrack, the mix thread stops producing samples.
    /// We verify by registering a consumer with data, sending ClearTrack,
    /// letting the mix thread run briefly, then checking the mixer output ring
    /// is still empty (because the consumer was cleared before the mixer ran).
    #[test]
    fn clear_track_command_stops_mix_thread_producing_samples() {
        use ringbuf::HeapRb;
        use std::time::Duration;

        // Build a standalone mix thread replicating the engine's mix loop
        // so we can inspect the mixer output consumer directly.
        let mixer_rb = HeapRb::<f32>::new(262_144);
        let (mixer_producer, mut mixer_output) = mixer_rb.split();

        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<MixerCommand>();
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);

        // Runs the engine's real mix loop.
        let thread = std::thread::spawn(move || {
            run_mix_loop(mixer::Mixer::new(mixer_producer), cmd_rx, running_clone);
        });

        // Push samples into a track ring buffer and register it
        let track_rb = HeapRb::<f32>::new(131_072);
        let (mut track_producer, track_consumer) = track_rb.split();
        for _ in 0..1024 {
            track_producer.push(1.0f32).ok();
        }
        cmd_tx
            .send(MixerCommand::RegisterTrack {
                slot: Slot {
                    id: 1,
                    consumer: track_consumer,
                    input_complete: Arc::new(AtomicBool::new(false)),
                },
            })
            .unwrap();

        // Let the mix thread run a few cycles so it starts consuming
        std::thread::sleep(Duration::from_millis(5));

        // Now send ClearTrack and drain any samples that already landed
        let (done_tx, done_rx) = mpsc::sync_channel::<()>(1);
        cmd_tx
            .send(MixerCommand::ClearTrack { done: done_tx })
            .unwrap();
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("mix thread must acknowledge ClearTrack");

        let before_len = mixer_output.len();
        // After the ack nothing more may arrive: drain, then the ring stays empty.
        let mut drain = vec![0.0f32; 262_144];
        mixer_output.pop_slice(&mut drain);

        // Wait another round — mix thread should not produce any more samples
        std::thread::sleep(Duration::from_millis(5));

        let after_len = mixer_output.len();

        running.store(false, Ordering::Release);
        thread.join().unwrap();

        assert_eq!(
            after_len, 0,
            "after ClearTrack, mix thread must produce 0 new samples (had {} before drain)",
            before_len
        );
    }
}
