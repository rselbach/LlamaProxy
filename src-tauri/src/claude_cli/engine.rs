//! Drives the Claude Code CLI (`claude -p`) as the backend for a Claude
//! subscription. Claude Code keeps its own sign-in and makes the upstream
//! request; LlamaProxy never sees or stores its credentials.
//!
//! A conversation's turns go on in one Claude Code process, so Claude Code's
//! own record of the conversation, and with it the prompt cache, carries from
//! one request to the next. After a reply, the process is parked under a key
//! for the conversation as the client sees it; a request that continues that
//! conversation takes the process back and sends only what is new. Anything
//! else (a first turn, an edited or branched conversation, a process that was
//! let go) starts a new process that is told the whole conversation over
//! stream-json, with `shouldQuery: false` on all but the last message.
//!
//! The client's tools are listed by a loopback MCP server. When Claude calls
//! one, the call is shown to the client as the reply's tool use, and the MCP
//! request waits until the client's next request brings the result.

use super::http;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, MutexGuard, PoisonError, Weak},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines},
    net::TcpStream,
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{mpsc, oneshot, Notify},
    task::JoinHandle,
};

const MCP_SERVER: &str = "llamaproxy";
// how Claude Code names the tools of the adapter's MCP server
const TOOL_PREFIX: &str = "mcp__llamaproxy__";
// Claude Code keeps only [A-Za-z0-9_-] of an MCP tool name, up to 64
// characters with the prefix.
const TOOL_NAME_LIMIT: usize = 64;
// Each Claude Code process is a full CLI, so live processes, parked or
// answering, are bounded more tightly than proxied HTTP requests.
const MAX_PROCESSES: usize = 8;
// A parked process is let go after this long. Its prompt cache outlives it,
// but a later request then starts a new process told the whole conversation.
const PARK_TIMEOUT: Duration = Duration::from_secs(600);
// A stream with no output for this long is treated as stalled. Thinking
// streams deltas, so a live generation is never this quiet.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
// After the generation stops, Claude Code reports rate limits and its result
// within milliseconds; the turn ends once they arrive or this elapses.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(2);
const STATUS_TIMEOUT: Duration = Duration::from_secs(30);
// SIGTERM lets Claude Code release its sign-in refresh lock before it exits.
const EXIT_GRACE: Duration = Duration::from_secs(5);
const STDERR_TAIL_BYTES: usize = 8192;
const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
pub(super) const SESSION_HEADER: &str = "X-LlamaProxy-Claude-CLI-Session";
pub(super) const INSTALL_HINT: &str =
    "Claude Code was not found. Install it from https://code.claude.com and run `claude auth login`";

// These would route Claude Code away from the subscription sign-in, change the
// request it sends, or misreport how it was started. ANTHROPIC_BASE_URL in
// particular may point back at LlamaProxy.
const REMOVED_ENV: [&str; 15] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_CUSTOM_HEADERS",
    "ANTHROPIC_FOUNDRY_API_KEY",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_EXTRA_BODY",
    "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDECODE",
];

// These keep Claude Code to talking to the model: no updates, telemetry,
// memory, CLAUDE.md files, retries, compaction, or tool search hiding the
// client's tools. A client's tool call may run for a long time before its
// result arrives, longer than Claude Code's default wait.
const ADDED_ENV: [(&str, &str); 16] = [
    ("DISABLE_AUTOUPDATER", "1"),
    ("DISABLE_FEEDBACK_COMMAND", "1"),
    ("DISABLE_ERROR_REPORTING", "1"),
    ("DISABLE_TELEMETRY", "1"),
    ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
    ("CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL", "1"),
    ("CLAUDE_CODE_DISABLE_FILE_MENTIONS", "1"),
    ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1"),
    ("CLAUDE_CODE_DISABLE_CLAUDE_MDS", "1"),
    ("CLAUDE_CODE_MAX_RETRIES", "0"),
    ("DISABLE_AUTO_COMPACT", "1"),
    ("DISABLE_COMPACT", "1"),
    ("CLAUDE_CODE_TOTAL_TOKENS_REMINDER", "off"),
    ("ENABLE_TOOL_SEARCH", "false"),
    ("NO_COLOR", "1"),
    ("MCP_TOOL_TIMEOUT", "1800000"),
];

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn items(value: Option<&Value>) -> &[Value] {
    value
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn text_of(value: Option<&Value>) -> &str {
    value.and_then(Value::as_str).unwrap_or_default()
}

/// Looks for the Claude Code executable on PATH, then in the install locations
/// a desktop app's minimal PATH misses.
pub(super) fn find_command() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    };
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(name))
                .collect()
        })
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let home = PathBuf::from(home);
        candidates.push(home.join(".local").join("bin").join(name));
        candidates.push(home.join(".claude").join("local").join(name));
    }
    candidates.push(Path::new("/opt/homebrew/bin").join(name));
    candidates.push(Path::new("/usr/local/bin").join(name));
    candidates.into_iter().find(|candidate| candidate.is_file())
}

fn child_env(
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
    max_tokens: Option<u64>,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = inherited
        .into_iter()
        .filter(|(key, _)| {
            let key = key.to_string_lossy();
            let named = |name: &str| key.eq_ignore_ascii_case(name);
            !ADDED_ENV.iter().any(|(name, _)| named(name)) && !REMOVED_ENV.iter().any(|n| named(n))
        })
        .collect();
    env.extend(
        ADDED_ENV
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value))),
    );
    if let Some(max_tokens) = max_tokens {
        env.push((
            "CLAUDE_CODE_MAX_OUTPUT_TOKENS".into(),
            max_tokens.to_string().into(),
        ));
    }
    env
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct AuthStatus {
    logged_in: bool,
    auth_method: String,
    email: String,
    org_name: String,
}

/// Confirms that Claude Code is signed in to a Claude subscription and returns
/// a label for the account.
pub(super) async fn subscription_account(command: &Path) -> Result<String, String> {
    let mut status = Command::new(command);
    status
        .args(["auth", "status", "--json"])
        .env_clear()
        .envs(child_env(std::env::vars_os(), None))
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(STATUS_TIMEOUT, status.output())
        .await
        .map_err(|_| "`claude auth status` timed out".to_string())?
        .map_err(|e| format!("Could not run `claude auth status`: {e}"))?;
    let status: AuthStatus = serde_json::from_slice(&output.stdout).map_err(|_| {
        let stderr = String::from_utf8_lossy(&output.stderr);
        format!("Could not read `claude auth status`: {}", stderr.trim())
    })?;
    account_label(&status)
}

fn account_label(status: &AuthStatus) -> Result<String, String> {
    if !status.logged_in {
        return Err(
            "Claude Code is not signed in. Run `claude auth login` with your Claude subscription"
                .into(),
        );
    }
    // claude.ai is a subscription sign-in; oauth_token is a `claude
    // setup-token` grant for one. Anything else bills an API account.
    if status.auth_method != "claude.ai" && status.auth_method != "oauth_token" {
        return Err(format!(
            "Claude Code is signed in with \"{}\", not a Claude subscription. Run `claude auth login`",
            status.auth_method
        ));
    }
    Ok([&status.email, &status.org_name]
        .into_iter()
        .find(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| "Claude subscription".to_string()))
}

/// An Anthropic Messages request as Claude Code takes it.
#[derive(Debug, Default)]
struct Turn {
    model: String,
    stream: bool,
    system: String,
    // the MCP tools/list inventory
    tools: Vec<Value>,
    // maps a tool's name as Claude Code reports it back to the name the client declared
    client_tool_names: HashMap<String, String>,
    // the conversation as stream-json frames
    input: Vec<Value>,
    // the client's conversation, which keys a parked process
    messages: Vec<Value>,
    max_tokens: Option<u64>,
    effort: String,
    // covers what a process is started with, so a process is only reused for
    // a request it would have been started for
    settings_key: [u8; 32],
}

fn prepare_turn(request: &Value) -> Result<Turn, String> {
    let effort = request
        .pointer("/output_config/effort")
        .and_then(Value::as_str)
        .filter(|effort| EFFORTS.contains(effort));
    let mut turn = Turn {
        model: text_of(request.get("model")).to_string(),
        stream: request
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        system: system_text(request.get("system")),
        max_tokens: request.get("max_tokens").and_then(Value::as_u64),
        effort: effort.unwrap_or_default().to_string(),
        ..Turn::default()
    };
    let offer_tools = match text_of(request.pointer("/tool_choice/type")) {
        "" | "auto" => true,
        "none" => false,
        other => {
            return Err(format!(
                "Claude Code cannot force tool use; tool_choice type \"{other}\" is unsupported"
            ))
        }
    };
    if offer_tools {
        for tool in items(request.get("tools")) {
            let name = text_of(tool.get("name"));
            let kind = text_of(tool.get("type"));
            if name.is_empty() || (!kind.is_empty() && kind != "custom") {
                return Err(format!(
                    "Claude Code supports only client tools; got tool \"{name}\""
                ));
            }
            let claude_name = claude_tool_name(name);
            turn.tools.push(json!({
                "name": claude_name.trim_start_matches(TOOL_PREFIX),
                "description": text_of(tool.get("description")),
                "inputSchema": tool.get("input_schema").cloned().unwrap_or_else(|| json!({"type": "object"})),
            }));
            turn.client_tool_names.insert(claude_name, name.to_string());
        }
    }
    turn.messages = items(request.get("messages")).to_vec();
    turn.input = frames(&turn.messages)?;
    let settings = json!({
        "model": turn.model,
        "system": turn.system,
        "tools": turn.tools,
        "max_tokens": turn.max_tokens,
        "effort": turn.effort,
    });
    turn.settings_key = Sha256::digest(settings.to_string().as_bytes()).into();
    Ok(turn)
}

// Identifies a conversation as the client sees it: its words, tool calls, and
// tool results in order. It ignores what clients and the core drop or rewrite
// when a reply is sent back (thinking, ids, cache marks) and how they split it
// into messages and blocks.
fn conversation_key(settings_key: &[u8; 32], messages: &[Value]) -> [u8; 32] {
    let mut groups: Vec<(&str, Vec<String>)> = Vec::new();
    for message in messages {
        let role = text_of(message.get("role"));
        if groups.last().map(|(last, _)| *last) != Some(role) {
            groups.push((role, Vec::new()));
        }
        if let Some((_, parts)) = groups.last_mut() {
            conversation_parts(message.get("content"), parts);
        }
    }
    let mut hasher = Sha256::new();
    hasher.update(settings_key);
    for (role, parts) in groups.iter().filter(|(_, parts)| !parts.is_empty()) {
        hasher.update(format!("{}:{role}\0", parts.len()).as_bytes());
        for part in parts {
            hasher.update(format!("{}:", part.len()).as_bytes());
            hasher.update(part.as_bytes());
        }
    }
    hasher.finalize().into()
}

fn conversation_parts(content: Option<&Value>, parts: &mut Vec<String>) {
    let push_text = |parts: &mut Vec<String>, text: &str| {
        if text.is_empty() {
            return;
        }
        match parts.last_mut() {
            Some(last) if last.starts_with('t') => last.push_str(text),
            _ => parts.push(format!("t{text}")),
        }
    };
    let blocks = match content {
        Some(Value::String(text)) => return push_text(parts, text),
        Some(Value::Array(blocks)) => blocks,
        _ => return,
    };
    for block in blocks {
        match text_of(block.get("type")) {
            "text" => push_text(parts, text_of(block.get("text"))),
            "thinking" | "redacted_thinking" => {}
            "tool_use" => {
                let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                parts.push(format!("u{}\0{input}", text_of(block.get("name"))));
            }
            "tool_result" => {
                let error = block
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let mut result = Vec::new();
                conversation_parts(block.get("content"), &mut result);
                parts.push(format!("r{error}\0{}", result.join("\0")));
            }
            _ => {
                let mut block = block.clone();
                strip_cache_control(&mut block);
                parts.push(format!("o{block}"));
            }
        }
    }
}

// where the messages after the last reply begin: what a parked process has not seen yet
fn trailing_user_start(messages: &[Value]) -> usize {
    messages
        .iter()
        .rposition(|message| text_of(message.get("role")) == "assistant")
        .map_or(0, |index| index + 1)
}

// A client's tool_result as the MCP result Claude Code waits for. Words the
// client sent beside its results go with the last one.
fn mcp_tool_result(block: &Value, words: &[String]) -> Value {
    let mut content: Vec<Value> = match block.get("content") {
        Some(Value::String(text)) => vec![json!({"type": "text", "text": text})],
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|part| {
                let source = part.get("source");
                match (
                    text_of(part.get("type")),
                    text_of(source.and_then(|s| s.get("type"))),
                ) {
                    ("text", _) => json!({"type": "text", "text": text_of(part.get("text"))}),
                    ("image", "base64") => json!({
                        "type": "image",
                        "data": text_of(source.and_then(|s| s.get("data"))),
                        "mimeType": text_of(source.and_then(|s| s.get("media_type"))),
                    }),
                    _ => json!({"type": "text", "text": part.to_string()}),
                }
            })
            .collect(),
        _ => Vec::new(),
    };
    content.extend(
        words
            .iter()
            .map(|text| json!({"type": "text", "text": text})),
    );
    json!({
        "content": content,
        "isError": block.get("is_error").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn system_text(system: Option<&Value>) -> String {
    match system {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|block| text_of(block.get("text")))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

// The name Claude Code gives a client tool. Names it would rewrite are cleaned
// and suffixed with a hash of the original, so two tools never collapse into
// one and every request maps a name the same way.
fn claude_tool_name(name: &str) -> String {
    let room = TOOL_NAME_LIMIT - TOOL_PREFIX.len();
    let allowed = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    if !name.is_empty() && name.len() <= room && name.chars().all(allowed) {
        return format!("{TOOL_PREFIX}{name}");
    }
    let digest = Sha256::digest(name.as_bytes());
    let suffix: String = digest[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let cleaned: String = name
        .chars()
        .map(|c| if allowed(c) { c } else { '_' })
        .take(room - suffix.len() - 1)
        .collect();
    format!("{TOOL_PREFIX}{cleaned}_{suffix}")
}

// Renders the conversation as stream-json user and assistant frames. Every
// frame but the last is history Claude Code records without asking the model.
fn frames(messages: &[Value]) -> Result<Vec<Value>, String> {
    let Some(last) = messages.last() else {
        return Err("The Messages request has no messages".into());
    };
    if text_of(last.get("role")) != "user" {
        return Err(
            "Claude Code cannot continue an assistant prefill; the last message must be from the user"
                .into(),
        );
    }
    let mut out = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let role = text_of(message.get("role"));
        if role != "user" && role != "assistant" {
            return Err(format!("Message {index} has unsupported role \"{role}\""));
        }
        let content = frame_content(message.get("content"), role == "assistant");
        let querying = index + 1 == messages.len();
        if content.is_empty() {
            if querying {
                return Err("The last message has no content".into());
            }
            continue;
        }
        let mut frame = json!({"type": role, "message": {"role": role, "content": content}});
        if !querying {
            frame["shouldQuery"] = Value::Bool(false);
        }
        out.push(frame);
    }
    Ok(out)
}

// Normalizes a message's content to blocks. Claude Code places its own cache
// breakpoints, so the client's are dropped, and an assistant's tool calls are
// renamed to the tools Claude Code knows.
fn frame_content(content: Option<&Value>, assistant: bool) -> Vec<Value> {
    let blocks = match content {
        Some(Value::String(text)) if text.is_empty() => return Vec::new(),
        Some(Value::String(text)) => return vec![json!({"type": "text", "text": text})],
        Some(Value::Array(blocks)) => blocks,
        _ => return Vec::new(),
    };
    blocks
        .iter()
        .map(|block| {
            let mut block = block.clone();
            strip_cache_control(&mut block);
            if assistant && text_of(block.get("type")) == "tool_use" {
                if let Some(name) = block.get("name").and_then(Value::as_str) {
                    block["name"] = Value::String(claude_tool_name(name));
                }
            }
            block
        })
        .collect()
}

fn strip_cache_control(block: &mut Value) {
    let Some(object) = block.as_object_mut() else {
        return;
    };
    object.remove("cache_control");
    if let Some(Value::Array(nested)) = object.get_mut("content") {
        nested.iter_mut().for_each(strip_cache_control);
    }
}

// The client's system prompt is appended to Claude Code's own, never a
// replacement for it.
fn command_args(turn: &Turn, system_path: Option<&Path>, mcp_path: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-p",
        "--model",
        turn.model.as_str(),
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--tools",
        "",
        "--setting-sources",
        "",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--permission-mode",
        "dontAsk",
        "--allowedTools",
        "mcp__llamaproxy",
        "--no-session-persistence",
        "--mcp-config",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.push(mcp_path.into());
    if let Some(system_path) = system_path {
        args.push("--append-system-prompt-file".into());
        args.push(system_path.into());
    }
    if !turn.effort.is_empty() {
        args.push("--effort".into());
        args.push(turn.effort.clone().into());
    }
    args
}

/// An error to report as an Anthropic error of `kind` with an HTTP status.
#[derive(Clone, Debug)]
struct Failure {
    status: u16,
    kind: String,
    message: String,
}

impl Failure {
    fn api(message: impl Into<String>) -> Self {
        Self {
            status: 502,
            kind: "api_error".into(),
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self {
            status: 400,
            kind: "invalid_request_error".into(),
            message: message.into(),
        }
    }

    // maps the error Claude Code attaches to an assistant message it
    // synthesized instead of a model reply
    fn from_claude(error: &str, message: String) -> Self {
        let (status, kind) = match error {
            "authentication_failed" => (401, "authentication_error"),
            "billing_error" => (402, "billing_error"),
            "rate_limit" => (429, "rate_limit_error"),
            "invalid_request" => (400, "invalid_request_error"),
            "overloaded" => (529, "overloaded_error"),
            _ => (502, "api_error"),
        };
        let message = if message.is_empty() {
            format!("Claude Code reported {error}")
        } else {
            message
        };
        Self {
            status,
            kind: kind.into(),
            message,
        }
    }

    fn from_event(event: &Value) -> Self {
        let kind = text_of(event.pointer("/error/type"));
        let status = match kind {
            "authentication_error" => 401,
            "billing_error" => 402,
            "invalid_request_error" => 400,
            "rate_limit_error" => 429,
            "overloaded_error" => 529,
            _ => 502,
        };
        Self {
            status,
            kind: if kind.is_empty() { "api_error" } else { kind }.into(),
            message: text_of(event.pointer("/error/message")).to_string(),
        }
    }

    fn event(&self) -> Value {
        json!({"type": "error", "error": {"type": self.kind, "message": self.message}})
    }

    async fn write(&self, writer: &mut (impl AsyncWriteExt + Unpin)) -> Result<(), String> {
        http::write_json(
            writer,
            self.status,
            &[],
            self.event().to_string().as_bytes(),
        )
        .await
    }
}

#[derive(Debug)]
enum Line {
    // an Anthropic Messages stream event from the API response
    Event(Value),
    // a message Claude Code synthesized for an error, such as a failed sign-in or a rate limit
    Failure(Failure),
    RateLimit(Value),
    Result {
        is_error: bool,
        num_turns: i64,
        text: String,
    },
    Other,
}

fn parse_line(line: &str) -> Result<Line, String> {
    let value: Value = serde_json::from_str(line).map_err(|e| {
        let start: String = line.chars().take(300).collect();
        format!("Could not decode Claude Code output: {e}: {start}")
    })?;
    Ok(match text_of(value.get("type")) {
        "stream_event" => value
            .get("event")
            .map_or(Line::Other, |event| Line::Event(event.clone())),
        "assistant" => match value.get("error").and_then(Value::as_str) {
            Some(error) => {
                let text: Vec<&str> = items(value.pointer("/message/content"))
                    .iter()
                    .map(|block| text_of(block.get("text")))
                    .collect();
                Line::Failure(Failure::from_claude(error, text.join("\n")))
            }
            None => Line::Other,
        },
        "rate_limit_event" => {
            Line::RateLimit(value.get("rate_limit_info").cloned().unwrap_or(Value::Null))
        }
        "result" => Line::Result {
            is_error: value
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            num_turns: value.get("num_turns").and_then(Value::as_i64).unwrap_or(0),
            text: text_of(value.get("result")).to_string(),
        },
        _ => Line::Other,
    })
}

const USAGE_CREDITS_MESSAGE: &str = "Claude Code charged this request to Claude usage credits instead of your plan \
     limits. LlamaProxy ended the reply so further requests are not charged; turn off usage credits at \
     claude.ai/settings/usage to stay on plan limits";

// maps a tool call Claude Code streamed back to the name the client declared
fn rename_tool_call(event: &mut Value, client_tool_names: &HashMap<String, String>) {
    if text_of(event.get("type")) != "content_block_start" {
        return;
    }
    let Some(block) = event.get_mut("content_block") else {
        return;
    };
    if text_of(block.get("type")) != "tool_use" {
        return;
    }
    let Some(name) = block.get("name").and_then(Value::as_str) else {
        return;
    };
    let client_name = match client_tool_names.get(name) {
        Some(client_name) => client_name.clone(),
        None => name.strip_prefix(TOOL_PREFIX).unwrap_or(name).to_string(),
    };
    block["name"] = Value::String(client_name);
}

fn sse_frame(event: &Value) -> Vec<u8> {
    format!("event: {}\ndata: {event}\n\n", text_of(event.get("type"))).into_bytes()
}

// Assembles a whole Messages response from stream events, for requests that
// did not ask for a stream and to key the parked process by its reply.
#[derive(Debug, Default)]
struct MessageAccumulator {
    message: Map<String, Value>,
    content: Vec<Value>,
    partial_json: HashMap<usize, String>,
}

impl MessageAccumulator {
    fn apply(&mut self, event: &Value) -> Result<(), String> {
        let kind = text_of(event.get("type"));
        let index = || {
            event
                .get("index")
                .and_then(Value::as_u64)
                .map(|index| index as usize)
                .ok_or_else(|| format!("{kind} has no index"))
        };
        match kind {
            "message_start" => {
                self.message = event
                    .get("message")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                self.content.clear();
            }
            "content_block_start" => {
                let index = index()?;
                if self.content.len() <= index {
                    self.content.resize(index + 1, Value::Null);
                }
                self.content[index] = event.get("content_block").cloned().unwrap_or(Value::Null);
            }
            "content_block_delta" => {
                let index = index()?;
                let delta = event.get("delta").cloned().unwrap_or(Value::Null);
                let Some(block) = self.content.get_mut(index) else {
                    return Err(format!("content_block_delta for unknown block {index}"));
                };
                let append = |block: &mut Value, field: &str, text: &str| {
                    let current = text_of(block.get(field)).to_string();
                    block[field] = Value::String(current + text);
                };
                match text_of(delta.get("type")) {
                    "text_delta" => append(block, "text", text_of(delta.get("text"))),
                    "thinking_delta" => append(block, "thinking", text_of(delta.get("thinking"))),
                    "signature_delta" => {
                        append(block, "signature", text_of(delta.get("signature")))
                    }
                    "input_json_delta" => self
                        .partial_json
                        .entry(index)
                        .or_default()
                        .push_str(text_of(delta.get("partial_json"))),
                    "citations_delta" => {
                        if let Some(citation) = delta.get("citation") {
                            match block.get_mut("citations").and_then(Value::as_array_mut) {
                                Some(citations) => citations.push(citation.clone()),
                                None => block["citations"] = json!([citation]),
                            }
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = index()?;
                let Some(raw) = self.partial_json.remove(&index) else {
                    return Ok(());
                };
                let Some(block) = self.content.get_mut(index) else {
                    return Ok(());
                };
                block["input"] = if raw.trim().is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(&raw)
                        .map_err(|e| format!("Could not decode tool input of block {index}: {e}"))?
                };
            }
            "message_delta" => {
                if let Some(delta) = event.get("delta").and_then(Value::as_object) {
                    for (key, value) in delta {
                        self.message.insert(key.clone(), value.clone());
                    }
                }
                if let Some(usage) = event.get("usage").and_then(Value::as_object) {
                    let merged = self.message.entry("usage").or_insert_with(|| json!({}));
                    if let Some(merged) = merged.as_object_mut() {
                        for (key, value) in usage {
                            merged.insert(key.clone(), value.clone());
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(mut self) -> Value {
        self.message
            .insert("content".into(), Value::Array(self.content));
        Value::Object(self.message)
    }
}

fn create_private_dir(path: &Path, recursive: bool) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(recursive);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)
}

// A private directory for one process's system prompt and MCP configuration
// while the process starts.
struct TurnDir(PathBuf);

impl TurnDir {
    fn create() -> Result<Self, String> {
        let path =
            std::env::temp_dir().join(format!("llamaproxy-claude-{}", &super::random_key()?[..24]));
        create_private_dir(&path, false)
            .map_err(|e| format!("Could not create a Claude Code run directory: {e}"))?;
        Ok(Self(path))
    }

    fn write(&self, name: &str, contents: &[u8]) -> Result<PathBuf, String> {
        let path = self.0.join(name);
        std::fs::write(&path, contents)
            .map_err(|e| format!("Could not write Claude Code {name}: {e}"))?;
        Ok(path)
    }
}

impl Drop for TurnDir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("Could not remove a Claude Code run directory: {error}");
            }
        }
    }
}

// Claude Code calling one of the client's tools. Its MCP request waits until
// `reply` carries the client's result.
struct ToolCall {
    name: String,
    arguments: Value,
    // the call's id when Claude Code names it in `_meta`
    tool_use_id: String,
    reply: oneshot::Sender<Value>,
}

// The loopback MCP server that lists the client's tools to one Claude Code
// process and hands its tool calls to the process's session. Its path carries
// a random token.
struct ToolInventory {
    url: String,
    server: JoinHandle<()>,
}

impl ToolInventory {
    async fn start(tools: Vec<Value>) -> Result<(Self, mpsc::UnboundedReceiver<ToolCall>), String> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| format!("Could not listen for Claude Code tools: {e}"))?;
        let address = listener
            .local_addr()
            .map_err(|e| format!("Could not locate the Claude Code tools server: {e}"))?;
        let route = format!("/mcp/{}", super::random_key()?);
        let url = format!("http://{address}{route}");
        let tools = Arc::new(tools);
        let (calls, received) = mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let (route, tools, calls) = (route.clone(), tools.clone(), calls.clone());
                tokio::spawn(async move {
                    if let Err(error) =
                        serve_tool_request(&mut socket, &route, &tools, &calls).await
                    {
                        eprintln!("Could not answer a Claude Code tools request: {error}");
                    }
                });
            }
        });
        Ok((Self { url, server }, received))
    }

    fn mcp_config(&self) -> Vec<u8> {
        json!({"mcpServers": {MCP_SERVER: {"type": "http", "url": self.url}}})
            .to_string()
            .into_bytes()
    }
}

impl Drop for ToolInventory {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn serve_tool_request(
    socket: &mut TcpStream,
    route: &str,
    tools: &[Value],
    calls: &mpsc::UnboundedSender<ToolCall>,
) -> Result<(), String> {
    let request =
        match tokio::time::timeout(Duration::from_secs(10), http::read_request(socket)).await {
            Ok(Ok(request)) => request,
            Ok(Err((status, message))) => {
                return http::write_response(socket, status, &[], message.as_bytes()).await
            }
            Err(_) => return Ok(()),
        };
    if request.path() != route {
        return http::write_response(socket, 404, &[], b"").await;
    }
    if request.method != "POST" {
        return http::write_response(socket, 405, &[("Allow", "POST")], b"").await;
    }
    let reply = match serde_json::from_slice::<Value>(&request.body) {
        Ok(message)
            if text_of(message.get("method")) == "tools/call" && message.get("id").is_some() =>
        {
            Some(tool_call_reply(&message, calls).await)
        }
        Ok(message) => inventory_reply(&message, tools),
        Err(_) => Some(
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "Parse error"}}),
        ),
    };
    match reply {
        None => http::write_response(socket, 202, &[], b"").await,
        Some(reply) => http::write_json(socket, 200, &[], reply.to_string().as_bytes()).await,
    }
}

// hands a tools/call to the session and answers with the client's result once it arrives
async fn tool_call_reply(request: &Value, calls: &mpsc::UnboundedSender<ToolCall>) -> Value {
    let (reply, result) = oneshot::channel();
    let call = ToolCall {
        name: text_of(request.pointer("/params/name")).to_string(),
        arguments: request
            .pointer("/params/arguments")
            .cloned()
            .unwrap_or_else(|| json!({})),
        tool_use_id: text_of(
            request
                .pointer("/params/_meta")
                .and_then(|meta| meta.get("claudecode/toolUseId")),
        )
        .to_string(),
        reply,
    };
    let result = match calls.send(call) {
        Ok(()) => result.await.ok(),
        Err(_) => None,
    };
    let result = result.unwrap_or_else(|| {
        json!({"isError": true, "content": [{"type": "text", "text": "The client did not return this tool's result."}]})
    });
    json!({"jsonrpc": "2.0", "id": request.get("id"), "result": result})
}

// answers the MCP messages other than tools/call; notifications get no reply
fn inventory_reply(request: &Value, tools: &[Value]) -> Option<Value> {
    let id = request.get("id")?.clone();
    let result = match text_of(request.get("method")) {
        "initialize" => json!({
            "protocolVersion": request.pointer("/params/protocolVersion").cloned().unwrap_or_else(|| "2025-06-18".into()),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": MCP_SERVER, "version": env!("CARGO_PKG_VERSION")},
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools}),
        _ => {
            return Some(
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Method not found"}}),
            )
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

// A tool call the client was shown and has yet to answer, named as the MCP
// server lists the tool.
#[derive(Clone, Debug, PartialEq)]
struct ExpectedCall {
    id: String,
    name: String,
    input: Value,
}

fn expected_calls(reply: &Value) -> Vec<ExpectedCall> {
    items(reply.get("content"))
        .iter()
        .filter(|block| text_of(block.get("type")) == "tool_use")
        .map(|block| ExpectedCall {
            id: text_of(block.get("id")).to_string(),
            name: claude_tool_name(text_of(block.get("name")))
                .trim_start_matches(TOOL_PREFIX)
                .to_string(),
            input: block.get("input").cloned().unwrap_or_else(|| json!({})),
        })
        .collect()
}

// Pairs the client's next messages with the calls it was shown, returning each
// call's MCP result. Results are matched by call id, then by position for
// clients that rename ids. None means the messages do not just answer the
// calls, so the process cannot go on with them.
fn match_tool_results(
    expected: &[ExpectedCall],
    messages: &[Value],
) -> Option<Vec<(ExpectedCall, Value)>> {
    let mut results = Vec::new();
    let mut words = Vec::new();
    for message in messages {
        for block in frame_content(message.get("content"), false) {
            match text_of(block.get("type")) {
                "tool_result" => results.push(block),
                "text" => words.push(text_of(block.get("text")).to_string()),
                _ => return None,
            }
        }
    }
    if results.len() != expected.len() || results.is_empty() {
        return None;
    }
    let answers =
        |call: &ExpectedCall, result: &&Value| text_of(result.get("tool_use_id")) == call.id;
    let by_id = expected
        .iter()
        .all(|call| results.iter().any(|result| answers(call, &result)));
    let last = expected.len() - 1;
    let matched = expected
        .iter()
        .enumerate()
        .map(|(index, call)| {
            let result = if by_id {
                results.iter().find(|result| answers(call, result))
            } else {
                results.get(index)
            };
            let words: &[String] = if index == last { &words } else { &[] };
            (
                call.clone(),
                mcp_tool_result(result.unwrap_or(&Value::Null), words),
            )
        })
        .collect();
    Some(matched)
}

/// The live Claude Code processes: at most MAX_PROCESSES, answering or parked
/// between turns. A new process lets the oldest parked one go when the pool is
/// full, and waits only when every process is answering.
#[derive(Default)]
pub(super) struct Pool {
    state: Mutex<PoolState>,
    freed: Notify,
}

#[derive(Default)]
struct PoolState {
    live: usize,
    parked: Vec<Parked>,
    next_id: u64,
}

struct Parked {
    id: u64,
    key: [u8; 32],
    session: Session,
}

// one live process's place in its pool
struct Slot(Weak<Pool>);

impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(pool) = self.0.upgrade() {
            lock(&pool.state).live -= 1;
            pool.freed.notify_one();
        }
    }
}

impl Pool {
    async fn slot(self: &Arc<Self>) -> Slot {
        loop {
            let evicted = {
                let mut state = lock(&self.state);
                if state.live < MAX_PROCESSES {
                    state.live += 1;
                    return Slot(Arc::downgrade(self));
                }
                (!state.parked.is_empty()).then(|| state.parked.remove(0))
            };
            // Dropping the parked session ends its process and frees its slot.
            match evicted {
                Some(parked) => drop(parked),
                None => self.freed.notified().await,
            }
        }
    }

    fn take(&self, key: &[u8; 32]) -> Option<Session> {
        let mut state = lock(&self.state);
        let index = state.parked.iter().position(|parked| parked.key == *key)?;
        Some(state.parked.remove(index).session)
    }

    fn park(self: &Arc<Self>, key: [u8; 32], session: Session) {
        let id = {
            let mut state = lock(&self.state);
            state.next_id += 1;
            let id = state.next_id;
            state.parked.push(Parked { id, key, session });
            id
        };
        let pool = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(PARK_TIMEOUT).await;
            let Some(pool) = pool.upgrade() else {
                return;
            };
            let expired = {
                let mut state = lock(&pool.state);
                state
                    .parked
                    .iter()
                    .position(|parked| parked.id == id)
                    .map(|index| state.parked.remove(index))
            };
            drop(expired);
        });
    }

    /// Ends every parked process.
    pub(super) fn clear(&self) {
        let parked = std::mem::take(&mut lock(&self.state).parked);
        drop(parked);
    }

    #[cfg(test)]
    pub(super) fn parked(&self) -> usize {
        lock(&self.state).parked.len()
    }
}

// Ends its process when dropped, which is how a client that walks away
// mid-reply stops the generation.
struct Process(Option<Child>);

impl Process {
    fn running(&mut self) -> bool {
        self.0
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
    }

    async fn exit_status(&mut self) -> Option<std::process::ExitStatus> {
        let child = self.0.as_mut()?;
        tokio::time::timeout(EXIT_GRACE, child.wait())
            .await
            .ok()?
            .ok()
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else {
            return;
        };
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        #[cfg(unix)]
        let asked = child.id().is_some_and(|pid| {
            // SAFETY: kill only signals the child this handle still owns and has not reaped.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) == 0 }
        });
        #[cfg(not(unix))]
        let asked = false;
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            if let Err(error) = child.start_kill() {
                eprintln!("Could not stop a Claude Code process: {error}");
            }
            return;
        };
        runtime.spawn(async move {
            if asked && tokio::time::timeout(EXIT_GRACE, child.wait()).await.is_ok() {
                return;
            }
            if let Err(error) = child.kill().await {
                eprintln!("Could not stop a Claude Code process: {error}");
            }
        });
    }
}

// Owns one Claude Code process and everything it needs.
struct Session {
    process: Process,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    stderr: JoinHandle<Vec<u8>>,
    calls: mpsc::UnboundedReceiver<ToolCall>,
    // tool calls Claude Code made that the client has not answered yet
    held_calls: Vec<ToolCall>,
    // results waiting for Claude Code's call
    answers: Vec<(ExpectedCall, Value)>,
    // the last reply's tool calls, for the next request
    expected: Vec<ExpectedCall>,
    client_tool_names: HashMap<String, String>,
    // finished queries whose result line is still unread, so a later turn
    // does not take it for its own
    stale_results: usize,
    started: bool,
    stopped: bool,
    failed: bool,
    stop_reason: String,
    usage_credits: bool,
    _inventory: ToolInventory,
    // holds the prompt and MCP files until Claude Code has read them
    dir: Option<TurnDir>,
    _slot: Slot,
}

impl Session {
    async fn start(engine: &Engine, turn: &Turn) -> Result<Self, String> {
        let slot = engine.pool.slot().await;
        let command = engine
            .command
            .clone()
            .or_else(find_command)
            .ok_or(INSTALL_HINT)?;
        create_private_dir(&engine.work_dir, true)
            .map_err(|e| format!("Could not create the Claude Code working directory: {e}"))?;
        let dir = TurnDir::create()?;
        let (inventory, calls) = ToolInventory::start(turn.tools.clone()).await?;
        let system_path = match turn.system.is_empty() {
            true => None,
            false => Some(dir.write("system.md", turn.system.as_bytes())?),
        };
        let mcp_path = dir.write("mcp.json", &inventory.mcp_config())?;
        // Claude Code writes its working directory into the system prompt, so
        // one empty directory for every process keeps the prompt cache shared.
        let mut child = Command::new(&command)
            .args(command_args(turn, system_path.as_deref(), &mcp_path))
            .current_dir(&engine.work_dir)
            .env_clear()
            .envs(child_env(std::env::vars_os(), turn.max_tokens))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Could not start Claude Code ({}): {e}", command.display()))?;
        let streams = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let process = Process(Some(child));
        let (Some(stdin), Some(stdout), Some(mut stderr)) = streams else {
            return Err("Claude Code started without its standard streams".into());
        };
        let stderr = tokio::spawn(async move {
            let mut tail = Vec::new();
            let mut buffer = [0u8; 4096];
            while let Ok(read) = stderr.read(&mut buffer).await {
                if read == 0 {
                    break;
                }
                tail.extend_from_slice(&buffer[..read]);
                if tail.len() > STDERR_TAIL_BYTES {
                    tail.drain(..tail.len() - STDERR_TAIL_BYTES);
                }
            }
            tail
        });
        Ok(Self {
            process,
            stdin,
            lines: BufReader::new(stdout).lines(),
            stderr,
            calls,
            held_calls: Vec::new(),
            answers: Vec::new(),
            expected: Vec::new(),
            client_tool_names: turn.client_tool_names.clone(),
            stale_results: 0,
            started: false,
            stopped: false,
            failed: false,
            stop_reason: String::new(),
            usage_credits: false,
            _inventory: inventory,
            dir: Some(dir),
            _slot: slot,
        })
    }

    // Performs the stream-json handshake the Agent SDK opens with, then tells
    // Claude Code the conversation. Claude Code takes history in order only
    // after the handshake.
    async fn open(&mut self, frames: &[Value]) -> Result<(), Failure> {
        self.write(&json!({
            "type": "control_request",
            "request_id": "llamaproxy-initialize",
            "request": {"subtype": "initialize"},
        }))
        .await?;
        loop {
            let Some(line) = self.next_line(STATUS_TIMEOUT).await? else {
                return Err(self.exit_failure().await);
            };
            let handshake = serde_json::from_str::<Value>(&line)
                .is_ok_and(|value| text_of(value.get("type")) == "control_response");
            if handshake {
                break;
            }
        }
        // Claude Code has read its prompt and MCP files by its handshake reply.
        self.dir = None;
        self.send_frames(frames).await
    }

    async fn write(&mut self, frame: &Value) -> Result<(), Failure> {
        let mut line = frame.to_string();
        line.push('\n');
        let written = match self.stdin.write_all(line.as_bytes()).await {
            Ok(()) => self.stdin.flush().await,
            Err(error) => Err(error),
        };
        written.map_err(|e| Failure::api(format!("Could not write to Claude Code: {e}")))
    }

    // Writes messages one at a time. Claude Code records an assistant message
    // as soon as it reads it but queues a user message, so each user message
    // of the history is acknowledged before the next message is written;
    // written together, replies land ahead of the messages they answer and
    // tool results are lost.
    async fn send_frames(&mut self, frames: &[Value]) -> Result<(), Failure> {
        for frame in frames {
            self.write(frame).await?;
            let history = frame.get("shouldQuery") == Some(&Value::Bool(false));
            if history && text_of(frame.get("type")) == "user" {
                self.acknowledged().await?;
            }
        }
        Ok(())
    }

    // waits for the zero-turn result Claude Code reports for a history message
    async fn acknowledged(&mut self) -> Result<(), Failure> {
        loop {
            let Some(line) = self.next_line(IDLE_TIMEOUT).await? else {
                return Err(self.exit_failure().await);
            };
            if line.trim().is_empty() {
                continue;
            }
            match parse_line(&line).map_err(Failure::api)? {
                Line::Result {
                    is_error: false,
                    num_turns: 0,
                    ..
                } => return Ok(()),
                Line::Result { .. } if self.stale_results > 0 => self.stale_results -= 1,
                Line::Result { text, .. } => {
                    return Err(Failure::api(format!(
                        "Claude Code refused the conversation's history: {text}"
                    )))
                }
                Line::Failure(failure) => return Err(failure),
                Line::RateLimit(info) => self.note_rate_limit(&info),
                Line::Event(_) | Line::Other => {}
            }
        }
    }

    // Continues the conversation with the messages the process has not seen.
    // It reports whether the process could take them.
    async fn resume(&mut self, messages: &[Value]) -> bool {
        if !self.process.running() {
            return false;
        }
        if self.expected.is_empty() {
            let answers_calls = messages.iter().any(|message| {
                items(message.get("content"))
                    .iter()
                    .any(|block| text_of(block.get("type")) == "tool_result")
            });
            return match frames(messages) {
                Ok(frames) if !answers_calls => self.send_frames(&frames).await.is_ok(),
                _ => false,
            };
        }
        let Some(answers) = match_tool_results(&self.expected, messages) else {
            return false;
        };
        self.expected.clear();
        self.answers = answers;
        for call in std::mem::take(&mut self.held_calls) {
            self.answer(call);
        }
        true
    }

    // replies to a tool call with the client's result, or holds it until the
    // client's next request brings one
    fn answer(&mut self, call: ToolCall) {
        let by_id = self.answers.iter().position(|(expected, _)| {
            !call.tool_use_id.is_empty() && expected.id == call.tool_use_id
        });
        let position = by_id.or_else(|| {
            self.answers.iter().position(|(expected, _)| {
                expected.name == call.name && expected.input == call.arguments
            })
        });
        let Some(position) = position else {
            self.held_calls.push(call);
            return;
        };
        let (_, result) = self.answers.remove(position);
        if call.reply.send(result).is_err() {
            eprintln!("Claude Code stopped waiting for a tool result");
        }
    }

    fn begin_turn(&mut self) {
        self.started = false;
        self.stopped = false;
        self.stop_reason.clear();
    }

    // Reads Claude Code's output, answering the tool calls it makes meanwhile.
    // None means the output ended.
    async fn next_line(&mut self, timeout: Duration) -> Result<Option<String>, Failure> {
        loop {
            tokio::select! {
                line = tokio::time::timeout(timeout, self.lines.next_line()) => return match line {
                    Err(_) => Err(Failure::api("Claude Code stopped responding")),
                    Ok(Err(error)) => Err(Failure::api(format!("Could not read Claude Code output: {error}"))),
                    Ok(Ok(line)) => Ok(line),
                },
                Some(call) = self.calls.recv() => self.answer(call),
            }
        }
    }

    // returns the next stream event of this turn's reply, and None once it has stopped
    async fn next_event(&mut self) -> Result<Option<Value>, Failure> {
        if self.stopped {
            return Ok(None);
        }
        match self.read_event().await {
            Ok(event) => Ok(Some(event)),
            Err(failure) => {
                self.failed = true;
                Err(failure)
            }
        }
    }

    async fn read_event(&mut self) -> Result<Value, Failure> {
        loop {
            let Some(line) = self.next_line(IDLE_TIMEOUT).await? else {
                return Err(self.exit_failure().await);
            };
            if line.trim().is_empty() {
                continue;
            }
            match parse_line(&line).map_err(Failure::api)? {
                Line::Event(mut event) => {
                    let kind = text_of(event.get("type")).to_string();
                    if kind == "message_start" {
                        self.started = true;
                    }
                    if !self.started {
                        continue;
                    }
                    if kind == "error" {
                        self.stopped = true;
                        self.failed = true;
                        return Ok(event);
                    }
                    if kind == "message_delta" {
                        if let Some(reason) =
                            event.pointer("/delta/stop_reason").and_then(Value::as_str)
                        {
                            self.stop_reason = reason.to_string();
                        }
                    }
                    rename_tool_call(&mut event, &self.client_tool_names);
                    if kind == "message_stop" {
                        self.stopped = true;
                        self.settle().await;
                        if self.usage_credits {
                            self.failed = true;
                            let failure = Failure {
                                status: 402,
                                kind: "billing_error".into(),
                                message: USAGE_CREDITS_MESSAGE.into(),
                            };
                            return Ok(failure.event());
                        }
                    }
                    return Ok(event);
                }
                Line::Failure(failure) => return Err(failure),
                Line::RateLimit(info) => self.note_rate_limit(&info),
                // Claude Code acknowledges each history message with a
                // zero-turn result before it answers the last one.
                Line::Result {
                    is_error: false,
                    num_turns: 0,
                    ..
                } => {}
                Line::Result { .. } if self.stale_results > 0 => self.stale_results -= 1,
                Line::Result { text, .. } => {
                    return Err(Failure::api(if text.is_empty() {
                        "Claude Code finished without a reply".to_string()
                    } else {
                        text
                    }))
                }
                Line::Other => {}
            }
        }
    }

    // Waits briefly after a reply for the rate limits and, when the reply ends
    // the query, the result that follow it. A reply that calls tools ends no
    // query: Claude Code goes on to call them and waits.
    async fn settle(&mut self) {
        let calls_tools = self.stop_reason == "tool_use";
        let deadline = tokio::time::Instant::now() + SETTLE_TIMEOUT;
        let mut finished = false;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let Ok(Some(line)) = self.next_line(remaining).await else {
                break;
            };
            match parse_line(&line) {
                Ok(Line::RateLimit(info)) => {
                    self.note_rate_limit(&info);
                    if calls_tools {
                        break;
                    }
                }
                Ok(Line::Result { .. }) => {
                    finished = true;
                    break;
                }
                _ => {}
            }
        }
        if !calls_tools && !finished {
            self.stale_results += 1;
        }
    }

    fn note_rate_limit(&mut self, info: &Value) {
        let overage = info
            .get("isUsingOverage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.usage_credits = self.usage_credits || overage;
    }

    // Parks the process for the conversation's next request, or lets it go
    // when the turn did not end cleanly.
    fn finish_turn(mut self, pool: &Arc<Pool>, turn: &Turn, reply: Value) {
        if self.failed || !self.stopped || !self.process.running() {
            return;
        }
        self.expected = match self.stop_reason.as_str() {
            "tool_use" => expected_calls(&reply),
            _ => Vec::new(),
        };
        let mut conversation = turn.messages.clone();
        conversation.push(reply);
        pool.park(conversation_key(&turn.settings_key, &conversation), self);
    }

    // describes a process that ended without a reply, with the end of its stderr
    async fn exit_failure(&mut self) -> Failure {
        let status = self.process.exit_status().await;
        let stderr = match tokio::time::timeout(Duration::from_secs(1), &mut self.stderr).await {
            Ok(Ok(tail)) => String::from_utf8_lossy(&tail).trim().to_string(),
            _ => String::new(),
        };
        let mut message = match status {
            Some(status) => format!("Claude Code exited ({status}) before its reply finished"),
            None => "Claude Code closed its output before its reply finished".to_string(),
        };
        if !stderr.is_empty() {
            message.push_str(": ");
            message.push_str(&stderr);
        }
        Failure::api(message)
    }
}

pub(super) struct Engine {
    pub(super) pool: Arc<Pool>,
    // overrides the search for the `claude` executable
    pub(super) command: Option<PathBuf>,
    pub(super) work_dir: PathBuf,
}

/// Answers an Anthropic Messages request through Claude Code. A failure before
/// the reply starts is an Anthropic error body with its status, a streamed turn
/// is an event stream, and any other turn is a whole Messages JSON body. An
/// error after a stream has started is returned so the caller closes the
/// connection without completing the stream.
pub(super) async fn respond(
    engine: &Engine,
    request: &Value,
    writer: &mut (impl AsyncWriteExt + Unpin),
) -> Result<(), String> {
    let turn = match prepare_turn(request) {
        Ok(turn) => turn,
        Err(message) => return Failure::invalid(message).write(writer).await,
    };
    let start = trailing_user_start(&turn.messages);
    let mut parked = None;
    if start > 0 {
        let key = conversation_key(&turn.settings_key, &turn.messages[..start]);
        if let Some(mut session) = engine.pool.take(&key) {
            if session.resume(&turn.messages[start..]).await {
                parked = Some(session);
            }
        }
    }
    let resumed = parked.is_some();
    let mut session = match parked {
        Some(session) => session,
        None => {
            let mut session = match Session::start(engine, &turn).await {
                Ok(session) => session,
                Err(message) => return Failure::api(message).write(writer).await,
            };
            if let Err(failure) = session.open(&turn.input).await {
                return failure.write(writer).await;
            }
            session
        }
    };
    session.begin_turn();
    let headers = [(SESSION_HEADER, if resumed { "resumed" } else { "started" })];
    let mut event = match session.next_event().await {
        Ok(Some(first)) => first,
        Ok(None) => {
            return Failure::api("Claude Code finished without a reply")
                .write(writer)
                .await
        }
        Err(failure) => return failure.write(writer).await,
    };
    if text_of(event.get("type")) == "error" {
        return Failure::from_event(&event).write(writer).await;
    }
    let mut accumulator = MessageAccumulator::default();
    if !turn.stream {
        loop {
            if text_of(event.get("type")) == "error" {
                return Failure::from_event(&event).write(writer).await;
            }
            if let Err(message) = accumulator.apply(&event) {
                return Failure::api(message).write(writer).await;
            }
            event = match session.next_event().await {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(failure) => return failure.write(writer).await,
            };
        }
        let reply = accumulator.finish();
        let body = reply.to_string();
        session.finish_turn(&engine.pool, &turn, reply);
        return http::write_json(writer, 200, &headers, body.as_bytes()).await;
    }
    http::start_event_stream(writer, &headers).await?;
    loop {
        if text_of(event.get("type")) == "error" {
            http::write_chunk(writer, &sse_frame(&event)).await?;
            return Err(Failure::from_event(&event).message);
        }
        if accumulator.apply(&event).is_err() {
            session.failed = true;
        }
        if session.stopped {
            // Park before the reply's last event: the client may send its next
            // request as soon as it reads it.
            session.finish_turn(&engine.pool, &turn, accumulator.finish());
            http::write_chunk(writer, &sse_frame(&event)).await?;
            return http::end_stream(writer).await;
        }
        http::write_chunk(writer, &sse_frame(&event)).await?;
        event = match session.next_event().await {
            Ok(Some(next)) => next,
            Ok(None) => return http::end_stream(writer).await,
            Err(failure) => {
                http::write_chunk(writer, &sse_frame(&failure.event())).await?;
                return Err(failure.message);
            }
        };
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
