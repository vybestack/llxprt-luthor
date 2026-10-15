use super::error::ConfigError;

pub(crate) fn validate_repository(value: &str) -> Result<(), ConfigError> {
    let parts: Vec<_> = value.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || !part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        })
    {
        return Err(ConfigError::Invalid("invalid repository name".into()));
    }
    Ok(())
}
