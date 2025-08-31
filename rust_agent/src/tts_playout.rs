use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, StreamTrait, HostTrait};
use cpal::SampleFormat;
use ringbuf::{Consumer, HeapRb};
use ringbuf::ring_buffer::{RbRead, RbRef};
use std::io::Read;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const SR: u32 = 48_000;
const CHANNELS: u16 = 1;
const FRAME_SAMPLES: usize = (SR as usize * 20) / 1000; // 20 ms -> 960
const BYTES_PER_FRAME: usize = FRAME_SAMPLES * std::mem::size_of::<f32>(); // 3840

#[inline]
fn f32_to_i16(x: f32) -> i16 {
    let y = x.clamp(-1.0, 1.0) * i16::MAX as f32;
    y as i16
}

#[inline]
fn f32_to_u16(x: f32) -> u16 {
    // map [-1,1] -> [0, 65535]
    let y = ((x.clamp(-1.0, 1.0) * 0.5) + 0.5) * (u16::MAX as f32);
    y.round()
        .clamp(0.0, u16::MAX as f32) as u16
}


fn pick_output_device() -> Result<cpal::Device> {
    let override_name = std::env::var("VIRTUAL_AUDIO_DEVICE")
        .unwrap_or_default()
        .to_lowercase();

    // Substrings from known virtual devices
    let preferred = [
        // Windows
        "cable input", "vb-audio",
        // macOS
        "blackhole", "loopback",
        // Linux
        "loopback", "snd-aloop", "monitor", "null",
    ];

    let host = cpal::default_host();
    let mut fallback = None;

    for dev in host.output_devices().context("list output devices")? {
        let name = dev.name().unwrap_or_default();
        let lname = name.to_lowercase();

        if !override_name.is_empty() && lname.contains(&override_name) {
            eprintln!("Picked device (override) → {name}");
            return Ok(dev);
        }
        if preferred.iter().any(|k| lname.contains(k)) {
            eprintln!("Picked device (preferred) -> {name}");
            return Ok(dev);
        }
        if fallback.is_none() {
            fallback = Some((name, dev));
        }
    }

    let (name, dev) = fallback.context("no output devices found")?;
    eprintln!("Picked default device → {name}");
    Ok(dev)
}

fn build_stream<R>(
    device: &cpal::Device,
    cons: Consumer<f32, R>,
) -> Result<cpal::Stream>
where
    R: RbRef + Send + 'static,
    R::Rb: RbRead<f32> + Send + 'static,
{
    let supported = device.default_output_config()?;
    eprintln!(
        "Device default: {:?} @ {} Hz, {} ch",
        supported.sample_format(),
        supported.sample_rate().0,
        supported.channels()
    );

    let cfg = cpal::StreamConfig {
        channels: supported.channels(),
        sample_rate: supported.sample_rate(),
        buffer_size: cpal::BufferSize::Default,
    };

    if cfg.sample_rate.0 != 48_000 {
        eprintln!(
            "WARNING: device SR {} ≠ input SR 48000. (We can add resampling later.)",
            cfg.sample_rate.0
        );
    }

    let err_fn = |e| eprintln!("audio stream error: {e}");

    // Move `cons` into the chosen callback arm
    let stream = match supported.sample_format() {
        SampleFormat::F32 => {
            let mut cons_f32 = cons;
            device.build_output_stream(
                &cfg,
                move |data: &mut [f32], _| {
                    let ch = cfg.channels as usize;
                    for frame in data.chunks_mut(ch) {
                        let s = cons_f32.pop().unwrap_or(0.0f32);
                        for out in frame.iter_mut() { *out = s; }
                    }
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::I16 => {
            let mut cons_f32 = cons;
            device.build_output_stream(
                &cfg,
                move |data: &mut [i16], _| {
                    let ch = cfg.channels as usize;
                    for frame in data.chunks_mut(ch) {
                        let s = cons_f32.pop().unwrap_or(0.0f32);
                        let v: i16 = f32_to_i16(s);
                        for out in frame.iter_mut() { *out = v; }
                    }
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::U16 => {
            let mut cons_f32 = cons;
            device.build_output_stream(
                &cfg,
                move |data: &mut [u16], _| {
                    let ch = cfg.channels as usize;
                    for frame in data.chunks_mut(ch) {
                        let s = cons_f32.pop().unwrap_or(0.0f32);
                        let v: u16 = f32_to_u16(s);
                        for out in frame.iter_mut() { *out = v; }
                    }
                },
                err_fn,
                None,
            )?
        }
        other => {
            eprintln!("Unsupported device sample format {:?}; falling back to f32.", other);
            let mut cons_f32 = cons;
            device.build_output_stream(
                &cpal::StreamConfig {
                    channels: 1,
                    sample_rate: cpal::SampleRate(48_000),
                    buffer_size: cpal::BufferSize::Default,
                },
                move |data: &mut [f32], _| {
                    for s in data.iter_mut() { *s = cons_f32.pop().unwrap_or(0.0); }
                },
                err_fn,
                None,
            )?
        }
    };

    Ok(stream)
}

pub fn run() -> Result<()> {
    // ~200 ms heap-backed ring buffer
    let rb = HeapRb::<f32>::new((SR as usize / 5) * CHANNELS as usize);
    let (prod, cons) = rb.split();

    let device = pick_output_device()?;
    let stream = build_stream(&device, cons)?;
    stream.play()?;
    eprintln!("Playout stream @ 48kHz mono ready.");

    // Share the producer with per-connection threads
    let prod = Arc::new(Mutex::new(prod));

    // TCP server: each client sends 20 ms f32 frames at 48 kHz mono
    let listener = TcpListener::bind(("127.0.0.1", 49160)).context("bind 49160")?;
    eprintln!("Listening for PCM frames on 127.0.0.1:49160");

    for conn in listener.incoming() {
        let mut sock = conn.context("accept")?;
        eprintln!("Sender connected.");

        // Expect 18-byte header
        let mut header = [0u8; 18];
        sock.read_exact(&mut header)?;
        if &header != b"PCM48K_F32LE_MONO\n" {
            eprintln!("Bad header; closing.");
            continue;
        }

        let prod_handle = Arc::clone(&prod);

        let handle = thread::spawn(move || -> Result<()> {
            let mut buf = vec![0u8; BYTES_PER_FRAME];
            loop {
                // Read exactly one 20 ms frame
                let mut read = 0usize;
                while read < BYTES_PER_FRAME {
                    let n = sock.read(&mut buf[read..])?;
                    if n == 0 {
                        return Ok(()); 
                    }
                    read += n;
                }
                // Reinterpret as f32 samples
                let samples: &[f32] =
                    unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const f32, FRAME_SAMPLES) };

                if let Ok(mut p) = prod_handle.lock() {
                    for &s in samples {
                        let _ = p.push(s);
                    }
                }
            }
        });

        // Wait for client to finish; accept the next one afterwards
        while !handle.is_finished() {
            thread::sleep(Duration::from_millis(50));
        }
        let _ = handle.join();
        eprintln!("Sender disconnected.");
    }

    Ok(())
}
