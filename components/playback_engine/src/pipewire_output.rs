use std::thread;

use pipewire as pw;
use pw::{properties::properties, spa};
use ringbuf::HeapConsumer;
use spa::pod::Pod;
use tracing::{debug, info};

pub const DEFAULT_RATE: u32 = 48000;
pub const DEFAULT_CHANNELS: u32 = 2;
pub const CHAN_SIZE: usize = std::mem::size_of::<i16>();

pub struct PipewireOutput {
    // Thread handle to keep the PipeWire thread alive
    _pw_thread: thread::JoinHandle<Result<(), pw::Error>>,
}

impl PipewireOutput {
    pub fn new(sample_consumer: HeapConsumer<f32>) -> Result<Self, pw::Error> {
        // Create a ring buffer for audio samples
        info!("create pipe wire thread");
        // Spawn PipeWire thread
        let pw_thread = thread::spawn(move || {
            pw::init();
            let mainloop = pw::main_loop::MainLoop::new(None)?;
            let context = pw::context::Context::new(&mainloop)?;
            let core = context.connect(None)?;

            // Create user data struct to hold consumer
            struct UserData {
                consumer: HeapConsumer<f32>,
                frame_count: usize, // For debugging
            }

            let user_data = UserData {
                consumer: sample_consumer,
                frame_count: 0,
            };

            let stream = pw::stream::Stream::new(
                &core,
                "mdma-audio-output",
                properties! {
                    *pw::keys::MEDIA_TYPE => "Audio",
                    *pw::keys::MEDIA_ROLE => "Music",
                    *pw::keys::MEDIA_CATEGORY => "Playback",
                    *pw::keys::AUDIO_CHANNELS => "2",
                },
            )?;

            let _listener = stream
                .add_local_listener_with_user_data(user_data)
                .process(|stream, user_data| match stream.dequeue_buffer() {
                    None => println!("No buffer received"),
                    Some(mut buffer) => {
                        let datas = buffer.datas_mut();
                        let stride = CHAN_SIZE * DEFAULT_CHANNELS as usize;
                        let data = &mut datas[0];

                        let n_frames = if let Some(slice) = data.data() {
                            let n_frames = slice.len() / stride;

                            // Log every 100 frames for debugging
                            user_data.frame_count += 1;
                            if user_data.frame_count % 100 == 0 {
                                debug!(
                                    "Processing {} frames, consumer has {} samples",
                                    n_frames,
                                    user_data.consumer.len()
                                );
                            }

                            // Temporary buffer to read from consumer
                            let mut f32_buffer = vec![0.0f32; n_frames * DEFAULT_CHANNELS as usize];

                            // Read from consumer
                            let samples_read = user_data.consumer.pop_slice(&mut f32_buffer);

                            if user_data.frame_count % 100 == 0 && samples_read > 0 {
                                debug!("Read {} samples from consumer", samples_read);
                            }

                            // Convert f32 samples to i16 and copy to output buffer
                            for i in 0..n_frames {
                                for c in 0..DEFAULT_CHANNELS {
                                    // Calculate index in f32 buffer
                                    let f32_idx = i * DEFAULT_CHANNELS as usize + c as usize;

                                    // Get sample (or 0 if we've read all samples)
                                    let f32_sample = if f32_idx < samples_read {
                                        f32_buffer[f32_idx]
                                    } else {
                                        0.0
                                    };

                                    // Convert to i16 (-1.0..1.0 -> -32767..32767)
                                    let val =
                                        (f32_sample * 32767.0).clamp(-32767.0, 32767.0) as i16;

                                    // Copy to output buffer
                                    let start = i * stride + (c as usize * CHAN_SIZE);
                                    let end = start + CHAN_SIZE;
                                    let chan = &mut slice[start..end];
                                    chan.copy_from_slice(&i16::to_le_bytes(val));
                                }
                            }

                            n_frames
                        } else {
                            0
                        };

                        let chunk = data.chunk_mut();
                        *chunk.offset_mut() = 0;
                        *chunk.stride_mut() = stride as _;
                        *chunk.size_mut() = (stride * n_frames) as _;
                    }
                })
                .register()?;

            let mut audio_info = spa::param::audio::AudioInfoRaw::new();
            audio_info.set_format(spa::param::audio::AudioFormat::S16LE);
            audio_info.set_rate(DEFAULT_RATE);
            audio_info.set_channels(DEFAULT_CHANNELS);
            let mut position = [0; spa::param::audio::MAX_CHANNELS];
            position[0] = spa_sys::SPA_AUDIO_CHANNEL_FL;
            position[1] = spa_sys::SPA_AUDIO_CHANNEL_FR;
            audio_info.set_position(position);

            let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
                std::io::Cursor::new(Vec::new()),
                &pw::spa::pod::Value::Object(pw::spa::pod::Object {
                    type_: spa_sys::SPA_TYPE_OBJECT_Format,
                    id: spa_sys::SPA_PARAM_EnumFormat,
                    properties: audio_info.into(),
                }),
            )
            .unwrap()
            .0
            .into_inner();

            let mut params = [Pod::from_bytes(&values).unwrap()];

            stream.connect(
                spa::utils::Direction::Output,
                None,
                pw::stream::StreamFlags::AUTOCONNECT
                    | pw::stream::StreamFlags::MAP_BUFFERS
                    | pw::stream::StreamFlags::RT_PROCESS,
                &mut params,
            )?;

            mainloop.run();
            Ok(())
        });

        Ok(Self {
            _pw_thread: pw_thread,
        })
    }
}
