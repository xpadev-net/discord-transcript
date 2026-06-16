use discord_transcript::audio::receiver::BufferedFrame;
use discord_transcript::audio::wav::MAX_WAV_CHUNK_PCM_BYTES;
use discord_transcript::audio::{build_wav_bytes_raw, build_wav_chunk};
use discord_transcript::application::runtime::merge_user_chunks_to_mixdown;
use discord_transcript::audio::meeting_audio::{
    MAX_MEETING_AUDIO_CHUNKS, MAX_MEETING_AUDIO_SPAN_MS, MAX_SPEAKER_AUDIO_OUTPUTS,
    ProcessedAudioChunk, build_speaker_audio_inputs, build_speaker_audio_inputs_excluding_processed_chunks,
    load_chunks,
};
use discord_transcript::audio::wav::{MAX_SUPPORTED_WAV_SAMPLE_RATE, resample_pcm_16le};
use discord_transcript::infrastructure::workspace::MeetingWorkspaceLayout;
use std::fs;
use std::fs::File;
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

fn write_u16_header(wav: &mut [u8], offset: usize, value: u16) {
    wav[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32_header(wav: &mut [u8], offset: usize, value: u32) {
    wav[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn valid_48k_wav_for_header_tests() -> Vec<u8> {
    build_wav_bytes_raw(&i16_pcm(&[1, -1, 2, -2]), 48_000, 1, 16).unwrap()
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
fn speaker_audio_excludes_processed_chunks_but_keeps_timeline_base() {
    let base = unique_temp_dir("exclude_processed");
    fs::create_dir_all(&base).expect("dir should be created");

    let chunk_one = build_wav_bytes_raw(&vec![0; 2_000], 1_000, 1, 16).unwrap();
    fs::write(base.join("alice_1_1000.wav"), &chunk_one).unwrap();

    let chunk_two = build_wav_bytes_raw(&vec![0; 2_000], 1_000, 1, 16).unwrap();
    fs::write(base.join("bob_1_2500.wav"), &chunk_two).unwrap();

    let outputs = build_speaker_audio_inputs_excluding_processed_chunks(
        &base,
        false,
        &[ProcessedAudioChunk {
            speaker_id: "alice".to_owned(),
            sequence: 1,
            start_ms: 1_000,
        }],
    )
    .expect("speaker audio should build");

    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].speaker_id, "bob");
    assert_eq!(
        outputs[0].offset_ms, 1_500,
        "offset should stay relative to the full meeting, not the first unprocessed chunk"
    );

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
fn speaker_audio_removes_stale_generated_wavs_on_rebuild() {
    let root = unique_temp_dir("speaker_cleanup");
    let workspace =
        MeetingWorkspaceLayout::new(root.to_string_lossy().as_ref()).for_meeting("g1", "vc1", "m1");
    fs::create_dir_all(workspace.audio_dir()).expect("audio dir should be created");
    fs::create_dir_all(workspace.speakers_dir()).expect("speaker dir should be created");

    let stale_path = workspace.speakers_dir().join("old_speaker.wav");
    fs::write(&stale_path, b"stale").expect("stale speaker wav should be written");
    let keep_path = workspace.speakers_dir().join("notes.txt");
    fs::write(&keep_path, b"keep").expect("non-speaker file should be written");

    let chunk = build_wav_bytes_raw(&vec![0; 2_000], 1_000, 1, 16).unwrap();
    fs::write(workspace.audio_dir().join("alice_1_1000.wav"), &chunk).unwrap();

    let outputs = build_speaker_audio_inputs(&workspace.audio_dir(), false)
        .expect("speaker audio should build");

    assert_eq!(outputs.len(), 1);
    assert!(!stale_path.exists());
    assert!(keep_path.exists());

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
fn build_wav_chunk_rejects_far_future_gap_before_allocating() {
    let first = i16_pcm(&[1]);
    let second = i16_pcm(&[2]);
    let gap_ms = ((MAX_WAV_CHUNK_PCM_BYTES as u64 / 2) * 1_000 / 48_000) + 1_000;
    let err = build_wav_chunk(
        &[
            BufferedFrame {
                timestamp_ms: 0,
                pcm_16le_bytes: first,
            },
            BufferedFrame {
                timestamp_ms: gap_ms,
                pcm_16le_bytes: second,
            },
        ],
        48_000,
    )
    .expect_err("far-future frame gap should be rejected before allocating silence");

    assert!(err.to_string().contains("PCM assembly too large"));
}

#[test]
fn build_wav_bytes_raw_rejects_unaligned_pcm_payload() {
    let err = build_wav_bytes_raw(&[0, 0, 0], 48_000, 1, 16)
        .expect_err("odd PCM byte length should be rejected");

    assert!(err.to_string().contains("invalid PCM byte length"));
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
fn speaker_audio_rejects_long_gap_before_allocating_silence() {
    let base = unique_temp_dir("speaker_gap_limit");
    fs::create_dir_all(&base).expect("dir should be created");

    let wav = build_wav_bytes_raw(&i16_pcm(&[1]), 48_000, 1, 16).unwrap();
    fs::write(base.join("alice_1_0.wav"), &wav).unwrap();
    fs::write(
        base.join(format!("alice_2_{}.wav", MAX_MEETING_AUDIO_SPAN_MS + 60_000)),
        &wav,
    )
    .unwrap();

    let err = build_speaker_audio_inputs(&base, false)
        .expect_err("speaker stitching should reject oversized silence gaps");
    assert!(err.contains("speaker audio assembly exceeds PCM limit"));

    let _ = fs::remove_dir_all(base);
}

#[test]
fn speaker_audio_keeps_existing_outputs_when_rebuild_fails() {
    let root = unique_temp_dir("speaker_failure_keeps_existing");
    let workspace =
        MeetingWorkspaceLayout::new(root.to_string_lossy().as_ref()).for_meeting("g1", "vc1", "m1");
    fs::create_dir_all(workspace.audio_dir()).expect("audio dir should be created");
    fs::create_dir_all(workspace.speakers_dir()).expect("speaker dir should be created");

    let stale_path = workspace.speakers_dir().join("old_speaker.wav");
    fs::write(&stale_path, b"stale").expect("stale speaker wav should be written");
    let existing_output = workspace.speakers_dir().join("alice_speaker.wav");
    fs::write(&existing_output, b"existing").expect("existing speaker wav should be written");

    let wav = build_wav_bytes_raw(&i16_pcm(&[1]), 48_000, 1, 16).unwrap();
    fs::write(workspace.audio_dir().join("alice_1_0.wav"), &wav).unwrap();
    fs::write(workspace.audio_dir().join("bob_1_0.wav"), &wav).unwrap();
    fs::write(
        workspace
            .audio_dir()
            .join(format!("bob_2_{}.wav", MAX_MEETING_AUDIO_SPAN_MS + 60_000)),
        &wav,
    )
    .unwrap();

    let err = build_speaker_audio_inputs(&workspace.audio_dir(), false)
        .expect_err("oversized speaker rebuild should fail");
    assert!(err.contains("speaker audio assembly exceeds PCM limit"));
    assert!(stale_path.exists());
    assert_eq!(fs::read(&existing_output).unwrap(), b"existing");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn speaker_audio_rejects_sanitized_filename_collisions_before_overwrite() {
    let root = unique_temp_dir("speaker_collision");
    let workspace =
        MeetingWorkspaceLayout::new(root.to_string_lossy().as_ref()).for_meeting("g1", "vc1", "m1");
    fs::create_dir_all(workspace.audio_dir()).expect("audio dir should be created");
    fs::create_dir_all(workspace.speakers_dir()).expect("speaker dir should be created");

    let existing_output = workspace.speakers_dir().join("ab_speaker.wav");
    fs::write(&existing_output, b"existing").expect("existing speaker wav should be written");

    let wav = build_wav_bytes_raw(&i16_pcm(&[1]), 1_000, 1, 16).unwrap();
    fs::write(workspace.audio_dir().join("a:b_1_0.wav"), &wav).unwrap();
    fs::write(workspace.audio_dir().join("ab_1_0.wav"), &wav).unwrap();

    let err = build_speaker_audio_inputs(&workspace.audio_dir(), false)
        .expect_err("sanitized output collision should fail");
    assert!(err.contains("speaker audio output filename collision"));
    assert_eq!(fs::read(&existing_output).unwrap(), b"existing");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn speaker_audio_rejects_too_many_output_files() {
    let base = unique_temp_dir("speaker_output_limit");
    fs::create_dir_all(&base).expect("dir should be created");

    let wav = build_wav_bytes_raw(&i16_pcm(&[1]), 1_000, 1, 16).unwrap();
    for speaker_index in 0..=MAX_SPEAKER_AUDIO_OUTPUTS {
        fs::write(
            base.join(format!("speaker{speaker_index}_1_0.wav")),
            &wav,
        )
        .unwrap();
    }

    let err = build_speaker_audio_inputs(&base, false)
        .expect_err("speaker stitching should reject too many output files");
    assert!(err.contains("too many speaker audio outputs"));

    let _ = fs::remove_dir_all(base);
}

#[test]
fn load_chunks_rejects_too_many_valid_chunk_files() {
    let base = unique_temp_dir("chunk_count_limit");
    fs::create_dir_all(&base).expect("dir should be created");

    let wav = build_wav_bytes_raw(&i16_pcm(&[1]), 1_000, 1, 16).unwrap();
    for chunk_index in 0..=MAX_MEETING_AUDIO_CHUNKS {
        fs::write(base.join(format!("alice_{chunk_index}_0.wav")), &wav).unwrap();
    }

    let err = load_chunks(&base).expect_err("chunk loading should reject too many valid files");
    assert!(err.contains("too many audio chunks"));

    let _ = fs::remove_dir_all(base);
}

#[test]
fn load_chunks_rejects_oversized_wav_before_reading_body() {
    let base = unique_temp_dir("oversized_file");
    fs::create_dir_all(&base).expect("dir should be created");

    let path = base.join("alice_1_0.wav");
    let file = File::create(&path).expect("sparse wav placeholder should be created");
    file.set_len((MAX_WAV_CHUNK_PCM_BYTES + 45) as u64)
        .expect("sparse oversized wav should be extended");

    let err = load_chunks(&base).expect_err("oversized wav should not be loaded");
    assert!(err.contains("no audio chunks found"));
    assert!(err.contains("skipped 1 corrupt chunk(s)"));

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
fn load_chunks_skips_invalid_header_values_and_keeps_valid_48k_wav() {
    let base = unique_temp_dir("skip_invalid_headers");
    fs::create_dir_all(&base).expect("dir should be created");

    let valid = valid_48k_wav_for_header_tests();
    fs::write(base.join("alice_1_0.wav"), &valid).unwrap();

    let mut zero_rate = valid_48k_wav_for_header_tests();
    write_u32_header(&mut zero_rate, 24, 0);
    write_u32_header(&mut zero_rate, 28, 0);
    fs::write(base.join("badzero_1_0.wav"), zero_rate).unwrap();

    let unsupported_rate = MAX_SUPPORTED_WAV_SAMPLE_RATE + 1;
    let mut unsupported_high_rate = valid_48k_wav_for_header_tests();
    write_u32_header(&mut unsupported_high_rate, 24, unsupported_rate);
    write_u32_header(&mut unsupported_high_rate, 28, unsupported_rate * 2);
    fs::write(
        base.join("badunsupportedrate_1_0.wav"),
        unsupported_high_rate,
    )
    .unwrap();

    let mut huge_rate = valid_48k_wav_for_header_tests();
    write_u32_header(&mut huge_rate, 24, u32::MAX);
    fs::write(base.join("badhuge_1_0.wav"), huge_rate).unwrap();

    let mut bad_format = valid_48k_wav_for_header_tests();
    write_u16_header(&mut bad_format, 20, 3);
    fs::write(base.join("badformat_1_0.wav"), bad_format).unwrap();

    let mut bad_byte_rate = valid_48k_wav_for_header_tests();
    write_u32_header(&mut bad_byte_rate, 28, 1);
    fs::write(base.join("badbyterate_1_0.wav"), bad_byte_rate).unwrap();

    let mut bad_block_align = valid_48k_wav_for_header_tests();
    write_u16_header(&mut bad_block_align, 32, 4);
    fs::write(base.join("badalign_1_0.wav"), bad_block_align).unwrap();

    let mut bad_chunk_size = valid_48k_wav_for_header_tests();
    write_u32_header(&mut bad_chunk_size, 4, 35);
    fs::write(base.join("badchunksize_1_0.wav"), bad_chunk_size).unwrap();

    let chunks = load_chunks(&base).expect("valid 48k chunk should survive invalid neighbors");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].user_id, "alice");
    assert_eq!(chunks[0].sample_rate, 48_000);
    assert_eq!(chunks[0].duration_ms, 0);
    assert_eq!(chunks[0].pcm, i16_pcm(&[1, -1, 2, -2]));

    let mixdown =
        merge_user_chunks_to_mixdown(&base, false).expect("mixdown should use the valid chunk");
    let mixdown_wav = fs::read(mixdown).expect("mixdown wav should be readable");
    assert_eq!(
        u32::from_le_bytes([
            mixdown_wav[24],
            mixdown_wav[25],
            mixdown_wav[26],
            mixdown_wav[27]
        ]),
        48_000
    );
    assert_eq!(
        u32::from_le_bytes([
            mixdown_wav[40],
            mixdown_wav[41],
            mixdown_wav[42],
            mixdown_wav[43]
        ]) as usize,
        i16_pcm(&[1, -1, 2, -2]).len()
    );

    let _ = fs::remove_dir_all(base);
}

#[test]
fn mixdown_skips_chunks_beyond_meeting_wall_clock_cap() {
    let base = unique_temp_dir("cap_offset");
    fs::create_dir_all(&base).expect("dir should be created");

    let good = build_wav_bytes_raw(&vec![0; 2_000], 1_000, 1, 16).unwrap();
    fs::write(base.join("alice_1_1000.wav"), &good).unwrap();
    let far_future = build_wav_bytes_raw(&vec![0; 2_000], 1_000, 1, 16).unwrap();
    fs::write(
        base.join(format!(
            "bad_2_{}.wav",
            discord_transcript::audio::meeting_audio::MAX_MEETING_AUDIO_SPAN_MS + 60_000
        )),
        &far_future,
    )
    .unwrap();

    let mixdown =
        merge_user_chunks_to_mixdown(&base, false).expect("mixdown should ignore far-future chunk");
    let mixdown_path = PathBuf::from(mixdown);
    assert!(mixdown_path.exists());

    let wav = fs::read(&mixdown_path).expect("mixdown wav should be readable");
    let data_chunk_size =
        u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]) as usize;
    assert_eq!(
        data_chunk_size, 2_000,
        "mixdown should contain only the in-cap chunk PCM"
    );

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
