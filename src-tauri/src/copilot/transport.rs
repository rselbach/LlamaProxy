use super::{Copilot, Endpoint, BRIDGE_PATH};
use futures_util::StreamExt;
use serde_json::Value;
use std::{net::TcpListener, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::Semaphore,
};

const MAX_HEADERS: usize = 16 * 1024;
const MAX_BODY: usize = 32 * 1024 * 1024;

// This is a private, single-request HTTP/1 adapter for the core, not a general
// HTTP server. Accept only length-delimited JSON, close every connection, and
// reject ambiguous framing before reading a body. No CORS or arbitrary URLs.
struct Request {
    endpoint: Endpoint,
    content_length: usize,
}

pub(super) async fn serve(listener: TcpListener, service: Arc<Copilot>) -> Result<(), String> {
    let listener = tokio::net::TcpListener::from_std(listener).map_err(|e| e.to_string())?;
    let capacity = Arc::new(Semaphore::new(32));
    loop {
        let (mut socket, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let Ok(permit) = capacity.clone().try_acquire_owned() else {
            continue;
        };
        let service = service.clone();
        tauri::async_runtime::spawn(async move {
            let _permit = permit;
            match tokio::time::timeout(
                Duration::from_secs(30),
                read_request(&mut socket, &service.key),
            )
            .await
            {
                Ok(Ok((endpoint, body))) => {
                    let (mut reader, mut writer) = socket.split();
                    let mut disconnect = [0u8; 1];
                    // Dropping forwarding cancels the upstream stream when the
                    // core/client disconnects, including while awaiting headers.
                    tokio::select! {
                        result = tokio::time::timeout(Duration::from_secs(900), forward(&service, endpoint, body, &mut writer)) => {
                            if !matches!(result, Ok(Ok(()))) {
                                // Close without a final chunk: never turn a broken stream into success.
                                if let Err(error) = writer.shutdown().await {
                                    eprintln!("Could not close a Copilot stream: {error}");
                                }
                            }
                        }
                        _ = reader.read(&mut disconnect) => {}
                    }
                }
                Ok(Err((status, message))) => {
                    if let Err(error) = write_error(&mut socket, status, message).await {
                        eprintln!("Could not send a Copilot adapter error: {error}");
                    }
                }
                Err(_) => {
                    if let Err(error) =
                        write_error(&mut socket, 408, "Copilot request timed out").await
                    {
                        eprintln!("Could not send a Copilot adapter timeout: {error}");
                    }
                }
            }
        });
    }
}

async fn read_request(
    socket: &mut TcpStream,
    key: &str,
) -> Result<(Endpoint, Value), (u16, &'static str)> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break end + 4;
        }
        if bytes.len() >= MAX_HEADERS {
            return Err((431, "Request headers are too large"));
        }
        let mut buffer = [0u8; 2048];
        let count = socket
            .read(&mut buffer)
            .await
            .map_err(|_| (400, "Could not read request"))?;
        if count == 0 {
            return Err((400, "Incomplete request"));
        }
        bytes.extend_from_slice(&buffer[..count]);
    };
    if header_end > MAX_HEADERS {
        return Err((431, "Request headers are too large"));
    }
    let request = parse_headers(&bytes[..header_end], key)?;
    if bytes.len() - header_end > request.content_length {
        return Err((400, "Unexpected data after request"));
    }
    let received = bytes.len();
    bytes.resize(header_end + request.content_length, 0);
    socket
        .read_exact(&mut bytes[received..])
        .await
        .map_err(|_| (400, "Incomplete request body"))?;
    let body: Value =
        serde_json::from_slice(&bytes[header_end..]).map_err(|_| (400, "Invalid request JSON"))?;
    if !body.is_object() {
        return Err((400, "Request JSON must be an object"));
    }
    Ok((request.endpoint, body))
}

fn parse_headers(bytes: &[u8], key: &str) -> Result<Request, (u16, &'static str)> {
    let text = std::str::from_utf8(bytes).map_err(|_| (400, "Invalid request headers"))?;
    let mut lines = text.split("\r\n");
    let mut request = lines.next().unwrap_or_default().split(' ');
    let method = request.next();
    let path = request.next().unwrap_or_default();
    let version = request.next();
    if method != Some("POST") || version != Some("HTTP/1.1") || request.next().is_some() {
        return Err((405, "Only HTTP/1.1 POST requests are supported"));
    }
    let path = path
        .strip_prefix(BRIDGE_PATH)
        .ok_or((404, "Unknown Copilot endpoint"))?;
    let endpoint = match path {
        "/chat/completions" => Endpoint::Chat,
        "/responses" => Endpoint::Responses,
        "/v1/messages" | "/v1/messages?beta=true" => Endpoint::Messages,
        _ => return Err((404, "Unsupported Copilot endpoint")),
    };
    let mut length = None;
    let mut authorization = None;
    let mut api_key = None;
    let mut content_type = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or((400, "Malformed request header"))?;
        if name.is_empty()
            || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
            || value.bytes().any(|c| c < 32 && c != b'\t')
        {
            return Err((400, "Invalid request header"));
        }
        let value = value.trim();
        let slot = match name.to_ascii_lowercase().as_str() {
            "transfer-encoding" | "expect" => return Err((400, "Unsupported request framing")),
            "content-length" => &mut length,
            "authorization" => &mut authorization,
            "x-api-key" => &mut api_key,
            "content-type" => &mut content_type,
            _ => continue,
        };
        if slot.replace(value).is_some() {
            return Err((400, "Duplicate request header"));
        }
    }
    let supplied = authorization
        .and_then(|v| v.strip_prefix("Bearer "))
        .or(api_key)
        .unwrap_or_default();
    if !same_key(supplied, key) {
        return Err((401, "Invalid Copilot adapter key"));
    }
    if !content_type.is_some_and(|v| {
        v.split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .eq_ignore_ascii_case("application/json")
    }) {
        return Err((415, "Copilot requests must be JSON"));
    }
    let length = length.ok_or((411, "Content-Length is required"))?;
    if length.is_empty() || !length.bytes().all(|c| c.is_ascii_digit()) {
        return Err((400, "Invalid Content-Length"));
    }
    let content_length = length
        .parse::<usize>()
        .map_err(|_| (413, "Request body is too large"))?;
    if content_length > MAX_BODY {
        return Err((413, "Request body is too large"));
    }
    Ok(Request {
        endpoint,
        content_length,
    })
}

fn same_key(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .fold(0, |difference, (a, b)| difference | (a ^ b))
            == 0
}

async fn forward(
    service: &Copilot,
    endpoint: Endpoint,
    mut body: Value,
    writer: &mut (impl AsyncWriteExt + Unpin),
) -> Result<(), String> {
    if endpoint == Endpoint::Responses {
        if let Err(message) = remove_codex_hosted_tool(&mut body) {
            return write_error(writer, 400, message).await;
        }
    }
    let Some(model) = body.get("model").and_then(Value::as_str) else {
        return write_error(writer, 400, "A Copilot model is required").await;
    };
    let (mut token, account_token) = {
        let mut state = service.state.lock().await;
        if !state.account.as_ref().is_some_and(|a| {
            a.models
                .iter()
                .any(|m| m.id == model && m.endpoint == endpoint)
        }) {
            return write_error(
                writer,
                404,
                "Model is not available on this Copilot endpoint; refresh Copilot models",
            )
            .await;
        }
        let token = match service.token(&mut state).await {
            Ok(token) => token,
            Err(_) => {
                return write_error(
                    writer,
                    401,
                    "Copilot authorization failed; refresh models or sign in again",
                )
                .await
            }
        };
        (
            token,
            state.account.as_ref().map(|a| a.access_token.clone()),
        )
    };
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let initiator = initiator(&body);
    let mut response = None;
    for attempt in 0..2 {
        let mut request = service.api.request(
            &token,
            reqwest::Method::POST,
            endpoint.path(),
            stream,
            initiator,
        )?;
        if endpoint == Endpoint::Messages {
            request = request.header("anthropic-version", "2023-06-01");
        }
        if has_image(&body) {
            request = request.header("Copilot-Vision-Request", "true");
        }
        let upstream = match request.json(&body).send().await {
            Ok(response) => response,
            Err(_) => return write_error(writer, 502, "Could not reach Copilot").await,
        };
        if upstream.status() == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
            let mut state = service.state.lock().await;
            if state.account.as_ref().map(|a| &a.access_token) != account_token.as_ref() {
                return write_error(writer, 401, "Copilot account changed during the request")
                    .await;
            }
            if state
                .token
                .as_ref()
                .is_some_and(|cached| cached.secret == token.secret)
            {
                state.token = None;
            }
            token = match service.token(&mut state).await {
                Ok(token) => token,
                Err(_) => {
                    return write_error(writer, 401, "Copilot authorization expired; sign in again")
                        .await
                }
            };
            continue;
        }
        response = Some(upstream);
        break;
    }
    let response = response.ok_or_else(|| "Copilot did not return a response".to_string())?;
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()))
        .map(str::to_string);
    if !response.status().is_success() {
        let message = format!("Copilot rejected the request (HTTP {status})");
        return write_error_with_retry(writer, status, &message, retry_after.as_deref()).await;
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let content_type = if content_type.starts_with("text/event-stream") {
        "text/event-stream"
    } else if content_type.starts_with("application/json") {
        "application/json"
    } else {
        return write_error(writer, 502, "Copilot returned an unsupported response type").await;
    };
    writer.write_all(format!("HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n").as_bytes())
        .await.map_err(|e| e.to_string())?;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "Copilot response stream was interrupted".to_string())?;
        if chunk.is_empty() {
            continue;
        }
        writer
            .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        writer.write_all(&chunk).await.map_err(|e| e.to_string())?;
        writer.write_all(b"\r\n").await.map_err(|e| e.to_string())?;
    }
    writer
        .write_all(b"0\r\n\r\n")
        .await
        .map_err(|e| e.to_string())
}

// The core's Codex executor injects this hosted tool even for third-party
// Responses API keys. It is not part of the Copilot subscription API. Preserve
// caller-defined functions and reject explicit hosted-image requests.
fn remove_codex_hosted_tool(body: &mut Value) -> Result<(), &'static str> {
    if body.pointer("/tool_choice/type").and_then(Value::as_str) == Some("image_generation") {
        return Err("Copilot does not support hosted image generation");
    }
    if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
        let injected = serde_json::json!({"type":"image_generation", "output_format":"png"});
        if tools.iter().any(|tool| {
            tool.get("type").and_then(Value::as_str) == Some("image_generation")
                && *tool != injected
        }) {
            return Err("Copilot does not support hosted image generation");
        }
        tools.retain(|tool| *tool != injected);
    }
    Ok(())
}

fn initiator(body: &Value) -> &'static str {
    let last = body
        .get("messages")
        .or_else(|| body.get("input"))
        .and_then(Value::as_array)
        .and_then(|items| items.last());
    if last.is_some_and(|last| {
        matches!(
            last.get("role").and_then(Value::as_str),
            Some("assistant" | "tool")
        ) || last.get("type").and_then(Value::as_str) == Some("function_call_output")
            || last
                .get("content")
                .and_then(Value::as_array)
                .is_some_and(|blocks| {
                    blocks.iter().any(|block| {
                        block.get("type").and_then(Value::as_str) == Some("tool_result")
                    })
                })
    }) {
        "agent"
    } else {
        "user"
    }
}

fn has_image(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            matches!(
                object.get("type").and_then(Value::as_str),
                Some("image" | "image_url" | "input_image")
            ) || object.values().any(has_image)
        }
        Value::Array(values) => values.iter().any(has_image),
        _ => false,
    }
}

async fn write_error(
    writer: &mut (impl AsyncWriteExt + Unpin),
    status: u16,
    message: &str,
) -> Result<(), String> {
    write_error_with_retry(writer, status, message, None).await
}

async fn write_error_with_retry(
    writer: &mut (impl AsyncWriteExt + Unpin),
    status: u16,
    message: &str,
    retry_after: Option<&str>,
) -> Result<(), String> {
    let body =
        serde_json::json!({"error": {"type": "copilot_error", "message": message}}).to_string();
    let retry = retry_after
        .map(|value| format!("Retry-After: {value}\r\n"))
        .unwrap_or_default();
    writer.write_all(format!("HTTP/1.1 {status} Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n{retry}\r\n{body}", body.len()).as_bytes())
        .await.map_err(|e| e.to_string())
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
