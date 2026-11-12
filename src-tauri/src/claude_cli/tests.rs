use super::*;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = base.join(format!(
            "llamaproxy-claude-cli-test-{}",
            &random_key().unwrap()[..16]
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.0.join(name)).unwrap_or_default()
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!("Could not remove {}: {error}", self.0.display());
        }
    }
}

// What the fake Claude Code does for one query: print output, then optionally
// call one of the adapter's MCP tools and wait for its result, as Claude Code
// does after a tool_use reply.
#[derive(Default)]
struct Step {
    output: Vec<Value>,
    call: Option<Value>,
    // ignore its input from then on, like a process busy generating
    hang: bool,
}

fn reply(output: Vec<Value>) -> Step {
    Step {
        output,
        ..Step::default()
    }
}

// A `claude` stand-in. Each process records its arguments in args.<pid> and
// every input line in stdin, and answers each query (and each tool result it
// waits for) with the next step's output.
fn fake_claude(dir: &TestDir, auth: &Value, steps: &[Step]) -> PathBuf {
    fs::write(dir.0.join("auth.json"), auth.to_string()).unwrap();
    for (index, step) in steps.iter().enumerate() {
        let number = index + 1;
        let lines: String = step.output.iter().map(|line| format!("{line}\n")).collect();
        fs::write(dir.0.join(format!("output.{number}")), lines).unwrap();
        if let Some(call) = &step.call {
            fs::write(dir.0.join(format!("call.{number}")), call.to_string()).unwrap();
        }
        if step.hang {
            fs::write(dir.0.join(format!("hang.{number}")), "").unwrap();
        }
    }
    let script = format!(
        r#"#!/bin/sh
# Fake Claude Code for tests: answers each query with the next canned step.
dir='{dir}'
if [ "$1" = auth ]; then
  cat "$dir/auth.json"
  exit 0
fi
printf '%s\n' "$@" > "$dir/args.$$"
config=$(awk 'previous == "--mcp-config" {{ print }} {{ previous = $0 }}' "$dir/args.$$")
url=$(sed -n 's/.*"url":"\([^"]*\)".*/\1/p' "$config")
step=0
next_step() {{
  step=$((step + 1))
  if [ -f "$dir/output.$step" ]; then cat "$dir/output.$step"; fi
  if [ -f "$dir/hang.$step" ]; then exec sleep 600; fi
  if [ -f "$dir/call.$step" ]; then
    curl -s -X POST -H 'Content-Type: application/json' \
      --data-binary @"$dir/call.$step" "$url" > "$dir/result.$step"
    next_step
  fi
}}
while IFS= read -r line; do
  case "$line" in
    *'"control_request"'*)
      echo '{{"type":"control_response","response":{{"subtype":"success"}}}}'
      continue ;;
  esac
  printf '%s\n' "$line" >> "$dir/stdin"
  case "$line" in
    *'"shouldQuery":false,"type":"user"}}'*)
      echo '{{"type":"result","subtype":"success","is_error":false,"num_turns":0,"result":""}}'
      continue ;;
    *'"shouldQuery":false'*) continue ;;
  esac
  next_step
done
"#,
        dir = dir.0.display()
    );
    let path = dir.0.join("claude");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn signed_in() -> Value {
    json!({"loggedIn": true, "authMethod": "claude.ai", "email": "annie@greendale.edu"})
}

fn process_ids(dir: &TestDir) -> Vec<i32> {
    fs::read_dir(&dir.0)
        .unwrap()
        .filter_map(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            name.strip_prefix("args.")?.parse().ok()
        })
        .collect()
}

fn args(dir: &TestDir) -> Vec<String> {
    let pid = process_ids(dir)[0];
    dir.read(&format!("args.{pid}"))
        .lines()
        .map(str::to_string)
        .collect()
}

fn event(event: Value) -> Value {
    json!({"type": "stream_event", "event": event})
}

fn message_start() -> Value {
    event(
        json!({"type": "message_start", "message": {"id": "msg_greendale", "type": "message",
        "role": "assistant", "model": "claude-haiku-5-5", "content": [],
        "usage": {"input_tokens": 7, "output_tokens": 0}}}),
    )
}

fn tool_call_output(using_overage: bool) -> Vec<Value> {
    vec![
        json!({"type": "system", "subtype": "init", "tools": ["mcp__llamaproxy__lookup_schedule"]}),
        message_start(),
        event(
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        ),
        event(
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Checking."}}),
        ),
        event(json!({"type": "content_block_stop", "index": 0})),
        event(
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use",
            "id": "toolu_spanish", "name": "mcp__llamaproxy__lookup_schedule", "input": {}}}),
        ),
        event(
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"course\": "}}),
        ),
        event(
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "\"Spanish 101\"}"}}),
        ),
        event(json!({"type": "content_block_stop", "index": 1})),
        event(
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use", "stop_sequence": null}, "usage": {"output_tokens": 12}}),
        ),
        event(json!({"type": "message_stop"})),
        json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed", "isUsingOverage": using_overage}}),
    ]
}

fn text_output(text: &str) -> Vec<Value> {
    vec![
        message_start(),
        event(
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        ),
        event(
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
        ),
        event(json!({"type": "content_block_stop", "index": 0})),
        event(
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": null}, "usage": {"output_tokens": 3}}),
        ),
        event(json!({"type": "message_stop"})),
        json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed", "isUsingOverage": false}}),
        json!({"type": "result", "subtype": "success", "is_error": false, "num_turns": 1, "result": text}),
    ]
}

fn tool_call() -> Value {
    json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "lookup_schedule",
        "arguments": {"course": "Spanish 101"}, "_meta": {"claudecode/toolUseId": "toolu_spanish"}}})
}

fn question(stream: bool) -> Value {
    json!({"model": "haiku", "max_tokens": 512, "stream": stream, "system": "You are Annie.",
        "tools": [{"name": "lookup_schedule", "input_schema": {"type": "object"}}],
        "messages": [{"role": "user", "content": "When is Spanish 101?"}]})
}

fn with_tool_result(mut request: Value) -> Value {
    let messages = request["messages"].as_array_mut().unwrap();
    messages.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Checking."},
        {"type": "tool_use", "id": "toolu_spanish", "name": "lookup_schedule", "input": {"course": "Spanish 101"}}]}));
    messages.push(
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_spanish",
        "content": "Tuesdays at 9 with Chang", "cache_control": {"type": "ephemeral"}}]}),
    );
    request
}

// Starts the adapter with whatever connection the directory already holds.
fn start_saved(dir: &TestDir, command: PathBuf) -> Arc<ClaudeCli> {
    let (service, listener) = ClaudeCli::build(
        dir.0.join("account.json"),
        dir.0.join("config.yaml"),
        dir.0.join("work"),
        Some(command),
    )
    .unwrap();
    tokio::spawn(transport::serve(listener, service.clone()));
    service
}

fn start(dir: &TestDir, command: PathBuf) -> Arc<ClaudeCli> {
    let service = start_saved(dir, command);
    service
        .commit(Some(Account {
            label: "annie@greendale.edu".into(),
            disabled_models: Vec::new(),
        }))
        .unwrap();
    service
}

async fn post(service: &ClaudeCli, key: &str, body: &Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/v1/messages?beta=true", service.base_url))
        .bearer_auth(key)
        .json(body)
        .send()
        .await
        .unwrap()
}

async fn ask(service: &ClaudeCli, body: &Value) -> (u16, String, Value) {
    let response = post(service, &service.key, body).await;
    let status = response.status().as_u16();
    let session = response
        .headers()
        .get(engine::SESSION_HEADER)
        .map(|value| value.to_str().unwrap().to_string())
        .unwrap_or_default();
    (status, session, response.json().await.unwrap())
}

#[tokio::test]
async fn streams_client_tool_calls_and_appends_the_system_prompt() {
    let r = TestDir::new();
    let command = fake_claude(&r, &signed_in(), &[reply(tool_call_output(false))]);
    let service = start(&r, command);

    let mut request = question(true);
    request["output_config"] = json!({"effort": "high"});
    let response = post(&service, &service.key, &request).await;
    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    assert!(body.contains(r#""name":"lookup_schedule""#), "{body}");
    assert!(!body.contains("mcp__llamaproxy__"), "{body}");
    assert!(
        body.trim_end()
            .ends_with(r#"data: {"type":"message_stop"}"#),
        "{body}"
    );

    let args = args(&r);
    for want in [
        "-p",
        "--strict-mcp-config",
        "--no-session-persistence",
        "--append-system-prompt-file",
    ] {
        assert!(
            args.iter().any(|arg| arg == want),
            "{args:?} is missing {want}"
        );
    }
    assert!(
        !args.iter().any(|arg| arg.starts_with("--system-prompt")),
        "{args:?}"
    );
    for (flag, want) in [("--model", "haiku"), ("--effort", "high")] {
        let at = args.iter().position(|arg| arg == flag).unwrap();
        assert_eq!(args[at + 1], want, "{flag}");
    }
    let stdin = r.read("stdin");
    assert!(
        stdin.contains("When is Spanish 101?") && !stdin.contains("shouldQuery"),
        "{stdin}"
    );
}

#[tokio::test]
async fn assembles_a_whole_reply_when_the_request_does_not_stream() {
    let r = TestDir::new();
    let command = fake_claude(&r, &signed_in(), &[reply(tool_call_output(false))]);
    let service = start(&r, command);

    let (status, session, reply) = ask(&service, &question(false)).await;
    assert_eq!((status, session.as_str()), (200, "started"), "{reply}");
    assert_eq!(reply["stop_reason"], "tool_use");
    assert_eq!(reply["usage"]["output_tokens"], 12);
    assert_eq!(
        reply["content"][0],
        json!({"type": "text", "text": "Checking."})
    );
    assert_eq!(
        reply["content"][1],
        json!({"type": "tool_use", "id": "toolu_spanish",
        "name": "lookup_schedule", "input": {"course": "Spanish 101"}})
    );
}

#[tokio::test]
async fn a_tool_result_continues_in_the_same_process() {
    let r = TestDir::new();
    let steps = [
        Step {
            output: tool_call_output(false),
            call: Some(tool_call()),
            hang: false,
        },
        reply(text_output("Tuesdays at 9.")),
    ];
    let service = start(&r, fake_claude(&r, &signed_in(), &steps));

    let (status, _, first) = ask(&service, &question(false)).await;
    assert_eq!(status, 200, "{first}");
    let (status, session, second) = ask(&service, &with_tool_result(question(false))).await;
    assert_eq!((status, session.as_str()), (200, "resumed"), "{second}");
    assert_eq!(second["content"][0]["text"], "Tuesdays at 9.");
    assert_eq!(process_ids(&r).len(), 1);
    let result: Value = serde_json::from_str(&r.read("result.1")).unwrap();
    assert_eq!(result["id"], 7);
    assert_eq!(
        result["result"]["content"][0]["text"],
        "Tuesdays at 9 with Chang"
    );
    assert_eq!(service.engine.pool.parked(), 1);
}

#[tokio::test]
async fn an_edited_conversation_starts_a_new_process_with_its_history() {
    let r = TestDir::new();
    let service = start(
        &r,
        fake_claude(&r, &signed_in(), &[reply(text_output("Mondays."))]),
    );

    let (status, _, first) = ask(&service, &question(false)).await;
    assert_eq!(status, 200, "{first}");
    let mut edited = question(false);
    let messages = edited["messages"].as_array_mut().unwrap();
    messages.push(json!({"role": "assistant", "content": "Fridays."}));
    messages.push(json!({"role": "user", "content": "And Biology?"}));
    let (status, session, reply) = ask(&service, &edited).await;
    assert_eq!((status, session.as_str()), (200, "started"), "{reply}");
    assert_eq!(process_ids(&r).len(), 2);
    let stdin = r.read("stdin");
    assert!(
        stdin.contains("Fridays.") && stdin.contains(r#""shouldQuery":false"#),
        "{stdin}"
    );
}

#[tokio::test]
async fn usage_credits_end_the_reply_with_a_billing_error() {
    let r = TestDir::new();
    let command = fake_claude(&r, &signed_in(), &[reply(tool_call_output(true))]);
    let service = start(&r, command);

    let (status, _, reply) = ask(&service, &question(false)).await;
    assert_eq!(status, 402, "{reply}");
    assert_eq!(reply["error"]["type"], "billing_error");
    assert_eq!(service.engine.pool.parked(), 0);
}

#[tokio::test]
async fn claude_code_errors_keep_their_status() {
    let cases = [
        ("authentication_failed", 401, "authentication_error"),
        ("rate_limit", 429, "rate_limit_error"),
        ("overloaded", 529, "overloaded_error"),
    ];
    for (error, want_status, want_kind) in cases {
        let r = TestDir::new();
        let output = vec![json!({"type": "assistant", "error": error,
            "message": {"content": [{"type": "text", "text": "Pierce broke it"}]}})];
        let service = start(&r, fake_claude(&r, &signed_in(), &[reply(output)]));

        let (status, _, reply) = ask(&service, &question(false)).await;
        assert_eq!(status, want_status, "{error}: {reply}");
        assert_eq!(
            reply["error"],
            json!({"type": want_kind, "message": "Pierce broke it"})
        );
    }
}

#[tokio::test]
async fn rejects_callers_without_the_key_a_connection_or_a_listed_model() {
    let r = TestDir::new();
    let service = start(&r, fake_claude(&r, &signed_in(), &[]));

    assert_eq!(
        post(&service, "wrong-key", &question(false)).await.status(),
        401
    );
    let mut unknown = question(false);
    unknown["model"] = json!("gpt-5.4");
    assert_eq!(post(&service, &service.key, &unknown).await.status(), 404);
    let mut forced = question(false);
    forced["tool_choice"] = json!({"type": "any"});
    assert_eq!(post(&service, &service.key, &forced).await.status(), 400);
    service.disconnect().unwrap();
    assert_eq!(
        post(&service, &service.key, &question(false))
            .await
            .status(),
        401
    );
    assert!(process_ids(&r).is_empty());
}

#[tokio::test]
async fn a_client_that_disconnects_stops_claude_code() {
    let r = TestDir::new();
    let service = start(
        &r,
        fake_claude(
            &r,
            &signed_in(),
            &[Step {
                output: vec![message_start()],
                call: None,
                hang: true,
            }],
        ),
    );

    let mut response = post(&service, &service.key, &question(true)).await;
    assert!(response.chunk().await.unwrap().is_some());
    let pid = process_ids(&r)[0];
    drop(response);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    // SAFETY: signal 0 only checks whether the fake process still exists.
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "Claude Code kept running"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn connecting_requires_a_subscription_sign_in_and_owns_only_its_routes() {
    let cases = [
        (json!({"loggedIn": false}), "not signed in"),
        (
            json!({"loggedIn": true, "authMethod": "api_key", "email": "jeff@greendale.edu"}),
            "not a Claude subscription",
        ),
    ];
    for (auth, want) in cases {
        let r = TestDir::new();
        let (service, _listener) = ClaudeCli::build(
            r.0.join("account.json"),
            r.0.join("config.yaml"),
            r.0.join("work"),
            Some(fake_claude(&r, &auth, &[])),
        )
        .unwrap();
        let error = service.connect().await.unwrap_err();
        assert!(error.contains(want), "{error}");
        assert!(service.status().account.is_none());
    }

    let r = TestDir::new();
    let config = r.0.join("config.yaml");
    fs::write(&config, "port: 11432\nclaude-api-key:\n  - api-key: native-secret\n  - base-url: http://127.0.0.1:4321/llamaproxy-copilot\n    api-key: copilot-key\n").unwrap();
    let build = || {
        ClaudeCli::build(
            r.0.join("account.json"),
            config.clone(),
            r.0.join("work"),
            Some(fake_claude(&r, &signed_in(), &[])),
        )
        .unwrap()
    };
    let (service, _listener) = build();
    service.connect().await.unwrap();
    let status = service.status();
    assert_eq!(status.account.as_deref(), Some("annie@greendale.edu"));
    assert!(status.models.contains(&"claude-cli/opus".to_string()));
    let document: Value = serde_norway::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    let entries = document["claude-api-key"].as_array().unwrap();
    assert_eq!(entries.len(), 3, "{document}");
    assert_eq!(entries[2]["base-url"], service.base_url);
    assert_eq!(entries[2]["request-retry"], 0);
    assert_eq!(entries[2]["cloak"]["mode"], "never");
    assert!(entries[2]["models"]
        .as_array()
        .unwrap()
        .contains(&json!({"name": "opus", "alias": "claude-cli/opus",
            "thinking": {"levels": EFFORTS}})));

    let (restarted, _listener) = build();
    assert!(restarted.status().account.is_some());
    restarted.configure(&restarted.enabled_models()).unwrap();
    let document: Value = serde_norway::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    let entries = document["claude-api-key"].as_array().unwrap();
    assert_eq!(entries.len(), 3, "{document}");
    assert_eq!(entries[2]["base-url"], restarted.base_url);

    restarted.disconnect().unwrap();
    let document: Value = serde_norway::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(
        document["claude-api-key"],
        json!([{"api-key": "native-secret"},
        {"base-url": "http://127.0.0.1:4321/llamaproxy-copilot", "api-key": "copilot-key"}])
    );
    assert!(build().0.status().account.is_none());
}

fn create_model_alias(service: &ClaudeCli, model: &str, alias: &str, effort: &str) {
    let content = fs::read_to_string(&service.config_path).unwrap();
    let available = crate::parse_agent_model_options(&json!({"data": [{"id": model}]})).unwrap();
    let sources = crate::resolved_oauth_alias_sources(
        &content,
        &[],
        &available,
        crate::AliasSourceCapability::Reasoning,
    )
    .unwrap();
    let source = sources
        .iter()
        .find(|source| source.source.model == model)
        .unwrap();
    let updated = crate::add_model_alias_to_yaml(&content, source, alias, effort, false).unwrap();
    fs::write(&service.config_path, updated).unwrap();
}

#[test]
fn custom_model_aliases_survive_a_restart_and_unrelated_model_changes() {
    let r = TestDir::new();
    fs::write(r.0.join("config.yaml"), "port: 11432\n").unwrap();
    let build = || {
        ClaudeCli::build(
            r.0.join("account.json"),
            r.0.join("config.yaml"),
            r.0.join("work"),
            None,
        )
        .unwrap()
    };
    let (service, _listener) = build();
    service
        .commit(Some(Account {
            label: "annie@greendale.edu".into(),
            disabled_models: Vec::new(),
        }))
        .unwrap();
    create_model_alias(&service, "claude-cli/haiku", "greendale", "");
    create_model_alias(&service, "claude-cli/haiku", "study-group", "");

    let (restarted, _listener) = build();
    restarted.configure(&restarted.enabled_models()).unwrap();
    restarted
        .set_model_enabled("claude-cli/opus", false)
        .unwrap();
    restarted.configure(&restarted.enabled_models()).unwrap();
    let document: Value = serde_norway::from_str(&r.read("config.yaml")).unwrap();
    let entries = document["claude-api-key"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["base-url"], restarted.base_url);
    assert_eq!(entries[0]["api-key"], restarted.key);
    let models = entries[0]["models"].as_array().unwrap();
    for alias in ["claude-cli/haiku", "greendale", "study-group"] {
        assert_eq!(
            models.iter().filter(|model| model["alias"] == alias).count(),
            1
        );
        assert!(models.contains(&json!({"name": "haiku", "alias": alias,
            "thinking": {"levels": EFFORTS}})));
    }

    let content =
        crate::remove_thinking_alias_from_yaml(&r.read("config.yaml"), "greendale").unwrap();
    fs::write(&restarted.config_path, content).unwrap();
    restarted.configure(&restarted.enabled_models()).unwrap();
    let aliases = crate::thinking_aliases_from_yaml(&r.read("config.yaml")).unwrap();
    assert!(!aliases.iter().any(|entry| entry.alias == "greendale"));
    assert!(aliases.iter().any(|entry| entry.alias == "study-group"));

    restarted
        .set_model_enabled("claude-cli/haiku", false)
        .unwrap();
    let aliases = crate::thinking_aliases_from_yaml(&r.read("config.yaml")).unwrap();
    assert!(!aliases.iter().any(|entry| entry.alias == "study-group"));
}

async fn core_json(base: &str, path: &str, body: &Value) -> Value {
    let response = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .bearer_auth("client-test-key")
        .header("anthropic-version", "2023-06-01")
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    assert!(status.is_success(), "{path}: {status} {text}");
    serde_json::from_str(&text).unwrap()
}

// Starts the CLIProxyAPI executable with only the adapter's managed route.
async fn start_core(r: &TestDir, service: &ClaudeCli) -> (crate::CoreChild, String) {
    let binary = std::env::var("LLAMAPROXY_TEST_CORE").expect("LLAMAPROXY_TEST_CORE is required");
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut document = serde_norway::to_value(json!({"host": "127.0.0.1", "port": port,
        "auth-dir": r.0.join("oauth"), "api-keys": ["client-test-key"],
        "remote-management": {"disable-control-panel": true}}))
    .unwrap();
    config::configure(&mut document, &service.base_url, &service.key, &MODELS).unwrap();
    fs::write(
        &service.config_path,
        serde_norway::to_string(&document).unwrap(),
    )
    .unwrap();
    let log = fs::File::create(r.0.join("core.log")).unwrap();
    let mut command = std::process::Command::new(binary);
    command
        .arg("-config")
        .arg(&service.config_path)
        .current_dir(&r.0)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    let core = crate::spawn_core_child(command).unwrap();
    let base = format!("http://127.0.0.1:{port}");
    wait_for_core_model(r, &base, "claude-cli/haiku").await;
    (core, base)
}

async fn wait_for_core_model(r: &TestDir, base: &str, model: &str) {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let catalog = reqwest::Client::new()
            .get(format!("{base}/v1/models"))
            .bearer_auth("client-test-key")
            .send()
            .await;
        if let Ok(catalog) = catalog {
            let catalog: Value = catalog.json().await.unwrap();
            if r.read("core.log").contains("file watcher started")
                && catalog["data"].as_array().is_some_and(|models| {
                    models.iter().any(|entry| entry["id"] == model)
                })
            {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "Core did not expose {model}: {}",
            r.read("core.log")
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
#[ignore = "set LLAMAPROXY_TEST_CORE to the bundled CLIProxyAPI executable"]
async fn custom_model_alias_routes_to_claude_cli_through_the_core() {
    let r = TestDir::new();
    let service = start(
        &r,
        fake_claude(&r, &signed_in(), &[reply(text_output("Greendale."))]),
    );
    let (_core, base) = start_core(&r, &service).await;
    create_model_alias(&service, "claude-cli/haiku", "study-group", "high");
    service.configure(&service.enabled_models()).unwrap();
    wait_for_core_model(&r, &base, "study-group").await;

    for (path, request) in [
        ("/v1/chat/completions", json!({"model": "study-group",
            "messages": [{"role": "user", "content": "What college?"}]})),
        ("/v1/responses", json!({"model": "study-group",
            "input": [{"type": "message", "role": "user",
                "content": [{"type": "input_text", "text": "What college?"}]}]})),
        ("/v1/messages", json!({"model": "study-group", "max_tokens": 64,
            "messages": [{"role": "user", "content": "What college?"}]})),
    ] {
        let response = core_json(&base, path, &request).await;
        assert!(
            response.to_string().contains("Greendale."),
            "{path}: {response}"
        );
    }
    for pid in process_ids(&r) {
        let arguments = r.read(&format!("args.{pid}"));
        let arguments: Vec<_> = arguments.lines().collect();
        for (flag, want) in [("--model", "haiku"), ("--effort", "high")] {
            let at = arguments.iter().position(|arg| *arg == flag).unwrap();
            assert_eq!(arguments[at + 1], want, "{flag}");
        }
    }
    service.disconnect().unwrap();
}

// The parked process is found again only if the conversation still reads the
// same after the core has translated it, so this guards core updates.
#[tokio::test]
#[ignore = "set LLAMAPROXY_TEST_CORE to the bundled CLIProxyAPI executable"]
async fn the_core_keeps_a_tool_conversation_in_one_process() {
    let r = TestDir::new();
    let steps = [
        Step {
            output: tool_call_output(false),
            call: Some(tool_call()),
            hang: false,
        },
        reply(text_output("Tuesdays at 9.")),
    ];
    let service = start(&r, fake_claude(&r, &signed_in(), &steps));
    let (_core, base) = start_core(&r, &service).await;
    let parameters = json!({"type": "object", "properties": {"course": {"type": "string"}}});

    let mut chat = json!({"model": "claude-cli/haiku",
        "tools": [{"type": "function", "function": {"name": "lookup_schedule", "parameters": parameters}}],
        "messages": [{"role": "system", "content": "You are Annie."}, {"role": "user", "content": "When is Spanish 101?"}]});
    let first = core_json(&base, "/v1/chat/completions", &chat).await;
    let assistant = first["choices"][0]["message"].clone();
    let call_id = assistant["tool_calls"][0]["id"].clone();
    assert_eq!(
        assistant["tool_calls"][0]["function"]["name"], "lookup_schedule",
        "{first}"
    );
    let messages = chat["messages"].as_array_mut().unwrap();
    messages.push(assistant);
    messages
        .push(json!({"role": "tool", "tool_call_id": call_id, "content": "chat says Tuesdays"}));
    let second = core_json(&base, "/v1/chat/completions", &chat).await;
    assert_eq!(
        second["choices"][0]["message"]["content"], "Tuesdays at 9.",
        "{second}"
    );
    assert!(r.read("result.1").contains("chat says Tuesdays"));
    assert_eq!(process_ids(&r).len(), 1);

    let mut responses = json!({"model": "claude-cli/haiku", "instructions": "You are Annie.", "store": false,
        "tools": [{"type": "function", "name": "lookup_schedule", "parameters": parameters}],
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "When is Biology 101?"}]}]});
    let first = core_json(&base, "/v1/responses", &responses).await;
    let output = first["output"].as_array().unwrap().clone();
    let call = output
        .iter()
        .find(|item| item["type"] == "function_call")
        .unwrap()
        .clone();
    let input = responses["input"].as_array_mut().unwrap();
    input.extend(output);
    input.push(json!({"type": "function_call_output", "call_id": call["call_id"], "output": "responses says Tuesdays"}));
    let second = core_json(&base, "/v1/responses", &responses).await;
    assert!(second.to_string().contains("Tuesdays at 9."), "{second}");
    assert!(r.read("result.1").contains("responses says Tuesdays"));
    assert_eq!(process_ids(&r).len(), 2);

    let mut native = json!({"model": "claude-cli/haiku", "max_tokens": 512, "system": "You are Annie.",
        "tools": [{"name": "lookup_schedule", "input_schema": parameters}],
        "messages": [{"role": "user", "content": "When is Anthropology 101?"}]});
    let first = core_json(&base, "/v1/messages", &native).await;
    let content = first["content"].as_array().unwrap().clone();
    let call = content
        .iter()
        .find(|block| block["type"] == "tool_use")
        .unwrap()
        .clone();
    let messages = native["messages"].as_array_mut().unwrap();
    messages.push(json!({"role": "assistant", "content": content}));
    messages.push(json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": call["id"], "content": "messages says Tuesdays"}]}));
    let second = core_json(&base, "/v1/messages", &native).await;
    assert_eq!(second["content"][0]["text"], "Tuesdays at 9.", "{second}");
    assert!(r.read("result.1").contains("messages says Tuesdays"));
    assert_eq!(process_ids(&r).len(), 3);
}

// Spends a little of the signed-in Claude plan's usage, so it never runs by default.
#[tokio::test]
#[ignore = "set LLAMAPROXY_TEST_LIVE_CLAUDE=1 to spend Claude plan usage on a real request"]
async fn live_claude_code_answers_and_continues_a_tool_conversation() {
    assert_eq!(
        std::env::var("LLAMAPROXY_TEST_LIVE_CLAUDE").as_deref(),
        Ok("1")
    );
    let r = TestDir::new();
    let (service, listener) = ClaudeCli::build(
        r.0.join("account.json"),
        r.0.join("config.yaml"),
        r.0.join("work"),
        None,
    )
    .unwrap();
    service.connect().await.unwrap();
    tokio::spawn(transport::serve(listener, service.clone()));

    let mut request = json!({"model": "haiku", "max_tokens": 512, "stream": false,
        "system": "You are the Greendale registrar. Always call lookup_schedule before answering a schedule question.",
        "tools": [{"name": "lookup_schedule", "description": "Look up when a course meets.",
            "input_schema": {"type": "object", "properties": {"course": {"type": "string"}}, "required": ["course"]}}],
        "messages": [{"role": "user", "content": "When is Spanish 101?"}]});
    let (status, session, first) = ask(&service, &request).await;
    eprintln!("first turn: {first}");
    assert_eq!((status, session.as_str()), (200, "started"), "{first}");
    assert_eq!(first["stop_reason"], "tool_use", "{first}");
    let content = first["content"].as_array().unwrap().clone();
    let call = content
        .iter()
        .find(|block| block["type"] == "tool_use")
        .unwrap()
        .clone();
    assert_eq!(call["name"], "lookup_schedule");

    let messages = request["messages"].as_array_mut().unwrap();
    messages.push(json!({"role": "assistant", "content": content}));
    messages.push(
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": call["id"],
        "content": "Tuesdays at 9 with Chang"}]}),
    );
    let (status, session, second) = ask(&service, &request).await;
    eprintln!("second turn: {second}");
    assert_eq!((status, session.as_str()), (200, "resumed"), "{second}");
    assert!(second.to_string().contains("Tuesday"), "{second}");

    let streamed = post(
        &service,
        &service.key,
        &json!({"model": "haiku", "max_tokens": 64, "stream": true,
        "messages": [{"role": "user", "content": "Reply with the single word Greendale."}]}),
    )
    .await;
    assert_eq!(streamed.status(), 200);
    let body = streamed.text().await.unwrap();
    assert!(
        body.contains("Greendale")
            && body
                .trim_end()
                .ends_with(r#"data: {"type":"message_stop"}"#),
        "{body}"
    );
    assert_eq!(service.engine.pool.parked(), 2);
    service.disconnect().unwrap();
}

// The whole path with nothing faked: client format, core translation, adapter, Claude Code.
#[tokio::test]
#[ignore = "set LLAMAPROXY_TEST_LIVE_CLAUDE=1 and LLAMAPROXY_TEST_CORE to spend Claude plan usage through the core"]
async fn live_claude_code_serves_openai_clients_through_the_core() {
    assert_eq!(
        std::env::var("LLAMAPROXY_TEST_LIVE_CLAUDE").as_deref(),
        Ok("1")
    );
    let r = TestDir::new();
    let (service, listener) = ClaudeCli::build(
        r.0.join("account.json"),
        r.0.join("config.yaml"),
        r.0.join("work"),
        None,
    )
    .unwrap();
    service.connect().await.unwrap();
    tokio::spawn(transport::serve(listener, service.clone()));
    let (_core, base) = start_core(&r, &service).await;

    let mut chat = json!({"model": "claude-cli/haiku",
        "tools": [{"type": "function", "function": {"name": "lookup_schedule", "description": "Look up when a course meets.",
            "parameters": {"type": "object", "properties": {"course": {"type": "string"}}, "required": ["course"]}}}],
        "messages": [{"role": "system", "content": "You are the Greendale registrar. Always call lookup_schedule before answering a schedule question."},
            {"role": "user", "content": "When is Spanish 101?"}]});
    let first = core_json(&base, "/v1/chat/completions", &chat).await;
    eprintln!("chat first turn: {first}");
    let assistant = first["choices"][0]["message"].clone();
    assert_eq!(
        assistant["tool_calls"][0]["function"]["name"], "lookup_schedule",
        "{first}"
    );
    assert_eq!(service.engine.pool.parked(), 1);
    let call_id = assistant["tool_calls"][0]["id"].clone();
    let messages = chat["messages"].as_array_mut().unwrap();
    messages.push(assistant);
    messages.push(
        json!({"role": "tool", "tool_call_id": call_id, "content": "Tuesdays at 9 with Chang"}),
    );
    let second = core_json(&base, "/v1/chat/completions", &chat).await;
    eprintln!("chat second turn: {second}");
    assert!(
        second["choices"][0]["message"]["content"]
            .as_str()
            .unwrap()
            .contains("Tuesday"),
        "{second}"
    );
    assert_eq!(
        service.engine.pool.parked(),
        1,
        "the tool result started a second process"
    );

    let streamed = reqwest::Client::new()
        .post(format!("{base}/v1/responses"))
        .bearer_auth("client-test-key")
        .json(&json!({"model": "claude-cli/haiku", "stream": true, "input": [{"type": "message", "role": "user",
            "content": [{"type": "input_text", "text": "Reply with the single word Greendale."}]}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(streamed.status(), 200);
    let body = streamed.text().await.unwrap();
    assert!(
        body.contains("Greendale") && body.contains("response.completed"),
        "{body}"
    );
    service.disconnect().unwrap();
}

#[tokio::test]
async fn a_model_turned_off_leaves_the_routes_and_stays_off_after_a_restart() {
    let r = TestDir::new();
    let config = r.0.join("config.yaml");
    fs::write(&config, "port: 11432\n").unwrap();
    let command = fake_claude(&r, &signed_in(), &[reply(text_output("Mondays."))]);
    let aliases = || {
        let document: Value =
            serde_norway::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        let entries = document["claude-api-key"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        entries
            .iter()
            .flat_map(|entry| entry["models"].as_array().unwrap().clone())
            .map(|model| model["alias"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    let service = start(&r, command.clone());
    assert!(aliases().contains(&"claude-cli/haiku".to_string()));

    service
        .set_model_enabled("claude-cli/haiku", false)
        .unwrap();
    assert!(!aliases().contains(&"claude-cli/haiku".to_string()));
    assert!(aliases().contains(&"claude-cli/opus".to_string()));
    assert_eq!(service.status().disabled_models, ["claude-cli/haiku"]);
    assert_eq!(service.status().models.len(), MODELS.len());
    assert_eq!(
        post(&service, &service.key, &question(false))
            .await
            .status(),
        404
    );
    assert!(service
        .set_model_enabled("claude-cli/gpt-5.4", false)
        .is_err());
    assert!(service.set_model_enabled("haiku", false).is_err());

    let restarted = start_saved(&r, command);
    assert_eq!(restarted.status().disabled_models, ["claude-cli/haiku"]);
    restarted
        .set_model_enabled("claude-cli/haiku", true)
        .unwrap();
    assert!(aliases().contains(&"claude-cli/haiku".to_string()));
    assert_eq!(
        post(&restarted, &restarted.key, &question(false))
            .await
            .status(),
        200
    );

    for model in MODELS {
        restarted
            .set_model_enabled(&format!("claude-cli/{model}"), false)
            .unwrap();
    }
    assert!(aliases().is_empty());
}
