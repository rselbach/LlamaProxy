use super::*;
use crate::copilot::{config, random_key};
use std::{
    fs,
    process::{Command, Stdio},
};

// Uses a separately downloaded, checksum-verified core and fake loopback upstream.
// No live credentials, installed core, or user configuration are read or changed.
#[tokio::test]
#[ignore = "set LLAMAPROXY_TEST_CORE to the bundled CLIProxyAPI executable"]
async fn copilot_core_routes_and_translates_all_three_protocols() {
    let binary = std::env::var("LLAMAPROXY_TEST_CORE").expect("LLAMAPROXY_TEST_CORE is required");
    let completed = json!({"type":"response.completed", "response":{
        "id":"resp_abed", "object":"response", "status":"completed", "model":"abed", "created_at":1,
        "output":[{"id":"msg_abed", "type":"message", "role":"assistant", "status":"completed",
            "content":[{"type":"output_text", "text":"Greendale", "annotations":[]}]}],
        "usage":{"input_tokens":4, "output_tokens":2, "total_tokens":6}}});
    let sse = format!("event: response.completed\ndata: {completed}\n\n");
    let responses_body = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}", sse.len());
    let message_events = [
        json!({"type":"message_start","message":{"id":"msg_annie","type":"message","role":"assistant","model":"annie","content":[],"usage":{"input_tokens":4,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Greendale"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":2}}),
        json!({"type":"message_stop"}),
    ];
    let messages = message_events
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect::<String>();
    let messages_body = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{messages}", messages.len());
    let response_events = [
        json!({"type":"response.created","response":{"id":"resp_abed","model":"abed","status":"in_progress","output":[]}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"id":"msg_abed","type":"message","role":"assistant","content":[]}}),
        json!({"type":"response.output_text.delta","item_id":"msg_abed","output_index":0,"content_index":0,"delta":"Greendale"}),
        completed,
    ];
    let response_stream = response_events
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect::<String>();
    let response_stream_body = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_stream}", response_stream.len());
    let chat_chunk = json!({"id":"chat_troy","object":"chat.completion.chunk","model":"troy","created":1,"choices":[{"index":0,"delta":{"role":"assistant","content":"Greendale"},"finish_reason":null}]});
    let chat_done = json!({"id":"chat_troy","object":"chat.completion.chunk","model":"troy","created":1,"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":4,"completion_tokens":2,"total_tokens":6}});
    let chat_stream = format!("data: {chat_chunk}\n\ndata: {chat_done}\n\ndata: [DONE]\n\n");
    let chat_stream_body = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{chat_stream}", chat_stream.len());
    let chat_response = json_response(
        200,
        json!({"id":"chat_troy", "object":"chat.completion", "model":"troy", "created":1,
        "choices":[{"index":0,"message":{"role":"assistant","content":"Greendale"},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":4,"completion_tokens":2,"total_tokens":6}}),
    );
    let (upstream, capture) = http_fixture(vec![
        chat_response.clone(),
        responses_body,
        messages_body.clone(),
        chat_stream_body,
        response_stream_body,
        messages_body,
        chat_response,
    ])
    .await;
    let models = vec![
        Model {
            id: "troy".into(),
            endpoint: Endpoint::Chat,
        },
        Model {
            id: "abed".into(),
            endpoint: Endpoint::Responses,
        },
        Model {
            id: "annie".into(),
            endpoint: Endpoint::Messages,
        },
    ];
    let service = service(
        Api::new().unwrap(),
        State {
            account: Some(Account {
                login: "troy-barnes".into(),
                access_token: "github-test-token".into(),
                refresh_token: None,
                expires_at: None,
                disabled_models: Vec::new(),
                models: models.clone(),
            }),
            token: Some(Token {
                secret: "copilot-test-token".into(),
                endpoint: upstream.clone(),
                refresh_at: Instant::now() + Duration::from_secs(600),
            }),
            login: None,
        },
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}{BRIDGE_PATH}", listener.local_addr().unwrap());
    let adapter_task = tokio::spawn(serve(listener, Arc::new(service)));

    let port_reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = port_reservation.local_addr().unwrap().port();
    let root = std::env::temp_dir().join(format!(
        "llamaproxy-copilot-proof-{}",
        random_key().unwrap()
    ));
    fs::create_dir(&root).unwrap();
    let mut document = serde_norway::to_value(json!({"host":"127.0.0.1", "port":port,
        "auth-dir":root.join("oauth"), "api-keys":["client-test-key"], "request-retry":0,
        "remote-management":{"disable-control-panel":true}}))
    .unwrap();
    config::configure(&mut document, &url, "local-test-key", &models).unwrap();
    let path = root.join("config.yaml");
    fs::write(&path, serde_norway::to_string(&document).unwrap()).unwrap();
    let log = fs::File::create(root.join("core.log")).unwrap();
    let mut command = Command::new(binary);
    command
        .arg("-config")
        .arg(&path)
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    drop(port_reservation);
    let mut core = crate::spawn_core_child(command).unwrap();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(15);
    let catalog = loop {
        match client
            .get(format!("{base}/v1/models"))
            .bearer_auth("client-test-key")
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                break response.json::<Value>().await.unwrap()
            }
            _ => {
                assert!(
                    Instant::now() < deadline && core.try_wait().unwrap().is_none(),
                    "Core did not start: {}",
                    fs::read_to_string(root.join("core.log")).unwrap()
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };
    let ids: Vec<_> = catalog["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    for model in ["copilot/troy", "copilot/abed", "copilot/annie"] {
        assert!(ids.contains(&model), "{catalog}");
    }
    for model in ["troy", "abed", "annie"] {
        assert!(!ids.contains(&model), "{catalog}");
    }

    let deadline = Instant::now() + Duration::from_secs(15);
    while !fs::read_to_string(root.join("core.log"))
        .unwrap()
        .contains("file watcher started")
    {
        assert!(Instant::now() < deadline, "Core file watcher did not start");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    for enabled in [false, true] {
        let selected = if enabled { &models[..] } else { &models[1..] };
        config::write(&path, &url, "local-test-key", selected).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let catalog: Value = client
                .get(format!("{base}/v1/models"))
                .bearer_auth("client-test-key")
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let available = catalog["data"]
                .as_array()
                .unwrap()
                .iter()
                .any(|model| model["id"] == "copilot/troy");
            if available == enabled {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Model availability did not update: {catalog}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if !enabled {
            let response = client.post(format!("{base}/v1/chat/completions"))
                .bearer_auth("client-test-key")
                .json(&json!({"model":"copilot/troy", "messages":[{"role":"user","content":"Greendale"}]}))
                .send().await.unwrap();
            assert!(!response.status().is_success());
        }
    }

    for stream in [false, true] {
        for (path, body) in [
            (
                "/v1/messages",
                json!({"model":"copilot/troy", "max_tokens":32, "stream":stream,
            "messages":[{"role":"user","content":"Say Greendale"}]}),
            ),
            (
                "/v1/chat/completions",
                json!({"model":"copilot/abed", "stream":stream,
            "messages":[{"role":"user","content":"Say Greendale"}]}),
            ),
            (
                "/v1/responses",
                json!({"model":"copilot/annie", "stream":stream,"input":"Say Greendale"}),
            ),
        ] {
            let response = client
                .post(format!("{base}{path}"))
                .bearer_auth("client-test-key")
                .header("anthropic-version", "2023-06-01")
                .json(&body)
                .send()
                .await
                .unwrap();
            let status = response.status();
            let text = response.text().await.unwrap();
            assert!(
                status.is_success(),
                "{path}: {status} {text}; log: {}",
                fs::read_to_string(root.join("core.log")).unwrap()
            );
            assert!(text.contains("Greendale"), "{path}: {text}");
            assert!(!text.contains("test-token"));
            if stream {
                let terminal = match path {
                    "/v1/messages" => "message_stop",
                    "/v1/responses" => "response.completed",
                    _ => "[DONE]",
                };
                assert!(
                    text.contains(terminal),
                    "{path}: missing {terminal}: {text}"
                );
            }
        }
    }
    // An app restart can adopt a surviving core. Rebind without restarting it.
    adapter_task.abort();
    let mut rebound = crate::copilot::tests::service(
        Api::new().unwrap(),
        State {
            account: Some(Account {
                login: "troy-barnes".into(),
                access_token: "github-test-token".into(),
                refresh_token: None,
                expires_at: None,
                disabled_models: Vec::new(),
                models: models.clone(),
            }),
            token: Some(Token {
                secret: "copilot-test-token".into(),
                endpoint: upstream,
                refresh_at: Instant::now() + Duration::from_secs(600),
            }),
            login: None,
        },
    );
    rebound.key = "rebound-adapter-key".into();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let rebound_url = format!("http://{}{BRIDGE_PATH}", listener.local_addr().unwrap());
    let rebound_task = tokio::spawn(serve(listener, Arc::new(rebound)));
    config::write(&path, &rebound_url, "rebound-adapter-key", &models).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let response = client.post(format!("{base}/v1/chat/completions")).bearer_auth("client-test-key")
            .json(&json!({"model":"copilot/troy","messages":[{"role":"user","content":"Say Greendale"}]}))
            .send().await.unwrap();
        if response.status().is_success() {
            assert!(response.text().await.unwrap().contains("Greendale"));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "The running core did not reload the Copilot route"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let requests = tokio::time::timeout(Duration::from_secs(5), capture)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(requests.len(), 7);
    for (request, endpoint) in requests.iter().zip(
        ["/chat/completions", "/responses", "/v1/messages"]
            .into_iter()
            .cycle(),
    ) {
        assert!(
            request.starts_with(&format!("POST {endpoint} HTTP/1.1")),
            "{request}"
        );
        assert!(
            !request.contains("copilot/"),
            "Upstream saw a routing alias: {request}"
        );
        assert!(
            !request.contains("image_generation"),
            "Core injected an unsupported hosted tool: {request}"
        );
    }
    drop(core);
    rebound_task.abort();
    fs::remove_dir_all(root).unwrap();
}
