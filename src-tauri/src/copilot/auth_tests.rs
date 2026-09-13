use super::tests::{http_fixture, json_response, service};
use super::*;
use serde_json::json;

#[tokio::test]
async fn sign_in_persists_account_discovers_routes_and_refreshes_tokens() {
    let token_response = || {
        json_response(
            200,
            json!({"token":"short-lived-copilot-token",
        "expires_at":unix_now().unwrap() + 3600,"endpoints":{"api":"https://api.githubcopilot.com"}}),
        )
    };
    let (url, capture) = http_fixture(vec![
        json_response(200, json!({"access_token":"github-first", "refresh_token":"refresh-first", "expires_in":3600})),
        json_response(200, json!({"login":"troy-barnes"})),
        token_response(),
        json_response(200, json!({"data":[{"id":"greendale", "supported_endpoints":["/responses"]}]})),
        json_response(200, json!({"access_token":"github-renewed", "refresh_token":"refresh-renewed", "expires_in":3600})),
        token_response(),
    ]).await;
    let root =
        std::env::temp_dir().join(format!("llamaproxy-copilot-auth-{}", random_key().unwrap()));
    fs::create_dir(&root).unwrap();
    let mut service = service(
        Api::with_all_endpoints(url),
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
        },
    );
    service.path = root.join("account.json");
    service.config_path = root.join("config.yaml");
    fs::write(
        &service.config_path,
        "# Keep native accounts\ncodex-api-key:\n  - api-key: native-secret\n",
    )
    .unwrap();
    let mut state = service.state.lock().await;
    service.poll(&mut state).await.unwrap();
    assert!(state.login.is_none());
    assert_eq!(state.account.as_ref().unwrap().login, "troy-barnes");
    let saved = fs::read_to_string(&service.path).unwrap();
    assert!(saved.contains("github-first"));
    assert!(!saved.contains("short-lived-copilot-token"));
    let config = fs::read_to_string(&service.config_path).unwrap();
    assert!(config.contains("# Keep native accounts"));
    assert!(config.contains("copilot/greendale"));
    assert!(config.contains("native-secret"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert!(!config.contains("github-first"));
    assert!(!config.contains("short-lived-copilot-token"));
    service.token(&mut state).await.unwrap();
    service.token(&mut state).await.unwrap();

    state.account.as_mut().unwrap().expires_at = Some(0);
    state.token = None;
    service.token(&mut state).await.unwrap();
    let saved = load_account(&service.path).unwrap().unwrap();
    assert_eq!(saved.access_token, "github-renewed");
    assert_eq!(saved.refresh_token.as_deref(), Some("refresh-renewed"));
    let requests = capture.await.unwrap();
    assert_eq!(requests.len(), 6);
    assert!(requests[4].contains("refresh_token=refresh-first"));
    assert!(requests[5].contains("authorization: token github-renewed"));

    service.commit(&mut state, None).unwrap();
    assert!(load_account(&service.path).unwrap().is_none());
    assert!(state.token.is_none());
    let config = fs::read_to_string(&service.config_path).unwrap();
    assert!(config.contains("native-secret"));
    assert!(!config.contains("copilot/greendale"));
    assert!(service.models.read().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn invalid_routing_config_rolls_back_credentials_and_preserves_current_account() {
    let root = std::env::temp_dir().join(format!(
        "llamaproxy-copilot-rollback-{}",
        random_key().unwrap()
    ));
    fs::create_dir(&root).unwrap();
    let account = Account {
        login: "troy-barnes".into(),
        access_token: "original-token".into(),
        refresh_token: None,
        expires_at: None,
        models: vec![],
    };
    let mut service = service(
        Api::new().unwrap(),
        State {
            account: Some(account.clone()),
            token: None,
            login: None,
        },
    );
    service.path = root.join("account.json");
    service.config_path = root.join("config.yaml");
    save_account(&service.path, Some(&account)).unwrap();
    fs::write(&service.config_path, "codex-api-key: not-a-list\n").unwrap();
    let mut state = service.state.lock().await;
    let replacement = Account {
        login: "abed-nadir".into(),
        access_token: "replacement-token".into(),
        ..account
    };
    assert!(service.commit(&mut state, Some(replacement)).is_err());
    assert_eq!(
        load_account(&service.path).unwrap().unwrap().access_token,
        "original-token"
    );
    assert_eq!(
        state.account.as_ref().unwrap().access_token,
        "original-token"
    );
    assert_eq!(
        fs::read_to_string(&service.config_path).unwrap(),
        "codex-api-key: not-a-list\n"
    );
    fs::remove_dir_all(root).unwrap();
}
