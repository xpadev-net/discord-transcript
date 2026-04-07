use crate::audio::receiver::BufferedFrame;
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
