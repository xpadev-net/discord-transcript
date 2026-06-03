#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}
