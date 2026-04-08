use crate::audio::receiver::BufferedFrame;
use std::f64::consts::PI;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WavChunk {
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioError {
    InvalidPcmLength(usize),
    PcmTooLarge(usize),
}

impl Display for AudioError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPcmLength(length) => {
                write!(
                    f,
                    "invalid PCM byte length (must be multiple of 2): {length}"
                )
            }
            Self::PcmTooLarge(length) => {
                write!(
                    f,
                    "PCM data too large for WAV format (max ~4GB): {length} bytes"
                )
            }
        }
    }
}

impl std::error::Error for AudioError {}

pub fn build_wav_chunk(frames: &[BufferedFrame], sample_rate: u32) -> Result<WavChunk, AudioError> {
    let mut pcm = Vec::new();
    for frame in frames {
        if frame.pcm_16le_bytes.len() % 2 != 0 {
            return Err(AudioError::InvalidPcmLength(frame.pcm_16le_bytes.len()));
        }
        pcm.extend_from_slice(&frame.pcm_16le_bytes);
    }

    let wav = build_wav_bytes(&pcm, sample_rate, 1, 16)?;
    Ok(WavChunk { bytes: wav })
}

pub fn build_wav_bytes_raw(
    pcm_16le: &[u8],
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
) -> Result<Vec<u8>, AudioError> {
    build_wav_bytes(pcm_16le, sample_rate, channels, bits_per_sample)
}

/// Resample 16-bit little-endian PCM data from `from_rate` to `to_rate`.
///
/// Returns `(resampled_pcm, actual_output_rate)`. If resampling is not
/// supported for the given rate pair (only 48kHz→16kHz is implemented),
/// the input is returned unchanged with the original rate.
pub fn resample_pcm_16le(input: &[u8], from_rate: u32, to_rate: u32) -> (Vec<u8>, u32) {
    // Need at least 2 complete i16 samples (4 bytes) to avoid odd-byte reads,
    // and rates must differ to justify any work.
    if input.len() < 4 || from_rate == to_rate {
        return (input.to_vec(), from_rate);
    }

    // Only support 48kHz → 16kHz. The FIR coefficients are tuned for this pair.
    if from_rate != 48_000 || to_rate != 16_000 {
        return (input.to_vec(), from_rate);
    }

    let sample_count = input.len() / 2;
    if sample_count < 3 {
        return (input.to_vec(), from_rate);
    }

    // Parse i16 samples from little-endian bytes.
    let samples: Vec<f64> = (0..sample_count)
        .map(|i| i16::from_le_bytes([input[i * 2], input[i * 2 + 1]]) as f64)
        .collect();

    // Generate Blackman-windowed sinc low-pass FIR filter coefficients.
    // Cutoff at 7500 Hz (Nyquist of 16 kHz output is 8 kHz, with transition band).
    let coeffs = lowpass_fir_coefficients(RESAMPLE_FIR_TAPS, 7500.0, from_rate as f64);

    // Apply FIR filter and decimate by 3.
    let half_len = (coeffs.len() - 1) / 2;
    let output_count = sample_count / 3;
    let mut output = Vec::with_capacity(output_count * 2);

    for i in 0..output_count {
        let center = i * 3;
        let mut acc = 0.0f64;
        for (k, &coeff) in coeffs.iter().enumerate() {
            let idx = center as isize + k as isize - half_len as isize;
            let sample = if idx < 0 || idx >= samples.len() as isize {
                0.0
            } else {
                samples[idx as usize]
            };
            acc += sample * coeff;
        }
        let clamped = acc.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        output.extend_from_slice(&clamped.to_le_bytes());
    }

    (output, to_rate)
}

/// Normalize 16-bit PCM audio to a target RMS level.
///
/// `target_rms` is the desired RMS amplitude (e.g. 3000.0 for moderate volume).
/// Returns the input unchanged if it is too short or effectively silent.
pub fn normalize_rms_pcm_16le(input: &[u8], target_rms: f64) -> Vec<u8> {
    let sample_count = input.len() / 2;
    if sample_count == 0 || !target_rms.is_finite() || target_rms <= 0.0 {
        return input.to_vec();
    }

    // Calculate current RMS.
    let mut sum_sq = 0.0f64;
    for i in 0..sample_count {
        let sample = i16::from_le_bytes([input[i * 2], input[i * 2 + 1]]) as f64;
        sum_sq += sample * sample;
    }
    let current_rms = (sum_sq / sample_count as f64).sqrt();

    // Skip normalization if audio is effectively silent (RMS < 1).
    if current_rms < 1.0 {
        return input.to_vec();
    }

    let gain = target_rms / current_rms;

    let mut output = Vec::with_capacity(input.len());
    for i in 0..sample_count {
        let sample = i16::from_le_bytes([input[i * 2], input[i * 2 + 1]]) as f64;
        let normalized = (sample * gain)
            .round()
            .clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        output.extend_from_slice(&normalized.to_le_bytes());
    }
    output
}

const RESAMPLE_FIR_TAPS: usize = 45;

/// Generate a low-pass FIR filter using a Blackman-windowed sinc function.
fn lowpass_fir_coefficients(taps: usize, cutoff_hz: f64, sample_rate: f64) -> Vec<f64> {
    let m = taps - 1;
    let fc = cutoff_hz / sample_rate;
    let mut coeffs = vec![0.0f64; taps];
    let mut sum = 0.0;

    for (i, coeff) in coeffs.iter_mut().enumerate() {
        let n = i as f64;
        // Sinc function
        let sinc = if i == m / 2 {
            2.0 * fc
        } else {
            let x = 2.0 * PI * fc * (n - m as f64 / 2.0);
            x.sin() / (PI * (n - m as f64 / 2.0))
        };
        // Blackman window
        let window =
            0.42 - 0.5 * (2.0 * PI * n / m as f64).cos() + 0.08 * (4.0 * PI * n / m as f64).cos();
        *coeff = sinc * window;
        sum += *coeff;
    }

    // Normalize to unity gain at DC.
    for c in &mut coeffs {
        *c /= sum;
    }

    coeffs
}

fn build_wav_bytes(
    pcm_16le: &[u8],
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
) -> Result<Vec<u8>, AudioError> {
    // WAV uses u32 for both subchunk2_size and chunk_size (= 36 + subchunk2_size).
    // Reject PCM data that would overflow either field.
    if pcm_16le.len() > (u32::MAX - 36) as usize {
        return Err(AudioError::PcmTooLarge(pcm_16le.len()));
    }
    let byte_rate = sample_rate * channels as u32 * (bits_per_sample as u32 / 8);
    let block_align = channels * (bits_per_sample / 8);
    let subchunk2_size = pcm_16le.len() as u32;
    let chunk_size = 36 + subchunk2_size;

    let mut out = Vec::with_capacity(44 + pcm_16le.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&chunk_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");

    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());

    out.extend_from_slice(b"data");
    out.extend_from_slice(&subchunk2_size.to_le_bytes());
    out.extend_from_slice(pcm_16le);
    Ok(out)
}
