//! Bootstrap quota once per cycle before optional polling.
pub(crate) fn refresh(executable: &std::path::Path) -> std::io::Result<()> {
    // GitHub's rate_limit endpoint does not consume core quota.
    let output = super::read::api(executable, &["api", "rate_limit"])?;
    if !output.status.success() {
        return Err(std::io::Error::other("GitHub quota read failed"));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    super::budget::set_quota(&value)
}
