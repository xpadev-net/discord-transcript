import { formatDate } from "./formatters";

export const MEETING_TITLE_DISPLAY_MAX_CHARS = 80;

export interface MeetingTitleFields {
  title: string | null;
  started_at: string | null;
  voice_channel_id: string;
  voice_channel_name: string | null;
}

export function sanitizeMeetingTitle(title: string | null | undefined): string {
  const normalized = title?.trim() ?? "";
  if (
    normalized &&
    Array.from(normalized).length <= MEETING_TITLE_DISPLAY_MAX_CHARS &&
    !hasControlCharacter(normalized)
  ) {
    return normalized;
  }
  return "";
}

export function displayMeetingTitle(meeting: MeetingTitleFields): string {
  const title = sanitizeMeetingTitle(meeting.title);
  if (title) {
    return title;
  }
  const channel =
    meeting.voice_channel_name?.trim() || `VC ID: ${meeting.voice_channel_id}`;
  return meeting.started_at
    ? `${formatDate(meeting.started_at)} ${channel}`
    : channel;
}

export function hasControlCharacter(value: string): boolean {
  return Array.from(value).some((character) => {
    const code = character.codePointAt(0) ?? 0;
    return code < 32 || (code >= 127 && code <= 159);
  });
}
