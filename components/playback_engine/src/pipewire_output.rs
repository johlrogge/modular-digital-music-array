// src/pipewire_output.rs

use crate::error::PlaybackError;
use pipewire as pw;
use ringbuf::HeapConsumer;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

pub struct PipewireOutput {
    running: Arc<AtomicBool>,
    pw_thread: Option<std::thread::JoinHandle<()>>,
}

impl PipewireOutput {
    pub fn new(audio_consumer: HeapConsumer<f32>) -> Result<Self, PlaybackError> {
        // Initialize the flag as running
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        // Create a thread for PipeWire processing
        let pw_thread = std::thread::spawn(move || {
            // Initialize PipeWire inside the thread
            pw::init();

            // Create the main loop
            let main_loop =
                pw::main_loop::MainLoop::new(None).expect("Failed to create PipeWire main loop");

            // Create context and connect
            let context =
                pw::context::Context::new(&main_loop).expect("Failed to create PipeWire context");
            let core = context
                .connect(None)
                .expect("Failed to connect to PipeWire");

            tracing::info!("PipeWire initialized successfully");

            // Set up a stream
            let stream =
                setup_audio_stream(&core, audio_consumer).expect("Failed to set up audio stream");

            tracing::info!("PipeWire stream set up successfully");

            // Run the main loop until signaled to stop
            while running_clone.load(Ordering::SeqCst) {
                main_loop.run();
                std::thread::sleep(std::time::Duration::from_millis(10));
            }

            // Clean up
            drop(stream);
            drop(core);
            drop(context);
            drop(main_loop);

            // Use unsafe block for deinit
            unsafe {
                pw::deinit();
            }
        });

        Ok(Self {
            running,
            pw_thread: Some(pw_thread),
        })
    }
}

#[derive(Default)]
struct ListenerData {
    // We can add fields here if needed later
}
// Simplified stream setup
fn setup_audio_stream(
    core: &pw::core::Core,
    consumer: HeapConsumer<f32>,
) -> Result<pw::stream::Stream, PlaybackError> {
    // Audio format parameters
    const SAMPLE_RATE: u32 = 48000;
    const CHANNELS: u32 = 2;

    // Create properties using the properties! macro
    use pw::properties::properties;

    let props = properties! {
        "media.class" => "Audio/Sink",
        "node.name" => "mdma_playback"
    };

    // Create the stream
    let stream = pw::stream::Stream::new(core, "MDMA Playback", props)
        .map_err(|e| PlaybackError::AudioDevice(format!("Failed to create stream: {}", e)))?;

    // Create a shared consumer reference for callbacks
    let consumer_ref = Arc::new(parking_lot::Mutex::new(consumer));

    // Create debug counter
    let read_samples = Arc::new(AtomicUsize::new(0));
    let read_samples_clone = read_samples.clone();

    // Create periodic logging task
    std::thread::spawn(move || loop {
        let samples = read_samples_clone.swap(0, Ordering::Relaxed);
        tracing::info!(
            "PipeWire audio callback read {} samples in the last second",
            samples
        );
        std::thread::sleep(std::time::Duration::from_secs(1));
    });
    // Set up the stream listener with a process callback
    let consumer_clone = consumer_ref.clone();
    let read_samples = Arc::clone(&read_samples);

    // Use the new listener API in pipewire 0.8
    let _listener = stream
        .add_local_listener::<ListenerData>()
        .process(move |stream, _| {
            let callback_start = std::time::Instant::now();

            // Get the stream buffer
            let mut buffer = match stream.dequeue_buffer() {
                None => return,
                Some(buffer) => buffer,
            };

            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }

            let data = &mut datas[0];
            let data_ptr = match data.data() {
                None => return,
                Some(ptr) => ptr.as_mut_ptr() as *mut f32,
            };

            // Get the chunk size
            let chunk_size = data.chunk().size() as usize;
            let n_samples = chunk_size / std::mem::size_of::<f32>();

            // Create a slice from the raw pointer
            let output_slice = unsafe { std::slice::from_raw_parts_mut(data_ptr, n_samples) };

            // Now we can copy audio data from our consumer
            let mut consumer_guard = consumer_clone.lock();
            let available = consumer_guard.len();

            // Read from consumer to fill output buffer
            let read = consumer_guard.pop_slice(output_slice);
            if read < output_slice.len() {
                tracing::warn!(
                    "buffer underrun!!! {read}/{} samples available {available}",
                    output_slice.len()
                );

                // Fill the rest with silence
                output_slice[read..].fill(0.0);
            }

            // Update read counter
            read_samples.fetch_add(read, Ordering::Relaxed);

            let total_time = callback_start.elapsed();
            if total_time > std::time::Duration::from_millis(1) {
                tracing::warn!("Audio callback total time: {:?}", total_time);
            }
        })
        .register();

    // Create audio format using the builder_add! macro with proper Id wrapping
    use pw::spa::param::audio::AudioFormat;
    use pw::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use pw::spa::pod::builder::{builder_add, Builder};
    use pw::spa::pod::Pod;
    use pw::spa::utils::{Id, SpaTypes};

    // Create a buffer for the pod
    let mut buffer = vec![0u8; 1024];
    let mut builder = Builder::new(&mut buffer);

    // Create a simple audio format parameter with proper Id wrapping
    builder_add!(&mut builder,
        Object(
            SpaTypes::ObjectParamFormat.as_raw(),
            SpaTypes::None.as_raw()
        ) {
            FormatProperties::MediaType.as_raw() => Id(Id(MediaType::Audio.as_raw())),
            FormatProperties::MediaSubtype.as_raw() => Id(Id(MediaSubtype::Raw.as_raw())),
            FormatProperties::AudioFormat.as_raw() => Id(Id(AudioFormat::F32P.as_raw())),
            FormatProperties::AudioRate.as_raw() => Int(SAMPLE_RATE as i32),
            FormatProperties::AudioChannels.as_raw() => Int(CHANNELS as i32),
        }
    )
    .expect("Failed to create pod");

    // In pipewire 0.8, we need to use a different approach to get the pod
    // The builder_add! macro has modified our buffer and we can now create a pod reference from it
    let pod_ref = unsafe { Pod::from_raw(buffer.as_ptr() as *const _) };

    // Create a slice of pod references as needed by connect()
    let mut params = [pod_ref];

    // Connect the stream
    stream
        .connect(
            pw::spa::utils::Direction::Output,
            None,
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|e| PlaybackError::AudioDevice(format!("Failed to connect stream: {}", e)))?;

    tracing::info!("PipeWire stream connected successfully");

    Ok(stream)
}

impl Drop for PipewireOutput {
    fn drop(&mut self) {
        // Signal the thread to stop
        self.running.store(false, Ordering::SeqCst);

        // Wait for the thread to finish
        if let Some(thread) = self.pw_thread.take() {
            thread.join().expect("Failed to join PipeWire thread");
        }
    }
}
