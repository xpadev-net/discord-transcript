use crate::application::summary::SpeakerAudioInput;
use crate::audio::build_wav_bytes_raw;
use crate::audio::songbird_adapter::SsrcTracker;
use crate::audio::wav::{
    MAX_SUPPORTED_WAV_SAMPLE_RATE, MAX_WAV_CHUNK_PCM_BYTES, MIN_SUPPORTED_WAV_SAMPLE_RATE,
    checked_pcm_growth, is_supported_wav_sample_rate, pcm_byte_len_for_duration_ms,
    pcm_duration_ms, resample_pcm_16le,
};
use crate::infrastructure::storage_fs::sanitize_path_component;
use crate::infrastructure::workspace::SSRC_MAPPING_FILENAME;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Maximum wall-clock span for a single meeting mixdown (24 hours).
pub const MAX_MEETING_AUDIO_SPAN_MS: u64 = 24 * 3600 * 1000;
pub const MAX_MEETING_AUDIO_CHUNKS: usize = 4_096;
pub const MAX_MEETING_AUDIO_INPUT_PCM_BYTES: usize = 512 * 1024 * 1024;
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
        user_id: user_part.to_owned(),
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
    let bytes_per_sample = u32::from(bits_per_sample) / 8;
    let expected_block_align = u16::try_from(
        u32::from(channels)
            .checked_mul(bytes_per_sample)
            .ok_or_else(|| format!("invalid block align in {}", path.display()))?,
    )
    .map_err(|_| format!("invalid block align in {}", path.display()))?;
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
    let minimum_riff_chunk_size = 36usize
        .checked_add(data_chunk_size)
        .ok_or_else(|| format!("invalid data chunk size in {}", path.display()))?;
    if riff_chunk_size < minimum_riff_chunk_size {
        return Err(format!(
            "inconsistent WAV chunk size in {}: riff_chunk_size={}, expected at least {minimum_riff_chunk_size}",
            path.display(),
            riff_chunk_size
        ));
    }
    if data_chunk_size > MAX_WAV_CHUNK_PCM_BYTES {
        return Err(format!(
            "PCM data too large in {}: {data_chunk_size} bytes (max {MAX_WAV_CHUNK_PCM_BYTES})",
            path.display()
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
    tracker
        .all_mappings()
        .iter()
        .map(|(ssrc, user_id)| {
            let sanitized_key = sanitize_path_component(&SsrcTracker::fallback_key(*ssrc));
            (sanitized_key, user_id.clone())
        })
        .collect()
}

pub fn build_speaker_audio_inputs(
    meeting_dir: &Path,
    resample_to_16k: bool,
) -> Result<Vec<SpeakerAudioInput>, String> {
    build_speaker_audio_inputs_excluding_processed_chunks(meeting_dir, resample_to_16k, &[])
}

pub fn build_speaker_audio_inputs_excluding_processed_chunks(
    meeting_dir: &Path,
    resample_to_16k: bool,
    processed_chunks: &[ProcessedAudioChunk],
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

    let speaker_dir = meeting_dir.join("speakers");
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
