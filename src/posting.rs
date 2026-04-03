pub const DISCORD_MESSAGE_LIMIT: usize = 2_000;

/// Count the length of a string in UTF-16 code units, which is how Discord
/// measures its 2000-character message limit (like JavaScript's `string.length`).
fn utf16_len(s: &str) -> usize {
    s.chars().map(|c| c.len_utf16()).sum()
}

pub fn split_discord_message(text: &str, limit: usize) -> Vec<String> {
    if limit == 0 {
        return vec![text.to_owned()];
    }
    if text.is_empty() {
        return vec![];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for line in text.split_inclusive('\n') {
        if utf16_len(line) <= limit {
            push_segment(line, limit, &mut current, &mut chunks);
            continue;
        }

        for piece in split_hard(line, limit) {
            push_segment(&piece, limit, &mut current, &mut chunks);
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

fn push_segment(segment: &str, limit: usize, current: &mut String, chunks: &mut Vec<String>) {
    let current_len = utf16_len(current);
    let seg_len = utf16_len(segment);

    if current_len + seg_len <= limit {
        current.push_str(segment);
        return;
    }

    if !current.is_empty() {
        chunks.push(std::mem::take(current));
    }
    current.push_str(segment);
}

fn split_hard(input: &str, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut buf_len = 0usize;

    for ch in input.chars() {
        let ch_len = ch.len_utf16();
        if buf_len + ch_len > limit {
            out.push(std::mem::take(&mut buf));
            buf_len = 0;
        }
        buf.push(ch);
        buf_len += ch_len;
    }

    if !buf.is_empty() {
        out.push(buf);
    }

    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptDelivery {
    AttachTextFile,
    ShareLinkOnly,
}

pub fn decide_transcript_delivery(
    transcript_size_bytes: usize,
    attachment_limit_bytes: usize,
) -> TranscriptDelivery {
    if transcript_size_bytes <= attachment_limit_bytes {
        TranscriptDelivery::AttachTextFile
    } else {
        TranscriptDelivery::ShareLinkOnly
    }
}
