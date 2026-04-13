use chrono::{DateTime, Utc};
use serenity::all::{ChannelId, Http, MessageId};
use serenity::builder::GetMessages;
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcTextMessage {
    pub speaker_id: String,
    pub text: String,
    pub timestamp: DateTime<Utc>,
    pub timestamp_ms: i64,
    pub message_id: u64,
}

pub async fn fetch_vc_text_messages(
    http: &Http,
    voice_channel_id: &str,
    meeting_start: DateTime<Utc>,
    meeting_end: DateTime<Utc>,
) -> Result<Vec<VcTextMessage>, String> {
    let channel_id_u64 = voice_channel_id.parse::<u64>().map_err(|err| {
        format!("invalid voice_channel_id for VC chat fetch: {voice_channel_id} ({err})")
    })?;
    let channel_id = ChannelId::new(channel_id_u64);

    // Avoid scanning all post-meeting messages: start paging from just after meeting_end.
    let mut before: Option<MessageId> = Some(MessageId::new(snowflake_for_timestamp_ms(
        meeting_end.timestamp_millis().saturating_add(1),
    )));
    let mut out = Vec::new();
    let mut pages = 0u32;
    const MAX_PAGES: u32 = 50;
    loop {
        if pages >= MAX_PAGES {
            return Err(format!(
                "VC chat fetch pagination exceeded max pages ({MAX_PAGES})"
            ));
        }
        pages += 1;

        let mut builder = GetMessages::new().limit(100);
        if let Some(before_id) = before {
            builder = builder.before(before_id);
        }
        let batch = channel_id
            .messages(http, builder)
            .await
            .map_err(|err| err.to_string())?;
        if batch.is_empty() {
            break;
        }

        // Discord returns newest->oldest for this request; iterate newest first, then update `before`.
        for message in &batch {
            let timestamp_ms = message.timestamp.timestamp_millis();
            let Some(timestamp) = DateTime::<Utc>::from_timestamp_millis(timestamp_ms) else {
                continue;
            };

            if timestamp < meeting_start {
                // We are paging backwards; once we're older than meeting_start, we can stop.
                return Ok(finish_vc_messages(out));
            }
            if timestamp > meeting_end {
                continue;
            }

            let content = message.content.trim();
            let text = if content.is_empty() {
                if message.attachments.is_empty() {
                    continue;
                }
                "[attachment]".to_owned()
            } else {
                content.to_owned()
            };

            out.push(VcTextMessage {
                speaker_id: message.author.id.to_string(),
                text,
                timestamp,
                timestamp_ms,
                message_id: message.id.get(),
            });
        }

        // Continue paging from the oldest message id in this batch.
        before = batch.last().map(|m| m.id);
    }

    Ok(finish_vc_messages(out))
}

pub fn warn_and_fallback_on_vc_text_error(meeting_id: &str, err: &str) {
    warn!(
        meeting_id = %meeting_id,
        error = %err,
        "failed to fetch VC text messages; continuing with voice transcript only"
    );
}

fn finish_vc_messages(mut out: Vec<VcTextMessage>) -> Vec<VcTextMessage> {
    out.sort_by(|a, b| {
        a.timestamp_ms
            .cmp(&b.timestamp_ms)
            .then(a.message_id.cmp(&b.message_id))
    });
    out
}

fn snowflake_for_timestamp_ms(timestamp_ms: i64) -> u64 {
    // Discord epoch: 2015-01-01T00:00:00Z in milliseconds.
    const DISCORD_EPOCH_MS: i64 = 1_420_070_400_000;
    let clamped = (timestamp_ms - DISCORD_EPOCH_MS).max(0) as u64;
    clamped << 22
}
