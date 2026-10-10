use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Measurement {
    pub key: String,
    pub value: usize,
    pub limit: usize,
}

impl Measurement {
    pub fn diagnostic(&self) -> String {
        let metric = self.key.rsplit(':').next().unwrap_or("metric");
        format!("{}: {metric}={} limit={}", self.key, self.value, self.limit)
    }
}
