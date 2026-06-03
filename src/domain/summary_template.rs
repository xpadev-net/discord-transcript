use chrono::{DateTime, Utc};

pub const SUMMARY_TEMPLATE_VARIABLE_TRANSCRIPT_PATH: &str = "transcript_path";
pub const SUMMARY_TEMPLATE_VARIABLE_MANIFEST_PATH: &str = "manifest_path";
pub const SUMMARY_TEMPLATE_VARIABLE_LANGUAGE: &str = "language";
pub const SUMMARY_TEMPLATE_VARIABLE_SPEAKER_ROSTER: &str = "speaker_roster";
pub const SUMMARY_TEMPLATE_VARIABLE_DOMAIN_CONTEXT_PATH: &str = "domain_context_path";

pub const ALLOWED_SUMMARY_TEMPLATE_VARIABLES: &[&str] = &[
    SUMMARY_TEMPLATE_VARIABLE_TRANSCRIPT_PATH,
    SUMMARY_TEMPLATE_VARIABLE_MANIFEST_PATH,
    SUMMARY_TEMPLATE_VARIABLE_LANGUAGE,
    SUMMARY_TEMPLATE_VARIABLE_SPEAKER_ROSTER,
    SUMMARY_TEMPLATE_VARIABLE_DOMAIN_CONTEXT_PATH,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryTemplate {
    pub id: String,
    pub tenant_id: Option<String>,
    pub guild_id: String,
    pub name: String,
    pub template: String,
    pub active: bool,
    pub version: u32,
    pub updated_actor_user_id: Option<String>,
    pub archived_at: Option<DateTime<Utc>>,
    pub archived_actor_user_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSummaryTemplate {
    pub id: String,
    pub guild_id: String,
    pub name: String,
    pub template: String,
    pub active: bool,
    pub updated_actor_user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateSummaryTemplate {
    pub id: String,
    pub guild_id: String,
    pub name: String,
    pub template: String,
    pub active: Option<bool>,
    pub updated_actor_user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummaryTemplateValidationError {
    Empty,
    TooLarge,
    UnclosedVariable,
    EmptyVariable,
    UnknownVariable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryTemplateVariables {
    pub transcript_path: String,
    pub manifest_path: String,
    pub language: String,
    pub speaker_roster: String,
    pub domain_context_path: String,
}

impl SummaryTemplateVariables {
    fn value_for(&self, name: &str) -> Option<&str> {
        match name {
            SUMMARY_TEMPLATE_VARIABLE_TRANSCRIPT_PATH => Some(&self.transcript_path),
            SUMMARY_TEMPLATE_VARIABLE_MANIFEST_PATH => Some(&self.manifest_path),
            SUMMARY_TEMPLATE_VARIABLE_LANGUAGE => Some(&self.language),
            SUMMARY_TEMPLATE_VARIABLE_SPEAKER_ROSTER => Some(&self.speaker_roster),
            SUMMARY_TEMPLATE_VARIABLE_DOMAIN_CONTEXT_PATH => Some(&self.domain_context_path),
            _ => None,
        }
    }
}

pub fn validate_summary_template(template: &str) -> Result<(), SummaryTemplateValidationError> {
    if template.trim().is_empty() {
        return Err(SummaryTemplateValidationError::Empty);
    }
    if template.len() > 20_000 {
        return Err(SummaryTemplateValidationError::TooLarge);
    }
    visit_summary_template_variables(template, |_| Ok(()))
}

pub fn render_summary_template(
    template: &str,
    values: &SummaryTemplateVariables,
) -> Result<String, SummaryTemplateValidationError> {
    validate_summary_template(template)?;

    let mut rendered = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        rendered.push_str(&rest[..start]);
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find("}}") else {
            return Err(SummaryTemplateValidationError::UnclosedVariable);
        };
        let name = after_start[..end].trim();
        let value = values
            .value_for(name)
            .ok_or_else(|| SummaryTemplateValidationError::UnknownVariable(name.to_owned()))?;
        rendered.push_str(value);
        rest = &after_start[end + 2..];
    }
    rendered.push_str(rest);
    Ok(rendered)
}

pub fn summary_template_variables(
    template: &str,
) -> Result<Vec<String>, SummaryTemplateValidationError> {
    let mut variables = Vec::new();
    visit_summary_template_variables(template, |name| {
        if !variables.iter().any(|existing| existing == name) {
            variables.push(name.to_owned());
        }
        Ok(())
    })?;
    Ok(variables)
}

fn visit_summary_template_variables<F>(
    template: &str,
    mut visitor: F,
) -> Result<(), SummaryTemplateValidationError>
where
    F: FnMut(&str) -> Result<(), SummaryTemplateValidationError>,
{
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find("}}") else {
            return Err(SummaryTemplateValidationError::UnclosedVariable);
        };
        let name = after_start[..end].trim();
        if name.is_empty() {
            return Err(SummaryTemplateValidationError::EmptyVariable);
        }
        if !ALLOWED_SUMMARY_TEMPLATE_VARIABLES.contains(&name) {
            return Err(SummaryTemplateValidationError::UnknownVariable(
                name.to_owned(),
            ));
        }
        visitor(name)?;
        rest = &after_start[end + 2..];
    }
    Ok(())
}
