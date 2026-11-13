use serde::{Deserialize, Serialize};

/// Connect session tokens issued by coincube-api and persisted per network in
/// the Connect cache file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessTokenResponse {
    pub access_token: String,
    pub expires_at: i64,
    pub refresh_token: String,
}
