use super::{random_key, unix_now, Endpoint, Model};
use futures_util::StreamExt;
use reqwest::{Client, Response};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::Value;
use std::time::{Duration, Instant};

// Public OAuth client identifier used by GitHub's Copilot device flow, not a secret.
const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const USER_AGENT: &str = concat!(
    "LlamaProxy/",
    env!("CARGO_PKG_VERSION"),
    " GitHubCopilotChat/0.35.0"
);

pub(super) struct Api {
    client: Client,
    github: String,
    github_api: String,
    #[cfg(test)]
    copilot_endpoint: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_interval() -> u64 {
    5
}

#[derive(Deserialize)]
pub(super) struct OAuthToken {
    #[serde(default)]
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub error: String,
}

#[derive(Clone)]
pub(super) struct Token {
    pub secret: String,
    pub endpoint: String,
    pub refresh_at: Instant,
}

#[derive(Deserialize)]
struct TokenResponse {
    token: String,
    expires_at: u64,
    refresh_in: Option<u64>,
    endpoints: Option<TokenEndpoints>,
}

#[derive(Deserialize)]
struct TokenEndpoints {
    api: Option<String>,
}

#[derive(Deserialize)]
struct User {
    login: String,
}

impl Api {
    pub fn new() -> Result<Self, String> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(120))
            .user_agent(USER_AGENT)
            .build()
            .map_err(|_| "Failed to initialize the Copilot HTTP client".to_string())?;
        Ok(Self {
            client,
            github: "https://github.com".into(),
            github_api: "https://api.github.com".into(),
            #[cfg(test)]
            copilot_endpoint: None,
        })
    }

    pub async fn start_device_login(&self) -> Result<DeviceCode, String> {
        let response = self
            .client
            .post(format!("{}/login/device/code", self.github))
            .header("Accept", "application/json")
            .form(&[("client_id", CLIENT_ID)])
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| "Could not reach GitHub to start sign-in".to_string())?;
        let mut device: DeviceCode = read_json(response, "GitHub device sign-in").await?;
        if device.device_code.is_empty()
            || device.user_code.is_empty()
            || device.verification_uri != "https://github.com/login/device"
            || device.expires_in == 0
        {
            return Err("GitHub returned an invalid device sign-in response".into());
        }
        device.interval = device.interval.clamp(5, 900);
        device.expires_in = device.expires_in.min(900);
        Ok(device)
    }

    pub async fn poll_device_login(&self, code: &str) -> Result<OAuthToken, String> {
        self.oauth(&[
            ("client_id", CLIENT_ID),
            ("device_code", code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ])
        .await
    }

    pub async fn refresh_github_token(&self, token: &str) -> Result<OAuthToken, String> {
        self.oauth(&[
            ("client_id", CLIENT_ID),
            ("refresh_token", token),
            ("grant_type", "refresh_token"),
        ])
        .await
    }

    async fn oauth(&self, form: &[(&str, &str)]) -> Result<OAuthToken, String> {
        let response = self
            .client
            .post(format!("{}/login/oauth/access_token", self.github))
            .header("Accept", "application/json")
            .form(form)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| "Could not reach GitHub to complete sign-in".to_string())?;
        read_json(response, "GitHub authorization").await
    }

    pub async fn login_name(&self, secret: &str) -> Result<String, String> {
        let response = self
            .client
            .get(format!("{}/user", self.github_api))
            .bearer_auth(secret)
            .header("Accept", "application/vnd.github+json")
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| "Could not retrieve the GitHub account".to_string())?;
        let user: User = read_json(response, "GitHub account lookup").await?;
        if user.login.is_empty() {
            return Err("GitHub returned an empty account name".into());
        }
        Ok(user.login)
    }

    pub async fn exchange(&self, github_token: &str) -> Result<Token, String> {
        let response = self
            .client
            .get(format!("{}/copilot_internal/v2/token", self.github_api))
            .header("Authorization", format!("token {github_token}"))
            .header("Accept", "application/vnd.github+json")
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| "Could not exchange the GitHub token for Copilot access".to_string())?;
        let token: TokenResponse =
            read_json(response, "Copilot token exchange (check your subscription)").await?;
        let remaining = token.expires_at.saturating_sub(unix_now()?);
        if token.token.is_empty() || remaining == 0 {
            return Err("Copilot returned an empty or expired access token".into());
        }
        let endpoint = validate_endpoint(
            token
                .endpoints
                .and_then(|e| e.api)
                .as_deref()
                .unwrap_or("https://api.githubcopilot.com"),
        )?;
        let lifetime = token
            .refresh_in
            .filter(|n| *n > 0)
            .unwrap_or(remaining)
            .min(remaining);
        Ok(Token {
            secret: token.token,
            #[cfg(not(test))]
            endpoint,
            #[cfg(test)]
            endpoint: self.copilot_endpoint.clone().unwrap_or(endpoint),
            refresh_at: Instant::now() + Duration::from_secs(lifetime.saturating_sub(60)),
        })
    }

    pub async fn models(&self, token: &Token) -> Result<Vec<Model>, String> {
        let response = self
            .request(token, reqwest::Method::GET, "/models", false, "user")?
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| "Could not retrieve Copilot models".to_string())?;
        let value: Value = read_json(response, "Copilot model discovery").await?;
        parse_models(&value)
    }

    pub fn request(
        &self,
        token: &Token,
        method: reqwest::Method,
        path: &str,
        stream: bool,
        initiator: &str,
    ) -> Result<reqwest::RequestBuilder, String> {
        Ok(self
            .client
            .request(method, format!("{}{path}", token.endpoint))
            .bearer_auth(&token.secret)
            .header("Content-Type", "application/json")
            .header(
                "Accept",
                if stream {
                    "text/event-stream"
                } else {
                    "application/json"
                },
            )
            .header("Copilot-Integration-Id", "vscode-chat")
            .header("Editor-Version", "vscode/1.107.0")
            .header("Editor-Plugin-Version", "copilot-chat/0.35.0")
            .header("OpenAI-Intent", "conversation-edits")
            .header("X-Interaction-Type", "conversation-edits")
            .header("X-Initiator", initiator)
            .header("X-Request-Id", random_key()?))
    }

    #[cfg(test)]
    pub fn with_github_endpoint(endpoint: String) -> Self {
        Self {
            client: Client::new(),
            github: endpoint.clone(),
            github_api: endpoint,
            copilot_endpoint: None,
        }
    }

    #[cfg(test)]
    pub fn with_all_endpoints(endpoint: String) -> Self {
        let mut api = Self::with_github_endpoint(endpoint.clone());
        api.copilot_endpoint = Some(endpoint);
        api
    }
}

// Never include remote response bodies or reqwest errors in user-facing auth errors:
// either can echo credentials. HTTP status and operation identify the failure safely.
async fn read_json<T: DeserializeOwned>(response: Response, operation: &str) -> Result<T, String> {
    if !response.status().is_success() {
        return Err(format!(
            "{operation} failed (HTTP {})",
            response.status().as_u16()
        ));
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| format!("Could not read {operation} response"))?;
        if body.len() + chunk.len() > 4 * 1024 * 1024 {
            return Err(format!("{operation} response is too large"));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| format!("{operation} returned invalid JSON"))
}

pub(super) fn validate_endpoint(raw: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "Invalid Copilot API endpoint".to_string())?;
    let host = url.host_str().unwrap_or_default();
    if url.scheme() != "https"
        || !(host == "api.githubcopilot.com" || host.ends_with(".githubcopilot.com"))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|p| p != 443)
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err("Copilot returned an untrusted API endpoint".into());
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

pub(super) fn parse_models(value: &Value) -> Result<Vec<Model>, String> {
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| "Copilot returned an invalid model catalog".to_string())?;
    let mut models = Vec::new();
    for entry in data {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        if id.is_empty()
            || id.len() > 200
            || !id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        {
            continue;
        }
        let kind = entry.pointer("/capabilities/type").and_then(Value::as_str);
        if kind.is_some_and(|kind| kind != "chat") {
            continue;
        }
        let endpoints = entry.get("supported_endpoints").and_then(Value::as_array);
        let supported = |path: &str| {
            endpoints.is_some_and(|list| list.iter().any(|v| v.as_str() == Some(path)))
        };
        let endpoint = if supported("/responses") {
            Endpoint::Responses
        } else if supported("/chat/completions") {
            Endpoint::Chat
        } else if supported("/v1/messages") {
            Endpoint::Messages
        } else {
            // Missing metadata is not evidence that a model accepts chat requests.
            continue;
        };
        models.push(Model {
            id: id.to_string(),
            endpoint,
        });
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    if models.is_empty() {
        return Err("Copilot returned no supported chat models".into());
    }
    Ok(models)
}
