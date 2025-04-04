use pipewire as pw;
use pw::properties::properties;
use ringbuf::HeapConsumer;
use std::thread;
use tracing::{debug, info};

pub struct PipewireOutput {
    // Thread handle to keep the PipeWire thread alive
    _pw_thread: thread::JoinHandle<()>,
}

impl PipewireOutput {
    pub fn new(sample_consumer: HeapConsumer<f32>) -> Result<Self, pw::Error> {
        // Create a ring buffer for audio samples
        info!("create pipe wire thread");
        // Spawn PipeWire thread
        let pw_thread = thread::spawn(move || {
            // Initialize PipeWire

            info!("Initialize pipewoire");
            pw::init();

            // Create main loop
            let mainloop =
                pw::main_loop::MainLoop::new(None).expect("Failed to create PipeWire main loop");

            // Create context
            let context =
                pw::context::Context::new(&mainloop).expect("Failed to create PipeWire context");

            // Connect core
            let core = context
                .connect(None)
                .expect("Failed to connect to PipeWire core");

            info!("connected to pipewire core");

            // Prepare audio format
            let obj = pw::spa::pod::object!(
                pw::spa::utils::SpaTypes::ObjectParamFormat,
                pw::spa::param::ParamType::EnumFormat,
                pw::spa::pod::property!(
                    pw::spa::param::format::FormatProperties::MediaType,
                    Id,
                    pw::spa::param::format::MediaType::Audio
                ),
                pw::spa::pod::property!(
                    pw::spa::param::format::FormatProperties::MediaSubtype,
                    Id,
                    pw::spa::param::format::MediaSubtype::Raw
                ),
                pw::spa::pod::property!(
                    pw::spa::param::format::FormatProperties::AudioFormat,
                    Id,
                    pw::spa::param::audio::AudioFormat::F32P
                ),
                pw::spa::pod::property!(
                    pw::spa::param::format::FormatProperties::AudioRate,
                    Int,
                    48000
                ),
                pw::spa::pod::property!(
                    pw::spa::param::format::FormatProperties::AudioChannels,
                    Int,
                    2
                )
            );

            // Serialize the object
            let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
                std::io::Cursor::new(Vec::new()),
                &pw::spa::pod::Value::Object(obj),
            )
            .unwrap()
            .0
            .into_inner();

            // Create params
            let mut params = [pw::spa::pod::Pod::from_bytes(&values).unwrap()];

            // Create stream
            let stream = pw::stream::Stream::new(
                &core,
                "mdma-audio-output",
                properties! {
                    *pw::keys::MEDIA_TYPE => "Audio",
                    *pw::keys::MEDIA_CATEGORY => "Playback",
                    *pw::keys::MEDIA_ROLE => "Music"
                },
            )
            .expect("Failed to create stream");
            info!("stream created");
            // Prepare user data for the stream
            struct UserData {
                sample_consumer: HeapConsumer<f32>,
            }

            let user_data = UserData { sample_consumer };

            // Create stream listener
            let _listener = stream
                .add_local_listener_with_user_data(user_data)
                .state_changed(|_, _, old, new| {
                    println!("Stream state changed: {:?} -> {:?}", old, new);
                })
                .process(|stream, user_data| {
                    // Get buffer from stream
                    let mut buffer = match stream.dequeue_buffer() {
                        None => return,
                        Some(buffer) => buffer,
                    };

                    // Get buffer data
                    let datas = buffer.datas_mut();
                    if datas.is_empty() {
                        return;
                    }

                    let data = &mut datas[0];

                    // Determine buffer size and sample count
                    let chunk = data.chunk();
                    let n_frames = chunk.size() / 4; // Assuming 4 bytes per sample (f32)
                    let sample_count: usize = (n_frames * 2) as usize; // stereo

                    debug!("sample count {}", 0);
                    if sample_count == 0 {
                        return;
                    }

                    // Get data pointer as slice
                    let slice = unsafe {
                        std::slice::from_raw_parts_mut(
                            data.data().unwrap().as_mut_ptr() as *mut f32,
                            sample_count,
                        )
                    };

                    debug!("got beffer slice {}", slice.len());

                    // Fill buffer from ring buffer
                    let read = user_data
                        .sample_consumer
                        .pop_slice(&mut slice[..sample_count]);

                    debug!("Read {} bytes", read);

                    // Zero out any remaining space
                    if read < slice.len() {
                        slice[read..].fill(0.0);
                    }
                })
                .register()
                .expect("Failed to register stream listener");

            // Connect the stream
            stream
                .connect(
                    pw::spa::utils::Direction::Output,
                    None,
                    pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
                    &mut params,
                )
                .expect("Failed to connect stream");

            // Run the main loop (this will block)
            info!("Starting pipewire main loop");
            mainloop.run();

            // Cleanup (this won't be reached until mainloop is stopped)
            println!("PipeWire thread exiting");
            unsafe {
                pw::deinit();
            }
        });

        Ok(Self {
            _pw_thread: pw_thread,
        })
    }
}
