use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

const MAX_HEADERS: usize = 16 * 1024;
pub(super) const MAX_BODY: usize = 32 * 1024 * 1024;

// A private, single-request HTTP/1 reader for loopback peers: the managed core
// and Claude Code's MCP client. Every connection is closed after one reply, and
// ambiguous framing is rejected before a body is read.
pub(super) struct Request {
    pub(super) method: String,
    target: String,
    headers: Vec<(String, String)>,
    pub(super) body: Vec<u8>,
}

impl Request {
    pub(super) fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or_default()
    }

    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_str())
    }
}

pub(super) async fn read_request(socket: &mut TcpStream) -> Result<Request, (u16, &'static str)> {
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
    let (mut request, content_length) = parse_head(&bytes[..header_end])?;
    if bytes.len() - header_end > content_length {
        return Err((400, "Unexpected data after request"));
    }
    let received = bytes.len();
    bytes.resize(header_end + content_length, 0);
    socket
        .read_exact(&mut bytes[received..])
        .await
        .map_err(|_| (400, "Incomplete request body"))?;
    request.body = bytes.split_off(header_end);
    Ok(request)
}

fn parse_head(bytes: &[u8]) -> Result<(Request, usize), (u16, &'static str)> {
    let text = std::str::from_utf8(bytes).map_err(|_| (400, "Invalid request headers"))?;
    let mut lines = text.split("\r\n");
    let mut start = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target), Some("HTTP/1.1"), None) =
        (start.next(), start.next(), start.next(), start.next())
    else {
        return Err((400, "Only HTTP/1.1 requests are supported"));
    };
    let mut headers = Vec::new();
    let mut length = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or((400, "Malformed request header"))?;
        if name.is_empty()
            || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
            || value.bytes().any(|c| c < 32 && c != b'\t')
        {
            return Err((400, "Invalid request header"));
        }
        let name = name.to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "transfer-encoding" | "expect" => return Err((400, "Unsupported request framing")),
            "content-length" if length.replace(value).is_some() => {
                return Err((400, "Duplicate request header"))
            }
            _ => {}
        }
        headers.push((name, value.to_string()));
    }
    let content_length = match length {
        None => 0,
        Some(length) if length.is_empty() || !length.bytes().all(|c| c.is_ascii_digit()) => {
            return Err((400, "Invalid Content-Length"))
        }
        Some(length) => length
            .parse::<usize>()
            .ok()
            .filter(|length| *length <= MAX_BODY)
            .ok_or((413, "Request body is too large"))?,
    };
    let request = Request {
        method: method.to_string(),
        target: target.to_string(),
        headers,
        body: Vec::new(),
    };
    Ok((request, content_length))
}

pub(super) fn same_key(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .fold(0, |difference, (a, b)| difference | (a ^ b))
            == 0
}

fn extra_headers(headers: &[(&str, &str)]) -> String {
    headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect()
}

pub(super) async fn write_response(
    writer: &mut (impl AsyncWriteExt + Unpin),
    status: u16,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<(), String> {
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n{}\r\n",
        if status < 400 { "OK" } else { "Error" },
        body.len(),
        extra_headers(headers)
    );
    writer
        .write_all(head.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    writer.write_all(body).await.map_err(|e| e.to_string())
}

pub(super) async fn write_json(
    writer: &mut (impl AsyncWriteExt + Unpin),
    status: u16,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<(), String> {
    let mut all = vec![("Content-Type", "application/json")];
    all.extend_from_slice(headers);
    write_response(writer, status, &all, body).await
}

pub(super) async fn start_event_stream(
    writer: &mut (impl AsyncWriteExt + Unpin),
    headers: &[(&str, &str)],
) -> Result<(), String> {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\nCache-Control: no-store\r\n{}\r\n",
        extra_headers(headers)
    );
    writer
        .write_all(head.as_bytes())
        .await
        .map_err(|e| e.to_string())
}

pub(super) async fn write_chunk(
    writer: &mut (impl AsyncWriteExt + Unpin),
    chunk: &[u8],
) -> Result<(), String> {
    let mut framed = format!("{:x}\r\n", chunk.len()).into_bytes();
    framed.extend_from_slice(chunk);
    framed.extend_from_slice(b"\r\n");
    writer.write_all(&framed).await.map_err(|e| e.to_string())?;
    writer.flush().await.map_err(|e| e.to_string())
}

pub(super) async fn end_stream(writer: &mut (impl AsyncWriteExt + Unpin)) -> Result<(), String> {
    writer
        .write_all(b"0\r\n\r\n")
        .await
        .map_err(|e| e.to_string())
}
