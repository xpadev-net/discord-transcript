use regex::{Captures, Regex};
use std::collections::HashMap;
use std::sync::LazyLock;

static EMAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[a-z0-9._%+\-]+@[a-z0-9.\-]+\.[a-z]{2,}\b")
        .expect("email regex must compile")
});
static DISCORD_MENTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<@!?\d+>|<@&\d+>").expect("discord mention regex must compile"));
static AT_USERNAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)(?P<mention>@[A-Za-z0-9_]{2,32})").expect("at mention regex")
});
static PHONE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\+?\d[\d\-\s().]{8,}\d").expect("phone regex must compile"));

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MaskingStats {
    pub mention_replacements: usize,
    pub email_replacements: usize,
    pub phone_replacements: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaskedText {
    pub text: String,
    pub stats: MaskingStats,
}

pub fn mask_pii(input: &str) -> MaskedText {
    let mut stats = MaskingStats::default();
    let mut value = input.to_owned();

    let mut email_tokens: HashMap<String, usize> = HashMap::new();
    let mut mention_tokens: HashMap<String, usize> = HashMap::new();
    let mut phone_tokens: HashMap<String, usize> = HashMap::new();
    let mut email_counter = 0usize;
    let mut mention_counter = 0usize;
    let mut phone_counter = 0usize;

    value = replace_with_registry(
        &value,
        &EMAIL_RE,
        "EMAIL",
        &mut email_tokens,
        &mut email_counter,
        &mut stats.email_replacements,
        |_| true,
    );

    value = replace_with_registry(
        &value,
        &DISCORD_MENTION_RE,
        "USER",
        &mut mention_tokens,
        &mut mention_counter,
        &mut stats.mention_replacements,
        |_| true,
    );

    value = replace_with_registry(
        &value,
        &AT_USERNAME_RE,
        "USER",
        &mut mention_tokens,
        &mut mention_counter,
        &mut stats.mention_replacements,
        |caps| {
            let raw = caps.get(0).expect("full match should exist").as_str();
            !raw.starts_with("@EMAIL") && !raw.starts_with("@USER")
        },
    );

    value = replace_with_registry(
        &value,
        &PHONE_RE,
        "PHONE",
        &mut phone_tokens,
        &mut phone_counter,
        &mut stats.phone_replacements,
        |caps| {
            let m = caps.get(0).expect("full match should exist");
            let raw = m.as_str();
            if count_digits(raw) < 10 {
                return false;
            }
            // Exclude timestamp patterns like [123-456] used in transcript format
            let start = m.start();
            let end = m.end();
            let bytes = value.as_bytes();
            if start > 0 && end < bytes.len() && bytes[start - 1] == b'[' && bytes[end] == b']' {
                return false;
            }
            true
        },
    );

    MaskedText { text: value, stats }
}

fn replace_with_registry<F>(
    input: &str,
    regex: &Regex,
    prefix: &str,
    registry: &mut HashMap<String, usize>,
    next_index: &mut usize,
    replacement_count: &mut usize,
    filter: F,
) -> String
where
    F: Fn(&Captures<'_>) -> bool,
{
    regex
        .replace_all(input, |caps: &Captures<'_>| {
            if !filter(caps) {
                return caps
                    .get(0)
                    .expect("full match should exist")
                    .as_str()
                    .to_owned();
            }

            *replacement_count += 1;
            let raw = caps
                .get(0)
                .expect("full match should exist")
                .as_str()
                .to_owned();
            let index = registry.entry(raw).or_insert_with(|| {
                *next_index += 1;
                *next_index
            });
            format!("[{prefix}_{index}]")
        })
        .into_owned()
}

fn count_digits(input: &str) -> usize {
    input.chars().filter(|ch| ch.is_ascii_digit()).count()
}
