use serde::Deserialize;
use chrono;

#[derive(Deserialize, Clone, Debug)]
struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<OAuthInfo>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OAuthInfo {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct UsageResponse {
    pub five_hour: Option<RateLimitEntry>,
    pub seven_day: Option<RateLimitEntry>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct RateLimitEntry {
    pub utilization: f64,
    pub resets_at: String,
}


#[derive(Clone, Debug)]
pub struct RateLimitState {
    pub usage: UsageResponse,
    pub fetched_at: chrono::DateTime<chrono::Local>,
    pub error: Option<String>,
}

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const BETA_HEADER: &str = "oauth-2025-04-20";

fn creds_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join(".credentials.json"))
}

fn read_credentials() -> Result<OAuthInfo, String> {
    let path = creds_path().ok_or("Home directory not found")?;
    let data = std::fs::read_to_string(&path).map_err(|e| format!("Read creds: {e}"))?;
    let creds: Credentials =
        serde_json::from_str(&data).map_err(|e| format!("Parse creds: {e}"))?;
    creds
        .claude_ai_oauth
        .ok_or_else(|| "No OAuth credentials".into())
}

fn refresh_token(refresh_tok: &str) -> Result<OAuthInfo, String> {
    let client = reqwest::blocking::Client::new();
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_tok,
        "client_id": CLIENT_ID,
    });
    let resp = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .map_err(|e| format!("Refresh request: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("Refresh failed: {}", resp.status()));
    }

    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: String,
        expires_in: u64,
    }

    let tok: TokenResponse = resp.json().map_err(|e| format!("Parse token: {e}"))?;
    let expires_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + tok.expires_in * 1000;

    // Update credentials file
    if let Some(path) = creds_path() {
        let new_creds = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": tok.access_token,
                "refreshToken": tok.refresh_token,
                "expiresAt": expires_at,
                "scopes": ["user:inference", "user:mcp_servers", "user:profile", "user:sessions:claude_code"],
                "subscriptionType": "pro",
                "rateLimitTier": "default_claude_ai"
            }
        });
        let _ = std::fs::write(path, serde_json::to_string(&new_creds).unwrap());
    }

    Ok(OAuthInfo {
        access_token: tok.access_token,
        refresh_token: tok.refresh_token,
        expires_at,
    })
}

fn fetch_usage(access_token: &str) -> Result<UsageResponse, String> {
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(USAGE_URL)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("anthropic-beta", BETA_HEADER)
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .map_err(|e| format!("Usage request: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(format!("Usage API {status}: {body}"));
    }

    resp.json::<UsageResponse>()
        .map_err(|e| format!("Parse usage: {e}"))
}

pub fn fetch_rate_limit() -> RateLimitState {
    let now = chrono::Local::now();

    let result = (|| -> Result<UsageResponse, String> {
        let mut oauth = read_credentials()?;

        // Refresh if expiring within 5 minutes
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        if now_ms + 300_000 >= oauth.expires_at {
            oauth = refresh_token(&oauth.refresh_token)?;
        }

        fetch_usage(&oauth.access_token)
    })();

    match result {
        Ok(usage) => RateLimitState {
            usage,
            fetched_at: now,
            error: None,
        },
        Err(e) => RateLimitState {
            usage: UsageResponse::default(),
            fetched_at: now,
            error: Some(e),
        },
    }
}
