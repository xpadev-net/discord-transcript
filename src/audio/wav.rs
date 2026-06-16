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
    PcmAssemblyTooLarge { attempted: usize, max: usize },
    UnsupportedSampleRate(u32),
    InvalidWavFormat(&'static str),
}

impl Display for AudioError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPcmLength(length) => {
                write!(
                    f,
                    "invalid PCM byte length (must align to complete sample frames): {length}"
                )
            }
            Self::PcmTooLarge(length) => {
                write!(
                    f,
                    "PCM data too large for WAV format (max ~4GB): {length} bytes"
                )
            }
            Self::PcmAssemblyTooLarge { attempted, max } => {
                write!(
                    f,
                    "PCM assembly too large: attempted {attempted} bytes (max {max})"
                )
            }
            Self::UnsupportedSampleRate(sample_rate) => {
                write!(
                    f,
                    "unsupported WAV sample rate: {sample_rate} Hz (supported {MIN_SUPPORTED_WAV_SAMPLE_RATE}..={MAX_SUPPORTED_WAV_SAMPLE_RATE} Hz)"
                )
            }
            Self::InvalidWavFormat(reason) => write!(f, "invalid WAV format: {reason}"),
        }
    }
}

impl std::error::Error for AudioError {}

pub const MAX_WAV_CHUNK_PCM_BYTES: usize = 64 * 1024 * 1024;
// Low synthetic rates are used by focused tests; the safety-critical bounds are
// rejecting zero and capping extreme headers before timeline/allocation math.
pub const MIN_SUPPORTED_WAV_SAMPLE_RATE: u32 = 1;
pub const MAX_SUPPORTED_WAV_SAMPLE_RATE: u32 = 192_000;

pub fn is_supported_wav_sample_rate(sample_rate: u32) -> bool {
    (MIN_SUPPORTED_WAV_SAMPLE_RATE..=MAX_SUPPORTED_WAV_SAMPLE_RATE).contains(&sample_rate)
}

pub fn build_wav_chunk(frames: &[BufferedFrame], sample_rate: u32) -> Result<WavChunk, AudioError> {
    let mut pcm = Vec::new();
    let mut sorted_frames = frames.iter().collect::<Vec<_>>();
    sorted_frames.sort_by_key(|frame| frame.timestamp_ms);

    let mut output_end_ms: Option<u64> = None;
    for frame in sorted_frames {
        if frame.pcm_16le_bytes.len() % 2 != 0 {
            return Err(AudioError::InvalidPcmLength(frame.pcm_16le_bytes.len()));
        }

        if let Some(end_ms) = output_end_ms
            && frame.timestamp_ms > end_ms
        {
            let gap_ms = frame.timestamp_ms - end_ms;
            let gap_bytes = pcm_byte_len_for_duration_ms(gap_ms, sample_rate).unwrap_or(usize::MAX);
            let next_len = checked_pcm_growth(pcm.len(), gap_bytes, MAX_WAV_CHUNK_PCM_BYTES)?;
            pcm.try_reserve(next_len.saturating_sub(pcm.len()))
                .map_err(|_| AudioError::PcmAssemblyTooLarge {
                    attempted: next_len,
                    max: MAX_WAV_CHUNK_PCM_BYTES,
                })?;
            pcm.resize(next_len, 0);
            output_end_ms = Some(frame.timestamp_ms);
        }

        let next_len = checked_pcm_growth(
            pcm.len(),
            frame.pcm_16le_bytes.len(),
            MAX_WAV_CHUNK_PCM_BYTES,
        )?;
        pcm.try_reserve(next_len.saturating_sub(pcm.len()))
            .map_err(|_| AudioError::PcmAssemblyTooLarge {
                attempted: next_len,
                max: MAX_WAV_CHUNK_PCM_BYTES,
            })?;
        pcm.extend_from_slice(&frame.pcm_16le_bytes);
        output_end_ms = Some(
            output_end_ms
                .unwrap_or(frame.timestamp_ms)
                .saturating_add(pcm_duration_ms(&frame.pcm_16le_bytes, sample_rate)),
        );
    }

    let wav = build_wav_bytes(&pcm, sample_rate, 1, 16)?;
    Ok(WavChunk { bytes: wav })
}

pub(crate) fn checked_pcm_growth(
    current: usize,
    additional: usize,
    max: usize,
) -> Result<usize, AudioError> {
    let Some(next) = current.checked_add(additional) else {
        return Err(AudioError::PcmAssemblyTooLarge {
            attempted: usize::MAX,
            max,
        });
    };
    if next > max {
        return Err(AudioError::PcmAssemblyTooLarge {
            attempted: next,
            max,
        });
    }
    Ok(next)
}

pub(crate) fn pcm_byte_len_for_duration_ms(duration_ms: u64, sample_rate: u32) -> Option<usize> {
    let samples = (duration_ms as u128)
        .checked_mul(sample_rate as u128)?
        .checked_div(1_000)?;
    let bytes = samples.checked_mul(2)?;
    usize::try_from(bytes).ok()
}

pub fn pcm_duration_ms(pcm_16le: &[u8], sample_rate: u32) -> u64 {
    if sample_rate == 0 {
        return 0;
    }
    let samples = pcm_16le.len() as u128 / 2;
    (samples.saturating_mul(1_000) / sample_rate as u128) as u64
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
    if !is_supported_wav_sample_rate(sample_rate) {
        return Err(AudioError::UnsupportedSampleRate(sample_rate));
    }
    if channels == 0 {
        return Err(AudioError::InvalidWavFormat(
            "channel count must be greater than zero",
        ));
    }
    if bits_per_sample == 0 || !bits_per_sample.is_multiple_of(8) {
        return Err(AudioError::InvalidWavFormat(
            "bits per sample must be a non-zero multiple of 8",
        ));
    }

    let bytes_per_sample = u32::from(bits_per_sample) / 8;
    let block_align_u32 = u32::from(channels)
        .checked_mul(bytes_per_sample)
        .ok_or(AudioError::InvalidWavFormat("block align overflow"))?;
    let block_align = u16::try_from(block_align_u32)
        .map_err(|_| AudioError::InvalidWavFormat("block align exceeds WAV header range"))?;
    let byte_rate = sample_rate
        .checked_mul(block_align_u32)
        .ok_or(AudioError::InvalidWavFormat("byte rate overflow"))?;
    if !pcm_16le.len().is_multiple_of(usize::from(block_align)) {
        return Err(AudioError::InvalidPcmLength(pcm_16le.len()));
    }
    // WAV uses u32 for both subchunk2_size and chunk_size (= 36 + subchunk2_size).
    // Reject PCM data that would overflow either field.
    if pcm_16le.len() > (u32::MAX - 36) as usize {
        return Err(AudioError::PcmTooLarge(pcm_16le.len()));
    }
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
