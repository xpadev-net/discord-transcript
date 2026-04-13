use chrono::{DateTime, Utc};
use serenity::all::{ChannelId, Http};
use serenity::futures::StreamExt;
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcTextMessage {
    pub speaker_id: String,
    pub text: String,
    pub timestamp: DateTime<Utc>,
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

    let mut messages = channel_id.messages_iter(http).boxed();
    let mut out = Vec::new();
    while let Some(next) = messages.next().await {
        let message = next.map_err(|err| err.to_string())?;
        let ts = message.timestamp.unix_timestamp();
        let Some(timestamp) = DateTime::<Utc>::from_timestamp(ts, 0) else {
            continue;
        };

        if timestamp < meeting_start {
            break;
        }
        if timestamp > meeting_end {
            continue;
        }

        let content = message.content.trim();
        let text = if content.is_empty() {
            if message.attachments.is_empty() {
                continue;
            }
            "[添付ファイルあり]".to_owned()
        } else {
            content.to_owned()
        };

        out.push(VcTextMessage {
            speaker_id: message.author.id.to_string(),
            text,
            timestamp,
        });
    }

    out.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    Ok(out)
}

pub fn warn_and_fallback_on_vc_text_error(meeting_id: &str, err: &str) {
    warn!(
        meeting_id = %meeting_id,
        error = %err,
        "failed to fetch VC text messages; continuing with voice transcript only"
    );
}
