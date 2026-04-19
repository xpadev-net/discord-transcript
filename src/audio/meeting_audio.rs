use crate::application::summary::SpeakerAudioInput;
use crate::audio::build_wav_bytes_raw;
use crate::audio::songbird_adapter::SsrcTracker;
use crate::audio::wav::{normalize_rms_pcm_16le, resample_pcm_16le};
use crate::infrastructure::storage_fs::sanitize_path_component;
use crate::infrastructure::workspace::SSRC_MAPPING_FILENAME;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedFilename {
    user_id: String,
    sequence: u64,
    start_ms: Option<u64>,
}

pub fn load_chunks(meeting_dir: &Path) -> Result<Vec<LoadedChunk>, String> {
    let mut chunks = Vec::new();
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
            let parsed = parse_chunk_filename(&path)?;
            let (sample_rate, pcm) = read_wav_pcm(&path)?;
            let duration_ms = pcm_duration_ms(&pcm, sample_rate);
            let start_ms = parsed
                .start_ms
                .unwrap_or_else(|| fallback_start_ms(&path, duration_ms));
            chunks.push(LoadedChunk {
                user_id: parsed.user_id,
                sequence: parsed.sequence,
                start_ms,
                duration_ms,
                sample_rate,
                pcm,
                path,
            });
        }
    }

    if chunks.is_empty() {
        return Err("no audio chunks found for meeting".to_owned());
    }

    Ok(chunks)
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
    let data = fs::read(path).map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    if data.len() < 44 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err(format!(
            "invalid WAV header for {} (too small or missing RIFF/WAVE)",
            path.display()
        ));
    }

    let channels = u16::from_le_bytes([data[22], data[23]]);
    let sample_rate = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);
    let bits_per_sample = u16::from_le_bytes([data[34], data[35]]);
    if channels != 1 || bits_per_sample != 16 {
        return Err(format!(
            "unsupported WAV format for {}: channels={}, bits_per_sample={}",
            path.display(),
            channels,
            bits_per_sample
        ));
    }
    if &data[36..40] != b"data" {
        return Err(format!(
            "missing data chunk in WAV header for {}",
            path.display()
        ));
    }
    Ok((sample_rate, data[44..].to_vec()))
}

fn pcm_duration_ms(pcm: &[u8], sample_rate: u32) -> u64 {
    if sample_rate == 0 {
        return 0;
    }
    let samples = pcm.len() as u128 / 2;
    (samples.saturating_mul(1_000) / sample_rate as u128) as u64
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

fn silence_bytes(duration_ms: u64, sample_rate: u32) -> Vec<u8> {
    let samples = (duration_ms as u128)
        .saturating_mul(sample_rate as u128)
        .saturating_div(1_000) as usize;
    vec![0; samples.saturating_mul(2)]
}

/// Target RMS amplitude for per-speaker audio normalization.
/// 3000 out of i16 max (32767) is a moderate level that avoids clipping.
const NORMALIZE_TARGET_RMS: f64 = 3000.0;

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
    let mut chunks = load_chunks(meeting_dir)?;

    // Resolve any SSRC-based user IDs using persisted mapping
    let ssrc_mapping = load_ssrc_mapping(meeting_dir);
    for chunk in &mut chunks {
        if let Some(real_id) = ssrc_mapping.get(&chunk.user_id) {
            chunk.user_id = real_id.clone();
        }
    }
    let sample_rate = chunks.first().map(|c| c.sample_rate).unwrap_or(48_000);
    if chunks.iter().any(|c| c.sample_rate != sample_rate) {
        return Err("mixed sample rates are not supported".to_owned());
    }

    let meeting_start_ms = compute_meeting_start_ms(&chunks);

    let mut per_user: HashMap<String, Vec<LoadedChunk>> = HashMap::new();
    for chunk in chunks {
        per_user
            .entry(chunk.user_id.clone())
            .or_default()
            .push(chunk);
    }

    let speaker_dir = meeting_dir.join("speakers");
    fs::create_dir_all(&speaker_dir).map_err(|err| {
        format!(
            "failed to create speaker dir {}: {err}",
            speaker_dir.display()
        )
    })?;

    let mut outputs = Vec::new();
    for (user_id, mut user_chunks) in per_user {
        user_chunks.sort_by(|a, b| {
            a.start_ms
                .cmp(&b.start_ms)
                .then(a.sequence.cmp(&b.sequence))
        });
        let Some(first) = user_chunks.first() else {
            continue;
        };

        // Normalize each chunk's volume before stitching with silence gaps.
        // This avoids silence gaps diluting the RMS calculation.
        let normalized_first = normalize_rms_pcm_16le(&first.pcm, NORMALIZE_TARGET_RMS);

        let mut pcm_out = Vec::new();
        let mut current_ms = first.start_ms + first.duration_ms;
        pcm_out.extend_from_slice(&normalized_first);
        for chunk in user_chunks.iter().skip(1) {
            if chunk.start_ms > current_ms {
                let gap_ms = chunk.start_ms - current_ms;
                pcm_out.extend_from_slice(&silence_bytes(gap_ms, sample_rate));
            }
            let chunk_pcm = normalize_rms_pcm_16le(&chunk.pcm, NORMALIZE_TARGET_RMS);
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
                pcm_out.extend_from_slice(trimmed);
                current_ms = current_ms.saturating_add(pcm_duration_ms(trimmed, sample_rate));
                continue;
            }
            pcm_out.extend_from_slice(&chunk_pcm);
            current_ms = chunk.start_ms + chunk.duration_ms;
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
        let output_path = speaker_dir.join(format!("{safe_user}_speaker.wav"));
        fs::write(&output_path, &wav_bytes)
            .map_err(|err| format!("failed to write speaker audio for {user_id}: {err}"))?;

        outputs.push(SpeakerAudioInput {
            speaker_id: user_id,
            audio_path: output_path.to_string_lossy().to_string(),
            offset_ms: first.start_ms.saturating_sub(meeting_start_ms),
        });
    }

    outputs.sort_by(|a, b| a.speaker_id.cmp(&b.speaker_id));
    Ok(outputs)
}
