#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConfidencePermille(u16);

impl ConfidencePermille {
    pub fn new(value: u16) -> Result<Self, String> {
        if value <= 1000 {
            Ok(Self(value))
        } else {
            Err(format!(
                "confidence must be between 0 and 1000 permille: {value}"
            ))
        }
    }

    pub fn as_permille(self) -> u16 {
        self.0
    }

    pub fn as_sql_decimal(self) -> String {
        format!("{}.{:03}", self.0 / 1000, self.0 % 1000)
    }

    pub fn parse_sql_decimal(value: &str) -> Result<Self, String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("confidence decimal must not be empty".to_owned());
        }

        let (whole, fractional) = trimmed
            .split_once('.')
            .ok_or_else(|| format!("confidence decimal must contain a decimal point: {value}"))?;
        if !matches!(whole, "0" | "1") {
            return Err(format!(
                "confidence decimal whole part must be 0 or 1: {value}"
            ));
        }
        if fractional.is_empty()
            || fractional.len() > 3
            || !fractional.chars().all(|ch| ch.is_ascii_digit())
        {
            return Err(format!(
                "confidence decimal must have 1-3 fractional digits containing only digits: {value}"
            ));
        }
        if whole == "1" && fractional.chars().any(|ch| ch != '0') {
            return Err(format!("confidence decimal must be <= 1.000: {value}"));
        }

        let mut permille = if whole == "1" { 1000 } else { 0 };
        if whole == "0" {
            let padded = format!("{fractional:0<3}");
            permille = padded
                .parse::<u16>()
                .map_err(|err| format!("confidence decimal is invalid: {err}"))?;
        }
        Self::new(permille)
    }
}
