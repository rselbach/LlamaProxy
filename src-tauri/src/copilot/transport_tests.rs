use super::*;
#[path = "core_proof.rs"]
mod core_proof;
use crate::copilot::{
    api::Api,
    tests::{http_fixture, json_response, service},
    Account, Model, State, Token,
};
use serde_json::json;
use std::time::Instant;

fn headers(path: &str, extra: &str) -> Vec<u8> {
    format!("POST /llamaproxy-copilot{path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer local-test-key\r\nContent-Type: application/json\r\n{extra}\r\n").into_bytes()
}

#[test]
fn private_http_boundary_rejects_ambiguous_framing_and_unauthorized_requests() {
    for path in ["/chat/completions", "/responses", "/v1/messages?beta=true"] {
        assert!(parse_headers(&headers(path, "Content-Length: 2\r\n"), "local-test-key").is_ok());
    }
    for extra in [
        "Content-Length: 2\r\nContent-Length: 2\r\n",
        "Transfer-Encoding: chunked\r\n",
        "Content-Length: -1\r\n",
        "Content-Length: +1\r\n",
        "Content-Length: 9999999999999\r\n",
        "Content-Length: 2\r\nAuthorization: Bearer other\r\n",
        "Content-Length: 2\r\nExpect: 100-continue\r\n",
        "Content-Length: 2\r\n folded: header\r\n",
        "",
    ] {
        assert!(
            parse_headers(&headers("/responses", extra), "local-test-key").is_err(),
            "{extra}"
        );
    }
    assert!(parse_headers(&headers("/responses", "Content-Length: 2\r\n"), "wrong-key").is_err());
    assert!(parse_headers(
        &headers("/responses/compact", "Content-Length: 2\r\n"),
        "local-test-key"
    )
    .is_err());
    let claude = String::from_utf8(headers("/v1/messages", "Content-Length: 2\r\n"))
        .unwrap()
        .replace(
            "Authorization: Bearer local-test-key",
            "X-Api-Key: local-test-key",
        );
    assert!(parse_headers(claude.as_bytes(), "local-test-key").is_ok());
}

#[test]
fn continuation_and_image_headers_follow_request_content() {
    assert_eq!(initiator(&json!({"messages":[{"role":"user"}]})), "user");
    assert_eq!(initiator(&json!({"messages":[{"role":"tool"}]})), "agent");
    assert_eq!(
        initiator(&json!({"input":[{"type":"function_call_output"}]})),
        "agent"
    );
    assert!(has_image(
        &json!({"input":[{"content":[{"type":"input_image"}]}]})
    ));
    assert!(!has_image(&json!({"input":"troy"})));
}

async fn adapter(
    endpoint: Endpoint,
    upstream: String,
) -> (String, tokio::task::JoinHandle<Result<(), String>>) {
    let service = service(
        Api::with_all_endpoints(upstream.clone()),
        State {
            account: Some(Account {
                login: "troy-barnes".into(),
                access_token: "github-test-token".into(),
                refresh_token: None,
                expires_at: None,
                disabled_models: Vec::new(),
                models: vec![Model {
                    id: "troy".into(),
                    endpoint,
                }],
            }),
            token: Some(Token {
                secret: "copilot-test-token".into(),
                endpoint: upstream,
                refresh_at: Instant::now() + Duration::from_secs(600),
            }),
            login: None,
        },
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}{BRIDGE_PATH}", listener.local_addr().unwrap());
    let task = tokio::spawn(serve(listener, Arc::new(service)));
    (url, task)
}

#[tokio::test]
async fn forwards_json_and_streams_all_protocols_with_private_credentials() {
    for endpoint in [Endpoint::Chat, Endpoint::Responses, Endpoint::Messages] {
        let data = if endpoint == Endpoint::Chat {
            "data: {\"choices\":[{\"delta\":{\"content\":\"Troy\"}}]}\n\ndata: [DONE]\n\n"
        } else if endpoint == Endpoint::Responses {
            "event: response.output_text.delta\ndata: {\"delta\":\"Troy\"}\n\nevent: response.completed\ndata: {}\n\n"
        } else {
            "event: content_block_delta\ndata: {\"delta\":{\"text\":\"Troy\"}}\n\nevent: message_stop\ndata: {}\n\n"
        };
        let stream_response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n{:x}\r\n{}\r\n0\r\n\r\n",
            7, &data[..7], data.len()-7, &data[7..]);
        let (upstream, capture) = http_fixture(vec![
            json_response(200, json!({"model":"troy", "output":"Abed"})),
            stream_response,
        ])
        .await;
        let (url, task) = adapter(endpoint, upstream).await;
        let client = reqwest::Client::new();
        let response = client.post(format!("{url}{}", endpoint.path())).bearer_auth("local-test-key")
            .json(&json!({"model":"troy", "stream":false, "messages":[{"role":"user", "content":"Abed"}]}))
            .send().await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.json::<Value>().await.unwrap()["output"], "Abed");
        let response = client.post(format!("{url}{}", endpoint.path())).bearer_auth("local-test-key")
            .json(&json!({"model":"troy", "stream":true, "messages":[{"role":"tool", "content":[{"type":"image_url"}]}]}))
            .send().await.unwrap();
        assert_eq!(response.text().await.unwrap(), data);
        let requests = capture.await.unwrap();
        assert!(requests[0].starts_with(&format!("POST {} HTTP/1.1", endpoint.path())));
        assert!(requests[0].contains("authorization: Bearer copilot-test-token"));
        assert!(requests[0].contains("copilot-integration-id: vscode-chat"));
        assert!(requests[1].contains("x-initiator: agent"));
        assert!(requests[1].contains("copilot-vision-request: true"));
        assert!(!requests[0].contains("local-test-key"));
        assert!(!requests[0].contains("github-test-token"));
        task.abort();
    }
}

#[tokio::test]
async fn rejects_unknown_models_and_redacts_upstream_errors_preserving_retry_after() {
    let (upstream, capture) = http_fixture(vec!["HTTP/1.1 429 Limited\r\nContent-Type: application/json\r\nContent-Length: 18\r\nRetry-After: 30\r\nConnection: close\r\n\r\ncopilot-test-token".into()]).await;
    let (url, task) = adapter(Endpoint::Chat, upstream).await;
    let client = reqwest::Client::new();
    for (key, model, expected) in [
        ("wrong-key", "troy", 401),
        ("local-test-key", "unknown", 404),
        ("local-test-key", "troy", 429),
    ] {
        let response = client
            .post(format!("{url}/chat/completions"))
            .bearer_auth(key)
            .json(&json!({"model":model}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), expected);
        if expected == 429 {
            assert_eq!(response.headers()["retry-after"], "30");
        }
        assert!(!response
            .text()
            .await
            .unwrap()
            .contains("copilot-test-token"));
    }
    assert_eq!(capture.await.unwrap().len(), 1);
    task.abort();
}

#[tokio::test]
async fn request_body_can_arrive_separately_from_headers() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket, "local-test-key").await.unwrap()
    });
    client
        .write_all(&headers("/responses", "Content-Length: 16\r\n"))
        .await
        .unwrap();
    tokio::task::yield_now().await;
    client.write_all(b"{\"model\":\"troy\"}").await.unwrap();
    let (endpoint, body) = task.await.unwrap();
    assert_eq!(endpoint, Endpoint::Responses);
    assert_eq!(body["model"], "troy");
}

#[test]
fn removes_only_the_core_injected_hosted_tool() {
    let function = json!({"type":"function","name":"greendale","parameters":{"type":"object"}});
    let mut body =
        json!({"tools":[{"type":"image_generation","output_format":"png"}, function.clone()]});
    remove_codex_hosted_tool(&mut body).unwrap();
    assert_eq!(body["tools"], json!([function]));
    assert!(
        remove_codex_hosted_tool(&mut json!({"tool_choice":{"type":"image_generation"}})).is_err()
    );
    assert!(remove_codex_hosted_tool(
        &mut json!({"tools":[{"type":"image_generation","size":"1024x1024"}]})
    )
    .is_err());
}

#[tokio::test]
async fn unauthorized_inference_refreshes_once_and_replays_the_same_body() {
    let (upstream, capture) = http_fixture(vec![
        json_response(401, json!({"error":"expired"})),
        json_response(200, json!({"token":"renewed-copilot-token", "expires_at":crate::copilot::unix_now().unwrap() + 3600,
            "endpoints":{"api":"https://api.githubcopilot.com"}})),
        json_response(200, json!({"output":"Greendale"})),
    ]).await;
    let (url, task) = adapter(Endpoint::Chat, upstream).await;
    let response = reqwest::Client::new()
        .post(format!("{url}/chat/completions"))
        .bearer_auth("local-test-key")
        .json(&json!({"model":"troy","messages":[{"role":"user","content":"Abed"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.text().await.unwrap().contains("Greendale"));
    let requests = capture.await.unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[1].starts_with("GET /copilot_internal/v2/token "));
    assert!(requests[2].contains("authorization: Bearer renewed-copilot-token"));
    assert_eq!(
        requests[0].split_once("\r\n\r\n").unwrap().1,
        requests[2].split_once("\r\n\r\n").unwrap().1
    );
    task.abort();
}

#[tokio::test]
async fn broken_upstream_stream_is_not_reported_as_complete() {
    let (upstream, capture) = http_fixture(vec!["HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n9\r\ndata: hi\n\r\n".into()]).await;
    let (url, task) = adapter(Endpoint::Chat, upstream).await;
    let response = reqwest::Client::new()
        .post(format!("{url}/chat/completions"))
        .bearer_auth("local-test-key")
        .json(&json!({"model":"troy","stream":true}))
        .send()
        .await
        .unwrap();
    assert!(response.text().await.is_err());
    assert_eq!(capture.await.unwrap().len(), 1);
    task.abort();
}
