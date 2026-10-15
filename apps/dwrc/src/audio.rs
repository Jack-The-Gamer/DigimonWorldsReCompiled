//! Audio output: a ring buffer fed by the emulation thread at 44.1 kHz and
//! drained by the system audio callback, resampled to the device rate with
//! gentle drift correction so audio never pops or drifts out of sync.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

pub struct AudioShared {
    pub ring: Mutex<VecDeque<(f32, f32)>>,
    volume_bits: AtomicU32,
}

impl AudioShared {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { ring: Mutex::new(VecDeque::with_capacity(16384)), volume_bits: AtomicU32::new(0.8f32.to_bits()) })
    }

    pub fn set_volume(&self, v: f32) {
        self.volume_bits.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    fn volume(&self) -> f32 {
        f32::from_bits(self.volume_bits.load(Ordering::Relaxed))
    }

    /// Pushes interleaved 44.1 kHz stereo samples.
    pub fn push(&self, samples: &[i16]) {
        let mut r = self.ring.lock().unwrap();
        for c in samples.chunks_exact(2) {
            r.push_back((c[0] as f32 / 32768.0, c[1] as f32 / 32768.0));
        }
        // Never let latency grow beyond ~250 ms.
        while r.len() > 11025 {
            r.pop_front();
        }
    }

    pub fn clear(&self) {
        self.ring.lock().unwrap().clear();
    }

}

pub struct AudioOut {
    _stream: cpal::Stream,
}

const TARGET_FILL: f64 = 2048.0;

pub fn start(shared: Arc<AudioShared>) -> anyhow::Result<AudioOut> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or_else(|| anyhow::anyhow!("no audio output device"))?;
    let supported = device.default_output_config()?;
    let rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let mut pos = 0.0f64;
    let mut prev = (0.0f32, 0.0f32);
    let mut cur = (0.0f32, 0.0f32);
    let base_step = 44_100.0 / rate as f64;
    let sh = shared.clone();
    let mut fill = move |out: &mut dyn FnMut(usize, f32), frames: usize| {
        let mut ring = sh.ring.lock().unwrap();
        let vol = sh.volume();
        // Drift correction: speed up slightly when the buffer is too full.
        let err = (ring.len() as f64 - TARGET_FILL) / TARGET_FILL;
        let step = base_step * (1.0 + (err * 0.005).clamp(-0.005, 0.005));
        for f in 0..frames {
            pos += step;
            while pos >= 1.0 {
                pos -= 1.0;
                prev = cur;
                if let Some(s) = ring.pop_front() {
                    cur = s;
                } else {
                    // Underrun: fade towards silence.
                    cur = (cur.0 * 0.95, cur.1 * 0.95);
                }
            }
            let t = pos as f32;
            let l = (prev.0 + (cur.0 - prev.0) * t) * vol;
            let r = (prev.1 + (cur.1 - prev.1) * t) * vol;
            for ch in 0..channels {
                out(f * channels + ch, if ch % 2 == 0 { l } else { r });
            }
        }
    };
    let err_fn = |e| log::error!("audio stream error: {e}");
    let stream = match format {
        cpal::SampleFormat::F32 => device.build_output_stream(
            &config,
            move |data: &mut [f32], _| {
                let frames = data.len() / channels;
                fill(&mut |i, v| data[i] = v, frames);
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_output_stream(
            &config,
            move |data: &mut [i16], _| {
                let frames = data.len() / channels;
                fill(&mut |i, v| data[i] = (v.clamp(-1.0, 1.0) * 32767.0) as i16, frames);
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_output_stream(
            &config,
            move |data: &mut [u16], _| {
                let frames = data.len() / channels;
                fill(&mut |i, v| data[i] = ((v.clamp(-1.0, 1.0) * 32767.0) as i32 + 32768) as u16, frames);
            },
            err_fn,
            None,
        )?,
        other => anyhow::bail!("unsupported audio sample format {other:?}"),
    };
    stream.play()?;
    Ok(AudioOut { _stream: stream })
}
