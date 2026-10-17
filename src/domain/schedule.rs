use chrono::{DateTime, Utc};

/// A weekly recording schedule and its next occurrence, stored as absolute
/// instants so the runtime can compare it safely with UTC wall clock time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingSchedule {
    pub id: String,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub report_channel_id: String,
    pub created_by_user_id: String,
    pub timezone: String,
    pub local_start_time: String,
    pub next_start_at: DateTime<Utc>,
    pub duration_seconds: u32,
    pub active: bool,
}

pub fn validate_schedule_window(
    timezone: &str,
    local_start_time: &str,
    next_start_at: DateTime<Utc>,
    duration_seconds: u32,
    now: DateTime<Utc>,
) -> Result<(), &'static str> {
    if timezone.trim().is_empty() || timezone.len() > 100 {
        return Err("timezone is invalid");
    }
    let parts: Vec<_> = local_start_time.split(':').collect();
    if parts.len() != 2 || parts[0].parse::<u8>().is_err() || parts[1].parse::<u8>().is_err() {
        return Err("local start time must use HH:MM");
    }
    let hour = parts[0]
        .parse::<u8>()
        .map_err(|_| "local start time must use HH:MM")?;
    let minute = parts[1]
        .parse::<u8>()
        .map_err(|_| "local start time must use HH:MM")?;
    if hour > 23 || minute > 59 {
        return Err("local start time must use HH:MM");
    }
    if !(60..=86_400).contains(&duration_seconds) {
        return Err("duration must be between 60 seconds and 24 hours");
    }
    if next_start_at <= now {
        return Err("first occurrence must be in the future");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn validates_schedule_window_bounds() {
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        assert!(
            validate_schedule_window(
                "Asia/Tokyo",
                "09:30",
                now + chrono::Duration::days(1),
                3600,
                now
            )
            .is_ok()
        );
        assert!(
            validate_schedule_window(
                "Asia/Tokyo",
                "24:00",
                now + chrono::Duration::days(1),
                3600,
                now
            )
            .is_err()
        );
        assert!(
            validate_schedule_window("UTC", "09:30", now + chrono::Duration::days(1), 30, now)
                .is_err()
        );
    }
}
