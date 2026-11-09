use super::{engine, http, ClaudeCli, BRIDGE_PATH};
use serde_json::{json, Value};
use std::{net::TcpListener, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::Semaphore,
};

// A tool-using reply can wait on the client for a long time between events.
const TURN_TIMEOUT: Duration = Duration::from_secs(1800);

pub(super) async fn serve(listener: TcpListener, service: Arc<ClaudeCli>) -> Result<(), String> {
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
            let request =
                match tokio::time::timeout(Duration::from_secs(30), read(&mut socket, &service))
                    .await
                {
                    Ok(Ok(request)) => request,
                    Ok(Err((status, message))) => {
                        return report(&mut socket, status, message).await
                    }
                    Err(_) => {
                        return report(&mut socket, 408, "Claude CLI request timed out").await
                    }
                };
            let (mut reader, mut writer) = socket.split();
            let mut disconnect = [0u8; 1];
            // Dropping the turn ends its Claude Code process when the core or
            // client disconnects, so an abandoned reply stops generating.
            tokio::select! {
                result = tokio::time::timeout(TURN_TIMEOUT, engine::respond(&service.engine, &request, &mut writer)) => {
                    if matches!(result, Ok(Ok(()))) {
                        return;
                    }
                    // Close without a final chunk: never turn a broken stream into success.
                    if let Err(error) = writer.shutdown().await {
                        eprintln!("Could not close a Claude CLI stream: {error}");
                    }
                }
                _ = reader.read(&mut disconnect) => {}
            }
        });
    }
}

async fn read(socket: &mut TcpStream, service: &ClaudeCli) -> Result<Value, (u16, &'static str)> {
    let request = http::read_request(socket).await?;
    if request.method != "POST" {
        return Err((405, "Only POST requests are supported"));
    }
    if request.path().strip_prefix(BRIDGE_PATH) != Some("/v1/messages") {
        return Err((404, "Unsupported Claude CLI endpoint"));
    }
    let supplied = request
        .header("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .or_else(|| request.header("x-api-key"))
        .unwrap_or_default();
    if !http::same_key(supplied, &service.key) {
        return Err((401, "Invalid Claude CLI adapter key"));
    }
    let body: Value =
        serde_json::from_slice(&request.body).map_err(|_| (400, "Invalid request JSON"))?;
    if !body.is_object() {
        return Err((400, "Request JSON must be an object"));
    }
    let model = body.get("model").and_then(Value::as_str);
    match service.account().as_ref() {
        None => return Err((401, "Claude CLI is not connected in LlamaProxy")),
        Some(account) if !model.is_some_and(|model| account.enabled_models().contains(&model)) => {
            return Err((404, "Model is not available through Claude CLI"))
        }
        Some(_) => {}
    }
    Ok(body)
}

async fn report(socket: &mut TcpStream, status: u16, message: &str) {
    let kind = match status {
        401 => "authentication_error",
        404 => "not_found_error",
        _ => "invalid_request_error",
    };
    let body = json!({"type": "error", "error": {"type": kind, "message": message}}).to_string();
    if let Err(error) = http::write_json(socket, status, &[], body.as_bytes()).await {
        eprintln!("Could not send a Claude CLI adapter error: {error}");
    }
}
