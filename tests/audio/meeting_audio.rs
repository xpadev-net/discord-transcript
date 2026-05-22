use discord_transcript::audio::receiver::BufferedFrame;
use discord_transcript::audio::{build_wav_bytes_raw, build_wav_chunk};
use discord_transcript::application::runtime::merge_user_chunks_to_mixdown;
use discord_transcript::audio::meeting_audio::{build_speaker_audio_inputs, load_chunks};
use discord_transcript::audio::wav::resample_pcm_16le;
use discord_transcript::infrastructure::workspace::MeetingWorkspaceLayout;
use std::fs;
use std::path::PathBuf;

fn unique_temp_dir(test_name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "discord_transcript_meeting_audio_{test_name}_{nanos}"
    ))
}

fn i16_pcm(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

#[test]
fn speaker_audio_builds_offsets_and_gaps_per_user() {
    let base = unique_temp_dir("gaps");
    fs::create_dir_all(&base).expect("dir should be created");

    // Sample rate 1kHz for easy duration math: 1s = 2_000 bytes.
    let chunk_one = build_wav_bytes_raw(&vec![0; 2_000], 1_000, 1, 16).unwrap();
    fs::write(base.join("alice_1_1000.wav"), &chunk_one).unwrap();

    let chunk_two = build_wav_bytes_raw(&vec![0; 2_000], 1_000, 1, 16).unwrap();
    fs::write(base.join("alice_2_2500.wav"), &chunk_two).unwrap();

    // Bob starts later and speaks for 0.5 seconds.
    let bob_chunk = build_wav_bytes_raw(&vec![0; 1_000], 1_000, 1, 16).unwrap();
    fs::write(base.join("bob_1_1500.wav"), &bob_chunk).unwrap();

    let outputs = build_speaker_audio_inputs(&base, false).expect("speaker audio should build");
    assert_eq!(outputs.len(), 2);

    let alice = outputs
        .iter()
        .find(|o| o.speaker_id == "alice")
        .expect("alice audio should exist");
    assert_eq!(alice.offset_ms, 0);
    let alice_bytes = fs::read(&alice.audio_path).expect("alice audio should exist");
    // 1s audio + 0.5s gap + 1s audio = 2.5s = 5_000 bytes PCM + 44-byte header.
    assert_eq!(alice_bytes.len(), 5_044);

    let bob = outputs
        .iter()
        .find(|o| o.speaker_id == "bob")
        .expect("bob audio should exist");
    assert_eq!(bob.offset_ms, 500);
    let bob_bytes = fs::read(&bob.audio_path).expect("bob audio should exist");
    // 0.5s audio = 1_000 bytes + 44-byte header.
    assert_eq!(bob_bytes.len(), 1_044);

    let _ = fs::remove_dir_all(base);
}

#[test]
fn speaker_audio_writes_to_workspace_speakers_dir() {
    let root = unique_temp_dir("workspace_speakers");
    let workspace =
        MeetingWorkspaceLayout::new(root.to_string_lossy().as_ref()).for_meeting("g1", "vc1", "m1");
    fs::create_dir_all(workspace.audio_dir()).expect("audio dir should be created");

    let chunk = build_wav_bytes_raw(&vec![0; 2_000], 1_000, 1, 16).unwrap();
    fs::write(workspace.audio_dir().join("alice_1_1000.wav"), &chunk).unwrap();

    let outputs = build_speaker_audio_inputs(&workspace.audio_dir(), false)
        .expect("speaker audio should build");

    assert_eq!(outputs.len(), 1);
    assert_eq!(
        PathBuf::from(&outputs[0].audio_path),
        workspace.speakers_dir().join("alice_speaker.wav")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn build_wav_chunk_preserves_timestamp_gaps_as_silence() {
    let first = i16_pcm(&[1; 10]);
    let second = i16_pcm(&[2; 10]);
    let wav = build_wav_chunk(
        &[
            BufferedFrame {
                timestamp_ms: 1_000,
                pcm_16le_bytes: first.clone(),
            },
            BufferedFrame {
                timestamp_ms: 1_030,
                pcm_16le_bytes: second.clone(),
            },
        ],
        1_000,
    )
    .expect("wav chunk should build");

    let pcm = &wav.bytes[44..];
    assert_eq!(pcm.len(), 80);
    assert_eq!(&pcm[..20], &first);
    assert_eq!(&pcm[20..60], vec![0; 40].as_slice());
    assert_eq!(&pcm[60..], &second);
}

#[test]
fn build_wav_chunk_does_not_insert_extra_silence_for_contiguous_frames() {
    let first = i16_pcm(&[1; 10]);
    let second = i16_pcm(&[2; 10]);
    let wav = build_wav_chunk(
        &[
            BufferedFrame {
                timestamp_ms: 1_000,
                pcm_16le_bytes: first.clone(),
            },
            BufferedFrame {
                timestamp_ms: 1_010,
                pcm_16le_bytes: second.clone(),
            },
        ],
        1_000,
    )
    .expect("wav chunk should build");

    assert_eq!(wav.bytes.len(), 84);
    assert_eq!(&wav.bytes[44..64], &first);
    assert_eq!(&wav.bytes[64..], &second);
}

#[test]
fn build_wav_chunk_keeps_overlapping_frames_without_dropping_pcm() {
    let first = i16_pcm(&[1; 10]);
    let second = i16_pcm(&[2; 5]);
    let third = i16_pcm(&[3; 10]);
    let wav = build_wav_chunk(
        &[
            BufferedFrame {
                timestamp_ms: 1_000,
                pcm_16le_bytes: first.clone(),
            },
            BufferedFrame {
                timestamp_ms: 1_002,
                pcm_16le_bytes: second.clone(),
            },
            BufferedFrame {
                timestamp_ms: 2_000,
                pcm_16le_bytes: third.clone(),
            },
        ],
        1_000,
    )
    .expect("wav chunk should build");

    let pcm = &wav.bytes[44..];
    assert_eq!(pcm.len(), 2_020);
    assert_eq!(&pcm[..20], &first);
    assert_eq!(&pcm[20..30], &second);
    assert_eq!(&pcm[30..2_000], vec![0; 1_970].as_slice());
    assert_eq!(&pcm[2_000..], &third);
}

#[test]
fn speaker_audio_does_not_normalize_pcm_amplitude() {
    let base = unique_temp_dir("no_normalize");
    fs::create_dir_all(&base).expect("dir should be created");

    let pcm = i16_pcm(&[2, -2, 4, -4]);
    let wav = build_wav_bytes_raw(&pcm, 1_000, 1, 16).unwrap();
    fs::write(base.join("user_1_0.wav"), &wav).unwrap();

    let outputs = build_speaker_audio_inputs(&base, false).expect("speaker audio should build");
    let speaker_wav = fs::read(&outputs[0].audio_path).expect("speaker wav should exist");
    assert_eq!(&speaker_wav[44..], pcm.as_slice());

    let _ = fs::remove_dir_all(base);
}

#[test]
fn speaker_audio_handles_legacy_chunk_names() {
    let base = unique_temp_dir("legacy");
    fs::create_dir_all(&base).expect("dir should be created");

    let wav = build_wav_bytes_raw(&vec![0; 2_000], 1_000, 1, 16).unwrap();
    fs::write(base.join("legacyuser_1.wav"), &wav).unwrap();

    let outputs = build_speaker_audio_inputs(&base, false).expect("legacy naming should be supported");
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].speaker_id, "legacyuser");

    let _ = fs::remove_dir_all(base);
}

#[test]
fn load_chunks_skips_zero_byte_wav_and_builds_mixdown_from_remaining_chunks() {
    let base = unique_temp_dir("skip_zero_byte");
    fs::create_dir_all(&base).expect("dir should be created");

    fs::write(base.join("bad_2_1000.wav"), []).expect("zero-byte wav should be written");
    let good = build_wav_bytes_raw(&vec![0; 2_000], 1_000, 1, 16).unwrap();
    fs::write(base.join("alice_1_1000.wav"), &good).unwrap();

    let chunks = load_chunks(&base).expect("valid chunk should survive corrupt neighbor");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].user_id, "alice");

    let mixdown =
        merge_user_chunks_to_mixdown(&base, false).expect("mixdown should succeed with one chunk");
    assert!(PathBuf::from(mixdown).exists());

    let outputs = build_speaker_audio_inputs(&base, false).expect("speaker audio should succeed");
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].speaker_id, "alice");

    let _ = fs::remove_dir_all(base);
}

#[test]
fn load_chunks_skips_truncated_wav_and_builds_mixdown_from_remaining_chunks() {
    let base = unique_temp_dir("skip_truncated");
    fs::create_dir_all(&base).expect("dir should be created");

    let good = build_wav_bytes_raw(&vec![0; 2_000], 1_000, 1, 16).unwrap();
    let truncated = good[..80].to_vec();
    fs::write(base.join("bad_2_2000.wav"), &truncated).expect("truncated wav should be written");
    fs::write(base.join("alice_1_1000.wav"), &good).unwrap();

    let chunks = load_chunks(&base).expect("valid chunk should survive truncated neighbor");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].user_id, "alice");

    let mixdown =
        merge_user_chunks_to_mixdown(&base, false).expect("mixdown should succeed with one chunk");
    assert!(PathBuf::from(mixdown).exists());

    let outputs = build_speaker_audio_inputs(&base, false).expect("speaker audio should succeed");
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].speaker_id, "alice");

    let _ = fs::remove_dir_all(base);
}

#[test]
fn load_chunks_reports_skipped_count_when_all_chunks_are_corrupt() {
    let base = unique_temp_dir("all_corrupt");
    fs::create_dir_all(&base).expect("dir should be created");
    fs::write(base.join("bad_1_1000.wav"), []).expect("zero-byte wav should be written");
    let good = build_wav_bytes_raw(&vec![0; 2_000], 1_000, 1, 16).unwrap();
    fs::write(base.join("bad_2_2000.wav"), &good[..80]).unwrap();

    let err = load_chunks(&base).expect_err("only corrupt chunks should fail");
    assert!(err.contains("no audio chunks found for meeting"));
    assert!(err.contains("skipped 2 corrupt chunk(s)"));

    let _ = fs::remove_dir_all(base);
}

#[test]
fn load_chunks_ignores_temporary_chunk_files() {
    let base = unique_temp_dir("ignore_tmp");
    fs::create_dir_all(&base).expect("dir should be created");
    let wav = build_wav_bytes_raw(&[0; 96], 48_000, 1, 16).unwrap();
    fs::write(base.join("user_1_0.wav.tmp"), &wav).unwrap();
    fs::write(base.join("mixdown.wav.tmp"), &wav).unwrap();

    let err = load_chunks(&base).expect_err("only temporary files should not count as chunks");
    assert!(err.contains("no audio chunks found"));

    let _ = fs::remove_dir_all(base);
}

#[test]
fn speaker_audio_resamples_48k_to_16k() {
    let base = unique_temp_dir("resample");
    fs::create_dir_all(&base).expect("dir should be created");

    // 48kHz, 1 second = 48_000 samples = 96_000 bytes PCM
    let pcm_48k = vec![0u8; 96_000];
    let wav = build_wav_bytes_raw(&pcm_48k, 48_000, 1, 16).unwrap();
    fs::write(base.join("user_1_0.wav"), &wav).unwrap();

    let outputs =
        build_speaker_audio_inputs(&base, true).expect("resampled speaker audio should build");
    assert_eq!(outputs.len(), 1);

    let wav_bytes = fs::read(&outputs[0].audio_path).expect("speaker wav should exist");
    // Verify WAV header sample rate is 16kHz
    let header_sample_rate =
        u32::from_le_bytes([wav_bytes[24], wav_bytes[25], wav_bytes[26], wav_bytes[27]]);
    assert_eq!(header_sample_rate, 16_000);

    // 48_000 samples / 3 = 16_000 samples = 32_000 bytes PCM + 44-byte header
    assert_eq!(wav_bytes.len(), 32_044);

    let _ = fs::remove_dir_all(base);
}

#[test]
fn resample_returns_unchanged_for_same_rate() {
    let input = vec![0u8; 100];
    let (output, rate) = resample_pcm_16le(&input, 16_000, 16_000);
    assert_eq!(output, input);
    assert_eq!(rate, 16_000);
}

#[test]
fn resample_returns_unchanged_for_unsupported_ratio() {
    let input = vec![0u8; 100];
    let (output, rate) = resample_pcm_16le(&input, 44_100, 16_000);
    assert_eq!(output, input);
    assert_eq!(rate, 44_100);
}

#[test]
fn resample_returns_unchanged_for_empty_input() {
    let (output, rate) = resample_pcm_16le(&[], 48_000, 16_000);
    assert!(output.is_empty());
    assert_eq!(rate, 48_000);
}

#[test]
fn resample_returns_unchanged_for_short_input() {
    // 2 samples = 4 bytes, less than 3 needed for decimation
    let input = vec![0u8; 4];
    let (output, rate) = resample_pcm_16le(&input, 48_000, 16_000);
    assert_eq!(output, input);
    assert_eq!(rate, 48_000);
}

#[test]
fn resample_48k_to_16k_correct_sample_count() {
    // 300 samples at 48kHz → 100 samples at 16kHz
    let input = vec![0u8; 600]; // 300 samples * 2 bytes
    let (output, rate) = resample_pcm_16le(&input, 48_000, 16_000);
    assert_eq!(rate, 16_000);
    assert_eq!(output.len(), 200); // 100 samples * 2 bytes
}
