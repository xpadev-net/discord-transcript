use crate::audio::build_wav_bytes_raw;
use crate::audio::songbird_adapter::SsrcTracker;
use crate::audio::wav::{
    MAX_SUPPORTED_WAV_SAMPLE_RATE, MAX_WAV_CHUNK_PCM_BYTES, MIN_SUPPORTED_WAV_SAMPLE_RATE,
    checked_pcm_growth, is_supported_wav_sample_rate, pcm_byte_len_for_duration_ms,
    pcm_duration_ms, resample_pcm_16le,
};
use crate::domain::speaker::SpeakerAudioInput;
use crate::infrastructure::storage_fs::{
    decode_sanitized_path_component, sanitize_path_component, sanitize_path_component_candidates,
};
use crate::infrastructure::workspace::SSRC_MAPPING_FILENAME;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Maximum wall-clock span for a single meeting mixdown (24 hours).
pub const MAX_MEETING_AUDIO_SPAN_MS: u64 = 24 * 3600 * 1000;
pub const MAX_MEETING_AUDIO_CHUNKS: usize = 4_096;
pub const MAX_MEETING_AUDIO_INPUT_PCM_BYTES: usize = 512 * 1024 * 1024;
/// S3 final ASR retains the local mode's total raw-audio byte budget while
/// avoiding a PCM allocation proportional to this bound.
pub const MAX_S3_FINAL_ASR_BYTES: u64 =
    (MAX_MEETING_AUDIO_INPUT_PCM_BYTES as u64) + (44 * MAX_MEETING_AUDIO_CHUNKS as u64);
pub const MAX_SPEAKER_AUDIO_OUTPUTS: usize = 256;
pub const MAX_SPEAKER_AUDIO_PCM_BYTES: usize = 512 * 1024 * 1024;
const WAV_HEADER_BYTES: usize = 44;
const MAX_LOADED_WAV_FILE_BYTES: u64 = (MAX_WAV_CHUNK_PCM_BYTES + WAV_HEADER_BYTES) as u64;
const SPEAKER_BUILD_TMP_DIR: &str = ".speaker-build-tmp";

#[derive(Debug, Clone)]
pub struct LoadedChunk {
    pub user_id: String,
    pub sequence: u64,
    pub start_ms: u64,
    pub duration_ms: u64,
    pub sample_rate: u32,
    pub pcm: Vec<u8>,
    pub path: PathBuf,
}

/// Small final-ASR input metadata for one staged WAV. Unlike `LoadedChunk`,
/// this type never contains PCM and can be built without reading audio bodies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioChunkManifestEntry {
    pub speaker_id: String,
    pub sequence: u64,
    pub start_ms: u64,
    pub duration_ms: u64,
    pub sample_rate: u32,
    pub file_size_bytes: u64,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProcessedAudioChunk {
    pub speaker_id: String,
    pub sequence: u64,
    pub start_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedFilename {
    user_id: String,
    sequence: u64,
    start_ms: Option<u64>,
}

pub fn load_chunks(meeting_dir: &Path) -> Result<Vec<LoadedChunk>, String> {
    let mut chunks = Vec::new();
    let mut skipped_chunks = 0u32;
    let mut total_pcm_bytes = 0usize;
    let entries = fs::read_dir(meeting_dir).map_err(|err| {
        format!(
            "failed to read meeting dir {}: {err}",
            meeting_dir.display()
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("failed to read dir entry: {err}"))?;
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
            && path.file_stem().and_then(|s| s.to_str()) != Some("mixdown")
        {
            match load_single_chunk(&path) {
                Ok(chunk) => {
                    if chunks.len() >= MAX_MEETING_AUDIO_CHUNKS {
                        return Err(format!(
                            "too many audio chunks in {}: max {MAX_MEETING_AUDIO_CHUNKS}",
                            meeting_dir.display()
                        ));
                    }
                    total_pcm_bytes = checked_pcm_growth(
                        total_pcm_bytes,
                        chunk.pcm.len(),
                        MAX_MEETING_AUDIO_INPUT_PCM_BYTES,
                    )
                    .map_err(|err| {
                        format!(
                            "meeting audio input PCM exceeds limit in {}: {err}",
                            meeting_dir.display()
                        )
                    })?;
                    chunks.push(chunk);
                }
                Err(reason) => {
                    skipped_chunks += 1;
                    warn!(
                        meeting_dir = %meeting_dir.display(),
                        chunk_path = %path.display(),
                        skipped_chunks,
                        reason = %reason,
                        "skipping corrupt audio chunk"
                    );
                }
            }
        }
    }

    if chunks.is_empty() {
        return Err(if skipped_chunks > 0 {
            format!("no audio chunks found for meeting (skipped {skipped_chunks} corrupt chunk(s))")
        } else {
            "no audio chunks found for meeting".to_owned()
        });
    }

    if skipped_chunks > 0 {
        warn!(
            meeting_dir = %meeting_dir.display(),
            loaded_chunks = chunks.len(),
            skipped_chunks,
            "loaded meeting audio with skipped corrupt chunks"
        );
    }

    Ok(chunks)
}

/// Scans staged WAV metadata for S3 final ASR without materializing PCM.
pub fn scan_chunk_manifest(meeting_dir: &Path) -> Result<Vec<AudioChunkManifestEntry>, String> {
    let entries = fs::read_dir(meeting_dir).map_err(|err| {
        format!(
            "failed to read meeting dir {}: {err}",
            meeting_dir.display()
        )
    })?;
    let mut chunks = Vec::new();
    let mut skipped_chunks = 0u32;
    let mut total_bytes = 0u64;
    for entry in entries {
        let entry = entry.map_err(|err| format!("failed to read dir entry: {err}"))?;
        let path = entry.path();
        if path.is_dir()
            || !path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
            || path.file_stem().and_then(|stem| stem.to_str()) == Some("mixdown")
        {
            continue;
        }
        match load_chunk_metadata(&path) {
            Ok(chunk) => {
                if chunks.len() >= MAX_MEETING_AUDIO_CHUNKS {
                    return Err(format!(
                        "too many audio chunks in {}: max {MAX_MEETING_AUDIO_CHUNKS}",
                        meeting_dir.display()
                    ));
                }
                total_bytes = total_bytes
                    .checked_add(chunk.file_size_bytes)
                    .ok_or_else(|| "S3 final ASR audio size overflow".to_owned())?;
                if total_bytes > MAX_S3_FINAL_ASR_BYTES {
                    return Err(format!(
                        "S3 final ASR audio exceeds byte limit: {total_bytes} (max {MAX_S3_FINAL_ASR_BYTES})"
                    ));
                }
                chunks.push(chunk);
            }
            Err(reason) => {
                skipped_chunks += 1;
                warn!(
                    meeting_dir = %meeting_dir.display(),
                    chunk_path = %path.display(),
                    skipped_chunks,
                    reason = %reason,
                    "skipping corrupt audio chunk in S3 manifest"
                );
            }
        }
    }
    if chunks.is_empty() {
        return Err(if skipped_chunks > 0 {
            format!("no audio chunks found for meeting (skipped {skipped_chunks} corrupt chunk(s))")
        } else {
            "no audio chunks found for meeting".to_owned()
        });
    }
    let meeting_start_ms = chunks.iter().map(|chunk| chunk.start_ms).min().unwrap_or(0);
    let meeting_end_ms = chunks
        .iter()
        .map(|chunk| chunk.start_ms.saturating_add(chunk.duration_ms))
        .max()
        .unwrap_or(meeting_start_ms);
    if meeting_end_ms.saturating_sub(meeting_start_ms) > MAX_MEETING_AUDIO_SPAN_MS {
        return Err(format!(
            "S3 final ASR audio span exceeds limit: {} ms (max {MAX_MEETING_AUDIO_SPAN_MS})",
            meeting_end_ms.saturating_sub(meeting_start_ms)
        ));
    }
    let ssrc_mapping = load_ssrc_mapping(meeting_dir);
    for chunk in &mut chunks {
        if let Some(real_speaker_id) = ssrc_mapping.get(&chunk.speaker_id) {
            chunk.speaker_id.clone_from(real_speaker_id);
        }
    }
    let speaker_count = chunks
        .iter()
        .map(|chunk| chunk.speaker_id.as_str())
        .collect::<HashSet<_>>()
        .len();
    if speaker_count > MAX_SPEAKER_AUDIO_OUTPUTS {
        return Err(format!(
            "too many speaker audio outputs in {}: {} (max {MAX_SPEAKER_AUDIO_OUTPUTS})",
            meeting_dir.display(),
            speaker_count
        ));
    }
    chunks.sort_by(|left, right| {
        left.start_ms
            .cmp(&right.start_ms)
            .then(left.speaker_id.cmp(&right.speaker_id))
            .then(left.sequence.cmp(&right.sequence))
    });
    Ok(chunks)
}

/// Creates one bounded ASR input per raw chunk, keeping the meeting timeline
/// metadata instead of assembling speaker PCM in the bot process.
pub fn speaker_audio_inputs_from_manifest(
    chunks: &[AudioChunkManifestEntry],
    processed_chunks: &[ProcessedAudioChunk],
) -> Vec<SpeakerAudioInput> {
    let meeting_start_ms = manifest_meeting_start_ms(chunks);
    let ssrc_mapping = chunks
        .first()
        .and_then(|chunk| chunk.path.parent())
        .map(load_ssrc_mapping)
        .unwrap_or_default();
    let processed = processed_chunks
        .iter()
        .map(|chunk| {
            let speaker_id = ssrc_mapping
                .get(&chunk.speaker_id)
                .cloned()
                .or_else(|| {
                    SsrcTracker::parse_ssrc_fallback(&chunk.speaker_id).and_then(|ssrc| {
                        ssrc_mapping.get(&SsrcTracker::fallback_key(ssrc)).cloned()
                    })
                })
                .unwrap_or_else(|| chunk.speaker_id.clone());
            (speaker_id, chunk.sequence, chunk.start_ms)
        })
        .collect::<HashSet<_>>();
    chunks
        .iter()
        .filter(|chunk| {
            !processed.contains(&(chunk.speaker_id.clone(), chunk.sequence, chunk.start_ms))
        })
        .map(|chunk| SpeakerAudioInput {
            speaker_id: chunk.speaker_id.clone(),
            audio_path: chunk.path.to_string_lossy().into_owned(),
            offset_ms: chunk.start_ms.saturating_sub(meeting_start_ms),
        })
        .collect()
}

/// Prepares at most one raw chunk's PCM at a time; S3 final ASR never joins
/// samples across chunks in the bot process.
pub fn prepare_manifest_asr_inputs(
    chunks: &[AudioChunkManifestEntry],
    processed_chunks: &[ProcessedAudioChunk],
    resample_to_16k: bool,
) -> Result<Vec<SpeakerAudioInput>, String> {
    let meeting_start_ms = manifest_meeting_start_ms(chunks);
    let entries = chunks
        .iter()
        .map(|chunk| (chunk.path.as_path(), chunk))
        .collect::<HashMap<_, _>>();
    let mut inputs = speaker_audio_inputs_from_manifest(chunks, processed_chunks);
    inputs.sort_by(|left, right| {
        left.speaker_id
            .cmp(&right.speaker_id)
            .then(left.offset_ms.cmp(&right.offset_ms))
            .then(left.audio_path.cmp(&right.audio_path))
    });

    let mut speaker_ends = HashMap::<String, u64>::new();
    let mut output_dir: Option<PathBuf> = None;
    let mut prepared = Vec::with_capacity(inputs.len());
    for mut input in inputs {
        let input_path = Path::new(&input.audio_path);
        let entry = entries
            .get(input_path)
            .ok_or_else(|| format!("manifest entry missing for {}", input.audio_path))?;
        let chunk_end_ms = entry.start_ms.saturating_add(entry.duration_ms);
        let speaker_end_ms = speaker_ends.entry(input.speaker_id.clone()).or_default();
        let overlap_ms = speaker_end_ms.saturating_sub(entry.start_ms);
        if overlap_ms >= entry.duration_ms && overlap_ms > 0 {
            debug!(
                speaker_id = %input.speaker_id,
                sequence = entry.sequence,
                "skipping fully overlapped manifest chunk for final ASR"
            );
            continue;
        }

        let needs_trim = overlap_ms > 0;
        let needs_resample = resample_to_16k && entry.sample_rate != 16_000;
        if needs_trim || needs_resample {
            let (_, mut pcm) = read_wav_pcm(input_path)?;
            let mut effective_start_ms = entry.start_ms;
            if needs_trim {
                let samples_to_skip = overlap_ms.saturating_mul(entry.sample_rate as u64) / 1_000;
                let bytes_to_skip = samples_to_skip.saturating_mul(2) as usize;
                if bytes_to_skip >= pcm.len() {
                    continue;
                }
                pcm.drain(..bytes_to_skip);
                effective_start_ms = effective_start_ms.saturating_add(
                    samples_to_skip.saturating_mul(1_000) / entry.sample_rate as u64,
                );
                debug!(
                    speaker_id = %input.speaker_id,
                    sequence = entry.sequence,
                    overlap_ms,
                    "trimming overlapping manifest chunk for final ASR"
                );
            }
            let (pcm, output_rate) = if needs_resample {
                resample_pcm_16le(&pcm, entry.sample_rate, 16_000)
            } else {
                (pcm, entry.sample_rate)
            };
            if needs_resample && output_rate != 16_000 {
                return Err(format!(
                    "failed to resample manifest chunk {} to 16000 Hz",
                    input_path.display()
                ));
            }
            let dir = match &output_dir {
                Some(dir) => dir.clone(),
                None => {
                    let dir = input_path
                        .parent()
                        .ok_or_else(|| "manifest WAV path has no parent directory".to_owned())?
                        .join("transcription_chunks");
                    fs::create_dir_all(&dir)
                        .map_err(|err| format!("failed to create {}: {err}", dir.display()))?;
                    output_dir = Some(dir.clone());
                    dir
                }
            };
            let file_name = input_path
                .file_name()
                .ok_or_else(|| "manifest WAV path has no file name".to_owned())?;
            let output_path = dir.join(format!("{}.asr.wav", file_name.to_string_lossy()));
            let wav = build_wav_bytes_raw(&pcm, output_rate, 1, 16)
                .map_err(|err| format!("failed to encode prepared chunk: {err}"))?;
            fs::write(&output_path, wav)
                .map_err(|err| format!("failed to write {}: {err}", output_path.display()))?;
            input.audio_path = output_path.to_string_lossy().into_owned();
            input.offset_ms = effective_start_ms.saturating_sub(meeting_start_ms);
        }
        *speaker_end_ms = (*speaker_end_ms).max(chunk_end_ms);
        prepared.push(input);
    }
    Ok(prepared)
}

pub fn manifest_meeting_start_ms(chunks: &[AudioChunkManifestEntry]) -> u64 {
    chunks
        .iter()
        .map(|chunk| chunk.start_ms)
        .filter(|start_ms| *start_ms > 0)
        .min()
        .or_else(|| chunks.iter().map(|chunk| chunk.start_ms).min())
        .unwrap_or(0)
}

fn load_chunk_metadata(path: &Path) -> Result<AudioChunkManifestEntry, String> {
    let metadata = path
        .metadata()
        .map_err(|err| format!("failed to stat {}: {err}", path.display()))?;
    let file_size_bytes = metadata.len();
    if file_size_bytes > MAX_LOADED_WAV_FILE_BYTES {
        return Err(format!(
            "WAV file too large in {}: {file_size_bytes} bytes (max {MAX_LOADED_WAV_FILE_BYTES})",
            path.display()
        ));
    }
    let mut file =
        fs::File::open(path).map_err(|err| format!("failed to open {}: {err}", path.display()))?;
    let mut header = [0u8; WAV_HEADER_BYTES];
    file.read_exact(&mut header)
        .map_err(|err| format!("failed to read WAV header {}: {err}", path.display()))?;
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" || &header[12..16] != b"fmt " {
        return Err(format!("invalid PCM WAV header in {}", path.display()));
    }
    let riff_size = u32::from_le_bytes(header[4..8].try_into().unwrap()) as u64;
    let fmt_size = u32::from_le_bytes(header[16..20].try_into().unwrap());
    let audio_format = u16::from_le_bytes(header[20..22].try_into().unwrap());
    let channels = u16::from_le_bytes(header[22..24].try_into().unwrap());
    let sample_rate = u32::from_le_bytes(header[24..28].try_into().unwrap());
    let byte_rate = u32::from_le_bytes(header[28..32].try_into().unwrap());
    let block_align = u16::from_le_bytes(header[32..34].try_into().unwrap());
    let bits_per_sample = u16::from_le_bytes(header[34..36].try_into().unwrap());
    let data_size = u32::from_le_bytes(header[40..44].try_into().unwrap()) as u64;
    if fmt_size != 16
        || audio_format != 1
        || channels != 1
        || bits_per_sample != 16
        || !is_supported_wav_sample_rate(sample_rate)
        || block_align != 2
        || byte_rate != sample_rate.saturating_mul(2)
        || &header[36..40] != b"data"
        || data_size == 0
        || data_size > MAX_WAV_CHUNK_PCM_BYTES as u64
        || !data_size.is_multiple_of(u64::from(block_align))
        || riff_size < 36u64.saturating_add(data_size)
        || 8u64.saturating_add(riff_size) > file_size_bytes
        || 44u64.saturating_add(data_size) > file_size_bytes
    {
        return Err(format!(
            "invalid or unsupported PCM WAV metadata in {}",
            path.display()
        ));
    }
    let duration_ms = (u128::from(data_size) * 1_000 / (u128::from(sample_rate) * 2)) as u64;
    let parsed = parse_chunk_filename(path)?;
    let start_ms = parsed
        .start_ms
        .unwrap_or_else(|| fallback_start_ms(path, duration_ms));
    Ok(AudioChunkManifestEntry {
        speaker_id: parsed.user_id,
        sequence: parsed.sequence,
        start_ms,
        duration_ms,
        sample_rate,
        file_size_bytes,
        path: path.to_path_buf(),
    })
}

fn load_single_chunk(path: &Path) -> Result<LoadedChunk, String> {
    let parsed = parse_chunk_filename(path)?;
    let (sample_rate, pcm) = read_wav_pcm(path)?;
    let duration_ms = pcm_duration_ms(&pcm, sample_rate);
    let start_ms = parsed
        .start_ms
        .unwrap_or_else(|| fallback_start_ms(path, duration_ms));
    Ok(LoadedChunk {
        user_id: parsed.user_id,
        sequence: parsed.sequence,
        start_ms,
        duration_ms,
        sample_rate,
        pcm,
        path: path.to_path_buf(),
    })
}

fn parse_chunk_filename(path: &Path) -> Result<ParsedFilename, String> {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("invalid chunk filename: {}", path.display()))?;

    // Accept formats:
    // - user_seq.wav
    // - user_seq_start.wav
    let mut parts = stem.rsplitn(3, '_').collect::<Vec<_>>();
    parts.reverse();

    let (user_part, seq_part, start_part) = match parts.as_slice() {
        [user, seq] => (*user, Some(*seq), None),
        [user, seq, start] => (*user, Some(*seq), Some(*start)),
        _ => (stem, None, None),
    };

    let sequence = seq_part.and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    let start_ms = start_part.and_then(|s| s.parse::<u64>().ok());

    Ok(ParsedFilename {
        user_id: decode_sanitized_path_component(user_part).unwrap_or_else(|| user_part.to_owned()),
        sequence,
        start_ms,
    })
}

fn read_wav_pcm(path: &Path) -> Result<(u32, Vec<u8>), String> {
    let file_size = path
        .metadata()
        .map_err(|err| format!("failed to stat {}: {err}", path.display()))?
        .len();
    if file_size > MAX_LOADED_WAV_FILE_BYTES {
        return Err(format!(
            "WAV file too large in {}: {file_size} bytes (max {MAX_LOADED_WAV_FILE_BYTES})",
            path.display()
        ));
    }

    let data = fs::read(path).map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    if data.len() < 44 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err(format!(
            "invalid WAV header for {} (too small or missing RIFF/WAVE)",
            path.display()
        ));
    }

    let riff_chunk_size = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let riff_end = 8usize
        .checked_add(riff_chunk_size)
        .ok_or_else(|| format!("invalid RIFF chunk size in {}", path.display()))?;
    if riff_end > data.len() {
        return Err(format!(
            "truncated RIFF chunk in {}: declared {riff_chunk_size} bytes, file has {} bytes after RIFF header",
            path.display(),
            data.len().saturating_sub(8)
        ));
    }
    if &data[12..16] != b"fmt " {
        return Err(format!(
            "missing fmt chunk in WAV header for {}",
            path.display()
        ));
    }
    let fmt_chunk_size = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);
    if fmt_chunk_size != 16 {
        return Err(format!(
            "unsupported WAV fmt chunk size for {}: {fmt_chunk_size}",
            path.display()
        ));
    }

    let audio_format = u16::from_le_bytes([data[20], data[21]]);
    let channels = u16::from_le_bytes([data[22], data[23]]);
    let sample_rate = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);
    let byte_rate = u32::from_le_bytes([data[28], data[29], data[30], data[31]]);
    let block_align = u16::from_le_bytes([data[32], data[33]]);
    let bits_per_sample = u16::from_le_bytes([data[34], data[35]]);
    if audio_format != 1 {
        return Err(format!(
            "unsupported WAV audio format for {}: format={audio_format}",
            path.display()
        ));
    }
    if channels != 1 || bits_per_sample != 16 {
        return Err(format!(
            "unsupported WAV format for {}: channels={}, bits_per_sample={}",
            path.display(),
            channels,
            bits_per_sample
        ));
    }
    if !is_supported_wav_sample_rate(sample_rate) {
        return Err(format!(
            "unsupported WAV sample rate for {}: sample_rate={} (supported {}..={} Hz)",
            path.display(),
            sample_rate,
            MIN_SUPPORTED_WAV_SAMPLE_RATE,
            MAX_SUPPORTED_WAV_SAMPLE_RATE
        ));
    }
    let expected_block_align = channels * (bits_per_sample / 8);
    if block_align != expected_block_align {
        return Err(format!(
            "inconsistent WAV block align for {}: block_align={}, expected={expected_block_align}",
            path.display(),
            block_align
        ));
    }
    let expected_byte_rate = sample_rate
        .checked_mul(u32::from(expected_block_align))
        .ok_or_else(|| format!("invalid byte rate in {}", path.display()))?;
    if byte_rate != expected_byte_rate {
        return Err(format!(
            "inconsistent WAV byte rate for {}: byte_rate={}, expected={expected_byte_rate}",
            path.display(),
            byte_rate
        ));
    }
    if &data[36..40] != b"data" {
        return Err(format!(
            "missing data chunk in WAV header for {}",
            path.display()
        ));
    }
    let data_chunk_size = u32::from_le_bytes([data[40], data[41], data[42], data[43]]) as usize;
    if data_chunk_size > MAX_WAV_CHUNK_PCM_BYTES {
        return Err(format!(
            "PCM data too large in {}: {data_chunk_size} bytes (max {MAX_WAV_CHUNK_PCM_BYTES})",
            path.display()
        ));
    }
    let minimum_riff_chunk_size = 36usize + data_chunk_size;
    if riff_chunk_size < minimum_riff_chunk_size {
        return Err(format!(
            "inconsistent WAV chunk size in {}: riff_chunk_size={}, expected at least {minimum_riff_chunk_size}",
            path.display(),
            riff_chunk_size
        ));
    }
    if data_chunk_size == 0 {
        return Err(format!("empty PCM data chunk in {}", path.display()));
    }
    if !data_chunk_size.is_multiple_of(usize::from(block_align)) {
        return Err(format!(
            "invalid PCM byte length in {}: {data_chunk_size}",
            path.display()
        ));
    }
    let pcm_start = 44usize;
    let pcm_end = pcm_start
        .checked_add(data_chunk_size)
        .ok_or_else(|| format!("invalid PCM data size in {}", path.display()))?;
    if pcm_end > data.len() {
        return Err(format!(
            "truncated PCM data in {}: expected {data_chunk_size} bytes, found {}",
            path.display(),
            data.len().saturating_sub(pcm_start)
        ));
    }
    Ok((sample_rate, data[pcm_start..pcm_end].to_vec()))
}

fn fallback_start_ms(path: &Path, duration_ms: u64) -> u64 {
    match path.metadata().and_then(|m| m.modified()) {
        Ok(modified) => match modified.duration_since(std::time::UNIX_EPOCH) {
            Ok(dur) => dur
                .as_millis()
                .saturating_sub(duration_ms as u128)
                .try_into()
                .unwrap_or(0),
            Err(_) => 0,
        },
        Err(_) => 0,
    }
}

pub(crate) fn compute_meeting_start_ms(chunks: &[LoadedChunk]) -> u64 {
    chunks
        .iter()
        .map(|c| c.start_ms)
        .filter(|value| *value > 0)
        .min()
        .or_else(|| chunks.iter().map(|c| c.start_ms).min())
        .unwrap_or(0)
}

fn append_pcm_bounded(
    pcm_out: &mut Vec<u8>,
    bytes: &[u8],
    max: usize,
    context: &str,
) -> Result<(), String> {
    let next_len = checked_pcm_growth(pcm_out.len(), bytes.len(), max)
        .map_err(|err| format!("{context} exceeds PCM limit: {err}"))?;
    pcm_out
        .try_reserve(next_len.saturating_sub(pcm_out.len()))
        .map_err(|_| format!("{context} exceeds PCM limit: unable to reserve {next_len} bytes"))?;
    pcm_out.extend_from_slice(bytes);
    Ok(())
}

fn append_silence_bounded(
    pcm_out: &mut Vec<u8>,
    duration_ms: u64,
    sample_rate: u32,
    max: usize,
    context: &str,
) -> Result<(), String> {
    let silence_bytes =
        pcm_byte_len_for_duration_ms(duration_ms, sample_rate).unwrap_or(usize::MAX);
    let next_len = checked_pcm_growth(pcm_out.len(), silence_bytes, max)
        .map_err(|err| format!("{context} exceeds PCM limit: {err}"))?;
    pcm_out
        .try_reserve(next_len.saturating_sub(pcm_out.len()))
        .map_err(|_| format!("{context} exceeds PCM limit: unable to reserve {next_len} bytes"))?;
    pcm_out.resize(next_len, 0);
    Ok(())
}

/// Load the persisted SSRC-to-user mapping and build a lookup from sanitized
/// SSRC fallback filenames to real user IDs.
fn load_ssrc_mapping(meeting_dir: &Path) -> HashMap<String, String> {
    let mapping_path = meeting_dir.join(SSRC_MAPPING_FILENAME);
    let data = match fs::read(&mapping_path) {
        Ok(data) => data,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return HashMap::new(),
        Err(err) => {
            warn!(
                path = %mapping_path.display(),
                error = %err,
                "failed to read SSRC mapping file"
            );
            return HashMap::new();
        }
    };
    let tracker: SsrcTracker = match serde_json::from_slice(&data) {
        Ok(parsed) => parsed,
        Err(err) => {
            warn!(
                path = %mapping_path.display(),
                error = %err,
                "failed to parse SSRC mapping"
            );
            return HashMap::new();
        }
    };
    let mut lookup = HashMap::new();
    for (ssrc, user_id) in tracker.all_mappings() {
        let fallback_key = SsrcTracker::fallback_key(*ssrc);
        lookup.insert(fallback_key.clone(), user_id.clone());
        for sanitized_key in sanitize_path_component_candidates(&fallback_key) {
            lookup.insert(sanitized_key, user_id.clone());
        }
    }
    lookup
}

/// Directory under the audio dir that holds complete per-speaker playback
/// wavs (served by the speaker playback API).
const SPEAKER_PLAYBACK_DIR: &str = "speakers";
/// Directory that holds partial per-speaker wavs used only as transcription
/// inputs once live transcription already covered some chunks — kept out of
/// `speakers/` so partial audio never clobbers the playback copy.
const SPEAKER_TRANSCRIPTION_DIR: &str = "transcription_speakers";

pub fn build_speaker_audio_inputs(
    meeting_dir: &Path,
    resample_to_16k: bool,
) -> Result<Vec<SpeakerAudioInput>, String> {
    build_speaker_audio_inputs_with_dir(meeting_dir, resample_to_16k, &[], SPEAKER_PLAYBACK_DIR)
}

pub fn build_speaker_audio_inputs_excluding_processed_chunks(
    meeting_dir: &Path,
    resample_to_16k: bool,
    processed_chunks: &[ProcessedAudioChunk],
) -> Result<Vec<SpeakerAudioInput>, String> {
    build_speaker_audio_inputs_with_dir(
        meeting_dir,
        resample_to_16k,
        processed_chunks,
        SPEAKER_TRANSCRIPTION_DIR,
    )
}

fn build_speaker_audio_inputs_with_dir(
    meeting_dir: &Path,
    resample_to_16k: bool,
    processed_chunks: &[ProcessedAudioChunk],
    speaker_dir_name: &str,
) -> Result<Vec<SpeakerAudioInput>, String> {
    let mut chunks = load_chunks(meeting_dir)?;

    // Resolve any SSRC-based user IDs using persisted mapping
    let ssrc_mapping = load_ssrc_mapping(meeting_dir);
    let mut unresolved_fallback = 0u32;
    for chunk in &mut chunks {
        if let Some(real_id) = ssrc_mapping.get(&chunk.user_id) {
            chunk.user_id = real_id.clone();
        } else if SsrcTracker::parse_ssrc_fallback(&chunk.user_id).is_some() {
            unresolved_fallback += 1;
        }
    }

    if unresolved_fallback > 0 {
        warn!(
            meeting_dir = %meeting_dir.display(),
            unresolved_chunks = unresolved_fallback,
            "speaker audio build encountered unresolved SSRC fallback IDs; resulting filenames may be anonymized"
        );
    }

    let meeting_start_ms = compute_meeting_start_ms(&chunks);

    if !processed_chunks.is_empty() {
        let processed = processed_chunks
            .iter()
            .map(|chunk| (chunk.speaker_id.as_str(), chunk.sequence, chunk.start_ms))
            .collect::<std::collections::HashSet<_>>();
        let before = chunks.len();
        chunks.retain(|chunk| {
            !processed.contains(&(chunk.user_id.as_str(), chunk.sequence, chunk.start_ms))
        });
        let skipped = before.saturating_sub(chunks.len());
        if skipped > 0 {
            debug!(
                meeting_dir = %meeting_dir.display(),
                skipped_chunks = skipped,
                "skipping chunks already completed by live transcription"
            );
        }
    }

    if chunks.is_empty() {
        return Ok(Vec::new());
    }

    let sample_rate = chunks.first().map(|c| c.sample_rate).unwrap_or(48_000);
    if chunks.iter().any(|c| c.sample_rate != sample_rate) {
        return Err("mixed sample rates are not supported".to_owned());
    }

    let mut per_user: HashMap<String, Vec<LoadedChunk>> = HashMap::new();
    for chunk in chunks {
        per_user
            .entry(chunk.user_id.clone())
            .or_default()
            .push(chunk);
    }

    if per_user.len() > MAX_SPEAKER_AUDIO_OUTPUTS {
        return Err(format!(
            "too many speaker audio outputs in {}: {} (max {MAX_SPEAKER_AUDIO_OUTPUTS})",
            meeting_dir.display(),
            per_user.len()
        ));
    }

    let speaker_dir = meeting_dir.join(speaker_dir_name);
    fs::create_dir_all(&speaker_dir).map_err(|err| {
        format!(
            "failed to create speaker dir {}: {err}",
            speaker_dir.display()
        )
    })?;
    let speaker_tmp_dir = speaker_dir.join(SPEAKER_BUILD_TMP_DIR);
    if speaker_tmp_dir.exists() {
        fs::remove_dir_all(&speaker_tmp_dir).map_err(|err| {
            format!(
                "failed to remove stale speaker temp dir {}: {err}",
                speaker_tmp_dir.display()
            )
        })?;
    }
    fs::create_dir_all(&speaker_tmp_dir).map_err(|err| {
        format!(
            "failed to create speaker temp dir {}: {err}",
            speaker_tmp_dir.display()
        )
    })?;

    let mut outputs = Vec::new();
    let mut generated_speaker_files = HashSet::new();
    let mut staged_speaker_files = Vec::new();
    for (user_id, mut user_chunks) in per_user {
        user_chunks.sort_by(|a, b| {
            a.start_ms
                .cmp(&b.start_ms)
                .then(a.sequence.cmp(&b.sequence))
        });
        let Some(first) = user_chunks.first() else {
            continue;
        };

        let mut pcm_out = Vec::new();
        let mut current_ms = first.start_ms.saturating_add(first.duration_ms);
        append_pcm_bounded(
            &mut pcm_out,
            &first.pcm,
            MAX_SPEAKER_AUDIO_PCM_BYTES,
            "speaker audio assembly",
        )?;
        for chunk in user_chunks.iter().skip(1) {
            if chunk.start_ms > current_ms {
                let gap_ms = chunk.start_ms - current_ms;
                append_silence_bounded(
                    &mut pcm_out,
                    gap_ms,
                    sample_rate,
                    MAX_SPEAKER_AUDIO_PCM_BYTES,
                    "speaker audio assembly",
                )?;
                current_ms = chunk.start_ms;
            }
            let chunk_pcm = &chunk.pcm;
            if chunk.start_ms < current_ms {
                let overlap_ms = current_ms - chunk.start_ms;
                let samples_to_skip =
                    overlap_ms.saturating_mul(sample_rate as u64) as u128 / 1_000u128;
                let bytes_to_skip = samples_to_skip.saturating_mul(2) as usize;
                if bytes_to_skip >= chunk_pcm.len() {
                    debug!(
                        user_id = %chunk.user_id,
                        sequence = chunk.sequence,
                        start_ms = chunk.start_ms,
                        current_ms,
                        "skipping fully overlapped chunk while stitching speaker audio"
                    );
                    continue;
                }
                debug!(
                    user_id = %chunk.user_id,
                    sequence = chunk.sequence,
                    overlap_ms,
                    "trimming overlapping chunk while stitching speaker audio"
                );
                let trimmed = &chunk_pcm[bytes_to_skip..];
                append_pcm_bounded(
                    &mut pcm_out,
                    trimmed,
                    MAX_SPEAKER_AUDIO_PCM_BYTES,
                    "speaker audio assembly",
                )?;
                current_ms = current_ms.saturating_add(pcm_duration_ms(trimmed, sample_rate));
                continue;
            }
            append_pcm_bounded(
                &mut pcm_out,
                chunk_pcm,
                MAX_SPEAKER_AUDIO_PCM_BYTES,
                "speaker audio assembly",
            )?;
            current_ms = chunk.start_ms.saturating_add(chunk.duration_ms);
        }
        let (final_pcm, final_rate) = if resample_to_16k {
            let (resampled, rate) = resample_pcm_16le(&pcm_out, sample_rate, 16_000);
            if rate != 16_000 {
                warn!(
                    user_id = %user_id,
                    sample_rate,
                    "resampling skipped: unsupported sample rate (expected 48000)"
                );
            }
            (resampled, rate)
        } else {
            (pcm_out, sample_rate)
        };
        let wav_bytes = build_wav_bytes_raw(&final_pcm, final_rate, 1, 16)
            .map_err(|err| format!("failed to build speaker wav for {user_id}: {err}"))?;
        let safe_user = sanitize_path_component(&user_id);
        let output_file_name = format!("{safe_user}_speaker.wav");
        if generated_speaker_files.contains(&output_file_name) {
            return Err(format!(
                "speaker audio output filename collision for sanitized user id: {safe_user}"
            ));
        }
        let output_path = speaker_dir.join(&output_file_name);
        let staged_path = speaker_tmp_dir.join(&output_file_name);
        fs::write(&staged_path, &wav_bytes)
            .map_err(|err| format!("failed to write speaker audio for {user_id}: {err}"))?;
        generated_speaker_files.insert(output_file_name);
        staged_speaker_files.push((staged_path, output_path.clone()));

        outputs.push(SpeakerAudioInput {
            speaker_id: user_id,
            audio_path: output_path.to_string_lossy().to_string(),
            offset_ms: first.start_ms.saturating_sub(meeting_start_ms),
        });
    }

    for (staged_path, output_path) in &staged_speaker_files {
        fs::rename(staged_path, output_path).map_err(|err| {
            format!(
                "failed to promote speaker audio {} to {}: {err}",
                staged_path.display(),
                output_path.display()
            )
        })?;
    }
    fs::remove_dir_all(&speaker_tmp_dir).map_err(|err| {
        format!(
            "failed to remove speaker temp dir {}: {err}",
            speaker_tmp_dir.display()
        )
    })?;
    remove_stale_speaker_wavs(&speaker_dir, &generated_speaker_files)?;

    outputs.sort_by(|a, b| a.speaker_id.cmp(&b.speaker_id));
    Ok(outputs)
}

fn remove_stale_speaker_wavs(
    speaker_dir: &Path,
    generated_speaker_files: &HashSet<String>,
) -> Result<(), String> {
    let entries = fs::read_dir(speaker_dir).map_err(|err| {
        format!(
            "failed to read speaker dir {}: {err}",
            speaker_dir.display()
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("failed to read speaker dir entry: {err}"))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if file_name.ends_with("_speaker.wav") && !generated_speaker_files.contains(file_name) {
            fs::remove_file(&path).map_err(|err| {
                format!(
                    "failed to remove stale speaker wav {}: {err}",
                    path.display()
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod manifest_tests {
    use super::*;
    use std::io::{Seek, Write};

    fn write_sparse_wav(path: &Path, pcm_bytes: u32, sample_rate: u32) {
        let mut header = [0u8; WAV_HEADER_BYTES];
        header[0..4].copy_from_slice(b"RIFF");
        header[4..8].copy_from_slice(&(36u32 + pcm_bytes).to_le_bytes());
        header[8..12].copy_from_slice(b"WAVE");
        header[12..16].copy_from_slice(b"fmt ");
        header[16..20].copy_from_slice(&16u32.to_le_bytes());
        header[20..22].copy_from_slice(&1u16.to_le_bytes());
        header[22..24].copy_from_slice(&1u16.to_le_bytes());
        header[24..28].copy_from_slice(&sample_rate.to_le_bytes());
        header[28..32].copy_from_slice(&sample_rate.saturating_mul(2).to_le_bytes());
        header[32..34].copy_from_slice(&2u16.to_le_bytes());
        header[34..36].copy_from_slice(&16u16.to_le_bytes());
        header[36..40].copy_from_slice(b"data");
        header[40..44].copy_from_slice(&pcm_bytes.to_le_bytes());
        let mut file = fs::File::create(path).expect("fixture WAV should be created");
        file.write_all(&header)
            .expect("WAV header should be written");
        file.set_len(44 + u64::from(pcm_bytes))
            .expect("sparse WAV body should be sized");
    }

    #[test]
    fn s3_manifest_scans_large_audio_using_only_wav_metadata() {
        let root = std::env::temp_dir().join(format!(
            "discord_transcript_manifest_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("fixture directory should be created");
        let path = root.join("speaker_3_12000.wav");
        let pcm_bytes = 60 * 1024 * 1024;
        write_sparse_wav(&path, pcm_bytes, 48_000);

        let chunks = scan_chunk_manifest(&root).expect("metadata scan should succeed");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].speaker_id, "speaker");
        assert_eq!(chunks[0].sequence, 3);
        assert_eq!(chunks[0].start_ms, 12_000);
        assert_eq!(chunks[0].duration_ms, u64::from(pcm_bytes) * 1_000 / 96_000);
        assert_eq!(chunks[0].file_size_bytes, 44 + u64::from(pcm_bytes));
        let inputs = speaker_audio_inputs_from_manifest(&chunks, &[]);
        assert_eq!(inputs[0].speaker_id, "speaker");
        assert_eq!(inputs[0].offset_ms, 0);
        let completed = [ProcessedAudioChunk {
            speaker_id: "speaker".to_owned(),
            sequence: 3,
            start_ms: 12_000,
        }];
        assert!(speaker_audio_inputs_from_manifest(&chunks, &completed).is_empty());
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn manifest_filters_live_chunks_after_ssrc_mapping_resolves_speaker() {
        let root = std::env::temp_dir().join(format!(
            "discord_transcript_manifest_ssrc_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("fixture directory should be created");
        let path = root.join("ssrc%3A13829_3_12000.wav");
        write_sparse_wav(&path, 96_000, 48_000);
        fs::write(
            root.join(SSRC_MAPPING_FILENAME),
            br#"{"ssrc_to_user":{"13829":"user-42"}}"#,
        )
        .expect("SSRC mapping should be written");

        let chunks = scan_chunk_manifest(&root).expect("manifest scan should succeed");
        assert_eq!(chunks[0].speaker_id, "user-42");
        let processed = [ProcessedAudioChunk {
            speaker_id: "ssrc:13829".to_owned(),
            sequence: 3,
            start_ms: 12_000,
        }];
        assert!(speaker_audio_inputs_from_manifest(&chunks, &processed).is_empty());
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn manifest_asr_inputs_resample_each_chunk_to_configured_rate() {
        let root = std::env::temp_dir().join(format!(
            "discord_transcript_manifest_resample_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("fixture directory should be created");
        let path = root.join("speaker_1_1000.wav");
        write_sparse_wav(&path, 96_000, 48_000);

        let chunks = scan_chunk_manifest(&root).expect("metadata scan should succeed");
        let inputs = prepare_manifest_asr_inputs(&chunks, &[], true)
            .expect("single chunk resampling should succeed");
        assert_eq!(inputs.len(), 1);
        assert_eq!(
            load_chunk_metadata(Path::new(&inputs[0].audio_path))
                .expect("resampled WAV should be valid")
                .sample_rate,
            16_000
        );
        assert!(Path::new(&inputs[0].audio_path).starts_with(root.join("transcription_chunks")));
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn manifest_skips_riff_body_larger_than_staged_file() {
        let root = std::env::temp_dir().join(format!(
            "discord_transcript_manifest_corrupt_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("fixture directory should be created");
        let path = root.join("speaker_1_1000.wav");
        write_sparse_wav(&path, 2_000, 16_000);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("fixture should open");
        file.seek(std::io::SeekFrom::Start(4))
            .expect("RIFF size should be seekable");
        file.write_all(&10_000u32.to_le_bytes())
            .expect("invalid RIFF size should be written");

        assert!(scan_chunk_manifest(&root).is_err());
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn manifest_asr_inputs_trim_and_skip_same_speaker_overlaps() {
        let root = std::env::temp_dir().join(format!(
            "discord_transcript_manifest_overlap_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("fixture directory should be created");
        write_sparse_wav(&root.join("speaker_1_1000.wav"), 32_000, 16_000);
        write_sparse_wav(&root.join("speaker_2_1500.wav"), 32_000, 16_000);
        write_sparse_wav(&root.join("speaker_3_1800.wav"), 3_200, 16_000);

        let chunks = scan_chunk_manifest(&root).expect("metadata scan should succeed");
        let inputs = prepare_manifest_asr_inputs(&chunks, &[], false)
            .expect("overlapping chunks should be prepared");
        assert_eq!(inputs.len(), 2, "fully overlapped chunk should be skipped");
        assert_eq!(inputs[0].offset_ms, 0);
        assert_eq!(inputs[1].offset_ms, 1_000);
        assert!(Path::new(&inputs[1].audio_path).starts_with(root.join("transcription_chunks")));
        let trimmed = load_chunk_metadata(Path::new(&inputs[1].audio_path))
            .expect("overlap-trimmed WAV should be valid");
        assert_eq!(trimmed.duration_ms, 500);
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn manifest_rejects_too_many_speakers_before_asr() {
        let root = std::env::temp_dir().join(format!(
            "discord_transcript_manifest_speaker_limit_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("fixture directory should be created");
        for speaker in 0..=MAX_SPEAKER_AUDIO_OUTPUTS {
            write_sparse_wav(
                &root.join(format!("speaker{speaker:03}_1_1000.wav")),
                3_200,
                16_000,
            );
        }

        let error = scan_chunk_manifest(&root).expect_err("speaker limit should be enforced");
        assert!(error.contains("too many speaker audio outputs"));
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }
}
