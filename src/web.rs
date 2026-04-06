use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use std::sync::Arc;
use tokio_postgres::Client as PgClient;

const MEETING_HTML: &str = include_str!("../assets/meeting.html");

#[derive(Clone)]
pub struct WebState {
    pub db: Arc<PgClient>,
    pub chunk_storage_dir: String,
}

pub fn create_router(state: WebState) -> Router {
    Router::new()
        .route("/meetings/{meeting_id}", get(meeting_page))
        .route("/api/meetings/{meeting_id}", get(api_meeting))
        .route("/api/meetings/{meeting_id}/transcript", get(api_transcript))
        .route("/api/meetings/{meeting_id}/summary", get(api_summary))
        .route("/api/meetings/{meeting_id}/audio", get(api_audio))
        .with_state(state)
}

// ---------- Response types ----------

#[derive(Serialize)]
struct MeetingResponse {
    id: String,
    title: Option<String>,
    status: String,
    started_at: Option<String>,
    stopped_at: Option<String>,
    duration_seconds: Option<i32>,
}

#[derive(Serialize)]
struct TranscriptSegmentResponse {
    speaker_id: String,
    start_ms: i32,
    end_ms: i32,
    text: String,
    confidence: Option<f64>,
    is_noisy: bool,
}

#[derive(Serialize)]
struct SummaryResponse {
    markdown: Option<String>,
}

// ---------- Handlers ----------

async fn meeting_page(Path(meeting_id): Path<String>) -> impl IntoResponse {
    let html = MEETING_HTML.replace("{{MEETING_ID}}", &meeting_id);
    Html(html)
}

async fn api_meeting(
    State(state): State<WebState>,
    Path(meeting_id): Path<String>,
) -> Result<Json<MeetingResponse>, StatusCode> {
    let row = state
        .db
        .query_opt(
            "SELECT id, title, status, \
             to_char(started_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as started_at, \
             to_char(stopped_at, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as stopped_at, \
             meeting_duration_seconds \
             FROM meetings WHERE id=$1",
            &[&meeting_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(MeetingResponse {
        id: row.get("id"),
        title: row.get("title"),
        status: row.get("status"),
        started_at: row.get("started_at"),
        stopped_at: row.get("stopped_at"),
        duration_seconds: row.get("meeting_duration_seconds"),
    }))
}

async fn api_transcript(
    State(state): State<WebState>,
    Path(meeting_id): Path<String>,
) -> Result<Json<Vec<TranscriptSegmentResponse>>, StatusCode> {
    let rows = state
        .db
        .query(
            "SELECT speaker_id, start_ms, end_ms, text, confidence, is_noisy \
             FROM transcripts \
             WHERE meeting_id=$1 AND NOT is_deleted \
             ORDER BY start_ms",
            &[&meeting_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let segments: Vec<TranscriptSegmentResponse> = rows
        .iter()
        .map(|row| TranscriptSegmentResponse {
            speaker_id: row.get("speaker_id"),
            start_ms: row.get("start_ms"),
            end_ms: row.get("end_ms"),
            text: row.get("text"),
            confidence: row.get("confidence"),
            is_noisy: row.get("is_noisy"),
        })
        .collect();

    Ok(Json(segments))
}

async fn api_summary(
    State(state): State<WebState>,
    Path(meeting_id): Path<String>,
) -> Result<Json<SummaryResponse>, StatusCode> {
    let row = state
        .db
        .query_opt(
            "SELECT markdown FROM summaries WHERE meeting_id=$1 ORDER BY version DESC LIMIT 1",
            &[&meeting_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let markdown = row.map(|r| r.get::<_, String>("markdown"));
    Ok(Json(SummaryResponse { markdown }))
}

async fn api_audio(
    State(state): State<WebState>,
    Path(meeting_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let safe_id = crate::storage_fs::sanitize_path_component(&meeting_id);
    let path = std::path::PathBuf::from(&state.chunk_storage_dir)
        .join(&safe_id)
        .join("mixdown.wav");

    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let file_size = metadata.len();

    // Parse Range header
    if let Some(range_header) = headers.get(header::RANGE) {
        let range_str = range_header.to_str().map_err(|_| StatusCode::BAD_REQUEST)?;
        if let Some((start, end)) = parse_range(range_str, file_size) {
            let length = end - start + 1;
            let data = read_file_range(&path, start, length).await?;

            let content_range = format!("bytes {start}-{end}/{file_size}");
            let response = Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, "audio/wav")
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CONTENT_LENGTH, length.to_string())
                .header(header::CONTENT_RANGE, content_range)
                .body(axum::body::Body::from(data))
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            return Ok(response);
        }
    }

    // Full file response
    let data = tokio::fs::read(&path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "audio/wav")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, file_size.to_string())
        .body(axum::body::Body::from(data))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

// ---------- Helpers ----------

fn parse_range(range_str: &str, file_size: u64) -> Option<(u64, u64)> {
    let range_str = range_str.strip_prefix("bytes=")?;
    let mut parts = range_str.splitn(2, '-');
    let start_str = parts.next()?.trim();
    let end_str = parts.next()?.trim();

    if start_str.is_empty() {
        // Suffix range: bytes=-500
        let suffix_len: u64 = end_str.parse().ok()?;
        let start = file_size.saturating_sub(suffix_len);
        Some((start, file_size - 1))
    } else {
        let start: u64 = start_str.parse().ok()?;
        let end = if end_str.is_empty() {
            file_size - 1
        } else {
            end_str.parse().ok()?
        };
        if start > end || start >= file_size {
            return None;
        }
        let end = end.min(file_size - 1);
        Some((start, end))
    }
}

async fn read_file_range(
    path: &std::path::Path,
    start: u64,
    length: u64,
) -> Result<Vec<u8>, StatusCode> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut buf = vec![0u8; length as usize];
    file.read_exact(&mut buf)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(buf)
}
