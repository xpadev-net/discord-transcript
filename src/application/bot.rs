use crate::application::command::{
    CommandError, PermissionSet, RecordStartRequest, RecordStopRequest, record_start, record_stop,
};
use crate::application::stop::StopOutcome;
use crate::domain::StopReason;
use crate::infrastructure::storage::MeetingStore;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartCommandInput {
    pub meeting_id: String,
    pub guild_id: String,
    pub user_id: String,
    pub command_channel_id: String,
    pub user_voice_channel_id: Option<String>,
    pub permissions: PermissionSet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopCommandInput {
    pub guild_id: String,
    pub reason: StopReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopCommandResult {
    pub meeting_id: String,
    pub outcome: StopOutcome,
    pub message: String,
}

pub struct BotCommandService<S: MeetingStore> {
    pub store: S,
}

impl<S: MeetingStore> BotCommandService<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub fn handle_record_start(
        &mut self,
        input: StartCommandInput,
    ) -> Result<String, CommandError> {
        let result = record_start(
            &mut self.store,
            RecordStartRequest {
                meeting_id: input.meeting_id,
                guild_id: input.guild_id,
                started_by_user_id: input.user_id,
                command_channel_id: input.command_channel_id,
                user_voice_channel_id: input.user_voice_channel_id,
                permissions: input.permissions,
            },
        )?;

        Ok(format!(
            "録音を開始しました: meeting_id={}, vc={}, report_channel={}",
            result.meeting_id, result.voice_channel_id, result.report_channel_id
        ))
    }

    pub fn handle_record_stop(&mut self, input: StopCommandInput) -> Result<String, CommandError> {
        self.handle_record_stop_result(input)
            .map(|result| result.message)
    }

    pub fn handle_record_stop_result(
        &mut self,
        input: StopCommandInput,
    ) -> Result<StopCommandResult, CommandError> {
        let result = record_stop(
            &mut self.store,
            RecordStopRequest {
                guild_id: input.guild_id,
                reason: input.reason,
            },
        )?;
        let message = format!(
            "停止要求を受け付けました: meeting_id={}, outcome={:?}",
            result.meeting_id, result.outcome
        );
        Ok(StopCommandResult {
            meeting_id: result.meeting_id,
            outcome: result.outcome,
            message,
        })
    }
}
