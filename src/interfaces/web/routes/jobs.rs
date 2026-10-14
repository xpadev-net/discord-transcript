use super::super::*;

pub(in crate::interfaces::web) async fn api_list_jobs(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<JobResponse>>, StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    require_current_user_has_rbac_permission(&state, &user_id, RbacPermission::UsageView).await?;
    let query = parse_job_list_query(raw_query.as_deref())?;
    let normalized = normalize_job_list_query(&query)?;
    let rows = state
        .db
        .query(
            LIST_GUILD_JOBS_SQL,
            &[
                &auth.guild_id,
                &normalized.status,
                &normalized.job_type,
                &normalized.limit,
            ],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(rows.iter().map(job_response_from_row).collect()))
}

pub(in crate::interfaces::web) async fn api_retry_job(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(job_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    require_current_user_has_rbac_permission(&state, &user_id, RbacPermission::MeetingReprocess)
        .await?;
    validate_resource_id(&job_id)?;
    let request = parse_job_retry_request_body(&body)?;
    let normalized = normalize_job_retry_request(&request)?;

    let retry_input = state
        .db
        .query_opt(ADMIN_RETRY_JOB_INPUT_SQL, &[&job_id, &auth.guild_id])
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let Some(retry_input) = retry_input else {
        return Err(StatusCode::CONFLICT);
    };
    let meeting_id: String = retry_input
        .try_get("meeting_id")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let voice_channel_id: String = retry_input
        .try_get("voice_channel_id")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let raw_audio_not_cleaned: bool = retry_input
        .try_get("raw_audio_not_cleaned")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let has_recoverable_audio = if !raw_audio_not_cleaned {
        false
    } else {
        let layout = MeetingWorkspaceLayout::new(&state.chunk_storage_dir);
        let primary_audio_dir = layout
            .for_meeting(&auth.guild_id, &voice_channel_id, &meeting_id)
            .audio_dir();
        let legacy_audio_dir = layout.legacy_meeting_dir(&meeting_id);
        let check_meeting_id = meeting_id.clone();
        tokio::task::spawn_blocking(move || {
            crate::application::runtime::has_recoverable_audio_with_legacy_fallback(
                primary_audio_dir,
                legacy_audio_dir,
            )
        })
        .await
        .map_err(|err| {
            warn!(meeting_id = %check_meeting_id, error = %err, "retained audio inspection task failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .map_err(|err| {
            warn!(meeting_id = %meeting_id, error = %err, "failed to inspect retained audio before retry");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
    };
    if !has_recoverable_audio {
        return Ok(api_error_response(
            StatusCode::CONFLICT,
            "retry_input_unavailable",
            "This meeting cannot be retried because its retained audio is unavailable.",
        ));
    }
    let has_recoverable_audio_param = has_recoverable_audio.to_string();

    let row = state
        .db
        .query_opt(
            ADMIN_RETRY_JOB_SQL,
            &[
                &job_id,
                &auth.guild_id,
                &normalized.next_run_at,
                &has_recoverable_audio_param,
            ],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let Some(row) = row else {
        return Err(StatusCode::CONFLICT);
    };
    let response = job_response_from_row(&row);
    if let Some(summary_job_wakeups) = &state.summary_job_wakeups {
        summary_job_wakeups
            .enqueue(response.meeting_id.clone())
            .await;
    }
    record_audit_event(
        &state,
        web_audit_event(
            Some(auth.guild_id.clone()),
            Some(user_id),
            "job.retry",
            "job",
            Some(response.id.clone()),
            audit_request_metadata(&headers, "POST", &format!("/api/guild/jobs/{job_id}/retry")),
            json!({
                "meeting_id": response.meeting_id.clone(),
                "job_type": response.job_type.clone(),
                "status": response.status.clone(),
                "next_run_at": response.next_run_at.clone(),
            }),
        ),
    )
    .await;
    Ok(Json(response).into_response())
}

pub(in crate::interfaces::web) async fn api_cancel_job(
    State(state): State<WebState>,
    Extension(AuthUserId(user_id)): Extension<AuthUserId>,
    Path(job_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<JobResponse>, StatusCode> {
    let auth = state.auth.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    require_current_user_has_rbac_permission(&state, &user_id, RbacPermission::MeetingReprocess)
        .await?;
    validate_resource_id(&job_id)?;
    let request = parse_job_cancel_request_body(&body)?;
    let normalized = normalize_job_cancel_request(&request)?;
    let row = state
        .db
        .query_opt(
            ADMIN_CANCEL_JOB_SQL,
            &[&job_id, &auth.guild_id, &normalized.reason],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let Some(row) = row else {
        return Err(StatusCode::CONFLICT);
    };
    let response = job_response_from_row(&row);
    record_audit_event(
        &state,
        web_audit_event(
            Some(auth.guild_id.clone()),
            Some(user_id),
            "job.cancel",
            "job",
            Some(response.id.clone()),
            audit_request_metadata(
                &headers,
                "POST",
                &format!("/api/guild/jobs/{job_id}/cancel"),
            ),
            json!({
                "meeting_id": response.meeting_id.clone(),
                "job_type": response.job_type.clone(),
                "status": response.status.clone(),
                "reason_set": response.cancel_reason.is_some(),
            }),
        ),
    )
    .await;
    Ok(Json(response))
}
