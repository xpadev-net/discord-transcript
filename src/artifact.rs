use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactKind {
    TranscriptText,
    /// Transcript exceeds inline attachment limit; only a link is provided.
    TranscriptLink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptArtifact {
    pub kind: ArtifactKind,
    pub inline_attachment: Option<Vec<u8>>,
    pub link_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactError {
    MissingLink,
}

impl Display for ArtifactError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingLink => write!(f, "artifact link is required when attachment is omitted"),
        }
    }
}

impl std::error::Error for ArtifactError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactPolicy {
    pub attachment_limit_bytes: usize,
}

impl Default for ArtifactPolicy {
    fn default() -> Self {
        Self {
            attachment_limit_bytes: 8 * 1024 * 1024,
        }
    }
}

pub fn build_transcript_artifact(
    transcript_utf8: &str,
    policy: &ArtifactPolicy,
    fallback_link: Option<String>,
) -> Result<TranscriptArtifact, ArtifactError> {
    let bytes = transcript_utf8.as_bytes().to_vec();
    if bytes.len() <= policy.attachment_limit_bytes {
        return Ok(TranscriptArtifact {
            kind: ArtifactKind::TranscriptText,
            inline_attachment: Some(bytes),
            link_url: fallback_link,
        });
    }

    let link = fallback_link.ok_or(ArtifactError::MissingLink)?;
    Ok(TranscriptArtifact {
        kind: ArtifactKind::TranscriptLink,
        inline_attachment: None,
        link_url: Some(link),
    })
}
