use discord_transcript::audio::build_wav_bytes_raw;
use discord_transcript::audio::meeting_audio::build_speaker_audio_inputs;
use discord_transcript::audio::wav::resample_pcm_16le;
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
