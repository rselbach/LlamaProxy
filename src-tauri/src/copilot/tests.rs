use super::*;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn model(id: &str, endpoint: Endpoint) -> Model {
    Model {
        id: id.into(),
        endpoint,
    }
}

#[test]
fn model_catalog_uses_advertised_protocols_and_excludes_non_chat_models() {
    let models = api::parse_models(&json!({"data": [
        {"id":"troy-chat", "supported_endpoints":["/chat/completions"], "capabilities":{"type":"chat"}},
        {"id":"abed-responses", "supported_endpoints":["/chat/completions", "/responses"]},
        {"id":"annie-messages", "supported_endpoints":["/v1/messages"]},
        {"id":"embedding", "supported_endpoints":["/chat/completions"], "capabilities":{"type":"embeddings"}},
        {"id":"unknown"}, {"id":"untrusted/model", "supported_endpoints":["/responses"]},
        {"id":"troy-chat", "supported_endpoints":["/chat/completions"]}
    ]})).unwrap();
    assert_eq!(
        models,
        vec![
            model("abed-responses", Endpoint::Responses),
            model("annie-messages", Endpoint::Messages),
            model("troy-chat", Endpoint::Chat)
        ]
    );
    assert!(api::parse_models(&json!({"data":[]})).is_err());
    assert!(api::parse_models(&json!({})).is_err());
}

#[test]
fn endpoint_validation_prevents_credential_exfiltration() {
    for endpoint in [
        "https://api.githubcopilot.com",
        "https://api.business.githubcopilot.com/",
    ] {
        assert!(api::validate_endpoint(endpoint).is_ok(), "{endpoint}");
    }
    for endpoint in [
        "http://api.githubcopilot.com",
        "https://githubcopilot.com.evil.test",
        "https://api.githubcopilot.com@evil.test",
        "https://user@api.githubcopilot.com",
        "https://api.githubcopilot.com:8443",
        "https://127.0.0.1",
        "https://api.githubcopilot.com/path",
        "https://api.githubcopilot.com?token=secret",
        "https://api.githubcopilot.com#fragment",
    ] {
        assert!(api::validate_endpoint(endpoint).is_err(), "{endpoint}");
    }
}

#[test]
fn routes_are_idempotent_namespaced_and_preserve_user_entries() {
    let mut config: serde_norway::Value = serde_norway::from_str("port: 11432\nopenai-compatibility:\n  - name: Greendale\n    base-url: https://example.com\n    models: []\ncodex-api-key:\n  - api-key: native-secret\nclaude-api-key: []\n").unwrap();
    let original = config.clone();
    let models = vec![
        model("troy", Endpoint::Chat),
        model("abed", Endpoint::Responses),
        model("annie", Endpoint::Messages),
    ];
    let url = "http://127.0.0.1:4321/llamaproxy-copilot";
    assert!(config::configure(&mut config, url, "local-key", &models).unwrap());
    assert!(!config::configure(&mut config, url, "local-key", &models).unwrap());
    assert_eq!(
        config["openai-compatibility"][0],
        original["openai-compatibility"][0]
    );
    assert_eq!(config["codex-api-key"][0], original["codex-api-key"][0]);
    assert_eq!(
        config["codex-api-key"][1]["models"][0]["alias"].as_str(),
        Some("copilot/abed")
    );
    assert_eq!(
        config["claude-api-key"][0]["models"][0]["alias"].as_str(),
        Some("copilot/annie")
    );
    assert_eq!(
        config["openai-compatibility"][1]["models"][0]["alias"].as_str(),
        Some("copilot/troy")
    );
    let new_url = "http://127.0.0.1:5678/llamaproxy-copilot";
    assert!(config::configure(&mut config, new_url, "rotated-key", &models).unwrap());
    assert_eq!(config["codex-api-key"].as_sequence().unwrap().len(), 2);
    assert!(config::configure(&mut config, new_url, "rotated-key", &[]).unwrap());
    assert_eq!(config, original);
}

#[test]
fn route_patch_preserves_yaml_comments_and_rejects_malformed_sections() {
    let source = "# Greendale configuration\nport: 11432 # private\nopenai-compatibility: []\n";
    let models = [model("troy", Endpoint::Chat)];
    let updated = crate::patch_core_yaml_document(source, |document| {
        config::configure(
            document,
            "http://127.0.0.1:4321/llamaproxy-copilot",
            "key",
            &models,
        )
    })
    .unwrap()
    .unwrap();
    assert!(updated.contains("# Greendale configuration"));
    assert!(updated.contains("# private"));
    let mut invalid = serde_norway::from_str("codex-api-key: broken\n").unwrap();
    assert!(config::configure(&mut invalid, "url", "key", &models).is_err());
}

#[test]
fn account_storage_is_private_atomic_and_cleared_on_disconnect() {
    let root = std::env::temp_dir().join(format!("llamaproxy-copilot-{}", random_key().unwrap()));
    fs::create_dir(&root).unwrap();
    let path = root.join("account.json");
    assert!(load_account(&path).unwrap().is_none());
    let account = Account {
        login: "troy-barnes".into(),
        access_token: "test-github-secret".into(),
        refresh_token: None,
        expires_at: None,
        models: vec![model("troy", Endpoint::Chat)],
    };
    save_account(&path, Some(&account)).unwrap();
    assert_eq!(load_account(&path).unwrap().unwrap().login, "troy-barnes");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    save_account(&path, None).unwrap();
    assert!(load_account(&path).unwrap().is_none());
    assert!(!fs::read_to_string(&path)
        .unwrap()
        .contains("test-github-secret"));
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    fs::remove_dir_all(root).unwrap();
}

// Real loopback HTTP fixtures: production reqwest encoding/decoding is exercised.
pub(super) async fn http_fixture(
    responses: Vec<String>,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for response in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let end = loop {
                let mut chunk = [0u8; 4096];
                let count = socket.read(&mut chunk).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
                if let Some(end) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let header = String::from_utf8_lossy(&bytes[..end]);
            let length = header
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                .unwrap_or(0);
            let received = bytes.len();
            bytes.resize(end + length, 0);
            socket.read_exact(&mut bytes[received..]).await.unwrap();
            requests.push(String::from_utf8(bytes).unwrap());
            socket.write_all(response.as_bytes()).await.unwrap();
        }
        requests
    });
    (url, task)
}

pub(super) fn json_response(status: u16, body: serde_json::Value) -> String {
    let body = body.to_string();
    format!("HTTP/1.1 {status} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

fn state_with_login() -> State {
    State {
        account: None,
        token: None,
        login: Some(Login {
            cancel: CancellationToken::new(),
            device: DeviceCode {
                device_code: "private-device-code".into(),
                user_code: "TROY-ABED".into(),
                verification_uri: "https://github.com/login/device".into(),
                expires_in: 900,
                interval: 5,
            },
            expires_at: Instant::now() + Duration::from_secs(900),
            next_poll: Instant::now(),
        }),
    }
}

pub(super) fn service(api: Api, state: State) -> Copilot {
    let models = state
        .account
        .as_ref()
        .map(|a| a.models.clone())
        .unwrap_or_default();
    let cancel = state
        .login
        .as_ref()
        .map(|login| login.cancel.clone())
        .unwrap_or_default();
    Copilot {
        api,
        login_cancel: Mutex::new(cancel),
        state: AsyncMutex::new(state),
        models: RwLock::new(models),
        path: PathBuf::from("unused-copilot-test-account"),
        config_path: PathBuf::from("unused-copilot-test-config"),
        base_url: "http://127.0.0.1:1/llamaproxy-copilot".into(),
        key: "local-test-key".into(),
    }
}

#[tokio::test]
async fn device_polling_enforces_interval_slowdown_denial_and_expiry() {
    let (url, task) = http_fixture(vec![
        json_response(200, json!({"error":"authorization_pending"})),
        json_response(200, json!({"error":"slow_down"})),
        json_response(200, json!({"error":"access_denied"})),
    ])
    .await;
    let service = service(Api::with_github_endpoint(url), state_with_login());
    let mut state = service.state.lock().await;
    service.poll(&mut state).await.unwrap();
    let next_poll = state.login.as_ref().unwrap().next_poll;
    service.poll(&mut state).await.unwrap();
    assert_eq!(state.login.as_ref().unwrap().next_poll, next_poll);
    state.login.as_mut().unwrap().next_poll = Instant::now();
    service.poll(&mut state).await.unwrap();
    assert_eq!(state.login.as_ref().unwrap().device.interval, 10);
    state.login.as_mut().unwrap().next_poll = Instant::now();
    assert!(service
        .poll(&mut state)
        .await
        .unwrap_err()
        .contains("denied"));
    assert!(state.login.is_none());
    let requests = task.await.unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].contains("device_code=private-device-code"));
    let mut expired = state_with_login();
    expired.login.as_mut().unwrap().expires_at = Instant::now();
    assert!(service
        .poll(&mut expired)
        .await
        .unwrap_err()
        .contains("expired"));
    assert!(expired.login.is_none());
}

#[tokio::test]
async fn device_login_and_token_exchange_validate_responses_without_leaking_secrets() {
    let (url, task) = http_fixture(vec![
        json_response(
            200,
            json!({"device_code":"private-device-code", "user_code":"TROY-ABED",
            "verification_uri":"https://github.com/login/device", "expires_in":900, "interval":1}),
        ),
        json_response(
            200,
            json!({"token":"private-copilot-token", "expires_at":unix_now().unwrap() + 1800,
            "endpoints":{"api":"https://api.business.githubcopilot.com"}}),
        ),
        json_response(403, json!({"message":"secret-github-token"})),
    ])
    .await;
    let api = Api::with_github_endpoint(url);
    assert_eq!(api.start_device_login().await.unwrap().interval, 5);
    let token = api.exchange("secret-github-token").await.unwrap();
    assert_eq!(token.endpoint, "https://api.business.githubcopilot.com");
    assert!(token.refresh_at > Instant::now());
    let error = api.exchange("secret-github-token").await.err().unwrap();
    assert!(error.contains("403"));
    assert!(!error.contains("secret-github-token"));
    let requests = task.await.unwrap();
    assert!(requests[0].contains("client_id=Iv1.b507a08c87ecfe98"));
    assert!(requests[1].contains("authorization: token secret-github-token"));
}

#[test]
fn public_status_never_serializes_private_credentials() {
    let mut state = state_with_login();
    state.account = Some(Account {
        login: "troy-barnes".into(),
        access_token: "private-github-token".into(),
        refresh_token: Some("private-refresh-token".into()),
        expires_at: None,
        models: vec![model("troy", Endpoint::Chat)],
    });
    let service = service(Api::new().unwrap(), state);
    let state = service.state.try_lock().unwrap();
    let status = serde_json::to_string(&service.status(&state)).unwrap();
    assert!(status.contains("TROY-ABED"));
    assert!(status.contains("copilot/troy"));
    for secret in [
        "private-device-code",
        "private-github-token",
        "private-refresh-token",
    ] {
        assert!(!status.contains(secret));
    }
}

#[tokio::test]
async fn cancel_sign_in_interrupts_a_pending_network_poll_without_saving_credentials() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let service = Arc::new(service(
        Api::with_github_endpoint(format!("http://{}", listener.local_addr().unwrap())),
        state_with_login(),
    ));
    let polling = service.clone();
    let task = tokio::spawn(async move {
        let mut state = polling.state.lock().await;
        polling.poll(&mut state).await.unwrap();
        assert!(state.account.is_none());
        assert!(state.login.is_none());
    });
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut buffer = [0u8; 2048];
    assert!(socket.read(&mut buffer).await.unwrap() > 0);
    service.cancel_login().unwrap();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    let mut remaining = Vec::new();
    tokio::time::timeout(Duration::from_secs(1), socket.read_to_end(&mut remaining))
        .await
        .unwrap()
        .unwrap();
}
