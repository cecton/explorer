//! A scripted OpenAI-compatible server for tests that run the own agent loop end to end:
//! each request it receives is answered with the next reply of the script, streamed as
//! server-sent events, and recorded for the test to inspect.

use std::sync::Arc;

/// One piece of a streamed answer.
pub(crate) enum Part {
    Thinking(String),
    Text(String),
    Call {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    Usage {
        prompt: u64,
        cached: Option<u64>,
    },
}

/// The answer to one request.
pub(crate) struct Reply {
    parts: Vec<Part>,
    /// Keep the stream open after the parts, as a model that is still thinking.
    hang: bool,
    /// Runs when the request arrives, before anything is sent.
    on_request: Option<Box<dyn FnOnce() + Send>>,
}

impl Reply {
    pub(crate) fn new() -> Self {
        Self {
            parts: Vec::new(),
            hang: false,
            on_request: None,
        }
    }

    pub(crate) fn thinking(mut self, text: &str) -> Self {
        self.parts.push(Part::Thinking(text.to_string()));
        self
    }

    pub(crate) fn text(mut self, text: &str) -> Self {
        self.parts.push(Part::Text(text.to_string()));
        self
    }

    pub(crate) fn call(mut self, id: &str, name: &str, arguments: serde_json::Value) -> Self {
        self.parts.push(Part::Call {
            id: id.to_string(),
            name: name.to_string(),
            arguments,
        });
        self
    }

    pub(crate) fn usage(mut self, prompt: u64, cached: Option<u64>) -> Self {
        self.parts.push(Part::Usage { prompt, cached });
        self
    }

    /// The stream never ends once its parts are sent.
    pub(crate) fn hang(mut self) -> Self {
        self.hang = true;
        self
    }

    /// `action` runs when the request arrives, before the answer streams.
    pub(crate) fn on_request(mut self, action: impl FnOnce() + Send + 'static) -> Self {
        self.on_request = Some(Box::new(action));
        self
    }
}

pub(crate) struct FakeLlm {
    /// Base URL to give an endpoint.
    pub(crate) url: String,
    requests: Arc<parking_lot::Mutex<Vec<serde_json::Value>>>,
}

impl FakeLlm {
    /// Serves `script` in order: the n-th request gets the n-th reply, and extras are refused.
    pub(crate) async fn start(script: Vec<Reply>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let script = Arc::new(parking_lot::Mutex::new(std::collections::VecDeque::from(
            script,
        )));
        let seen = Arc::clone(&requests);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let seen = Arc::clone(&seen);
                let script = Arc::clone(&script);
                tokio::spawn(async move {
                    let _ = serve(stream, seen, script).await;
                });
            }
        });
        Self { url, requests }
    }

    /// The JSON bodies received so far, oldest first.
    pub(crate) fn requests(&self) -> Vec<serde_json::Value> {
        self.requests.lock().clone()
    }
}

async fn serve(
    mut stream: tokio::net::TcpStream,
    seen: Arc<parking_lot::Mutex<Vec<serde_json::Value>>>,
    script: Arc<parking_lot::Mutex<std::collections::VecDeque<Reply>>>,
) -> std::io::Result<()> {
    use serde_json::{Value, json};
    use tokio::io::AsyncWriteExt;
    let body = read_body(&mut stream).await?;
    seen.lock()
        .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
    let reply = script.lock().pop_front();
    let Some(mut reply) = reply else {
        let message = r#"{"error":{"message":"the script has no reply left"}}"#;
        let response = format!(
            "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{message}",
            message.len()
        );
        return stream.write_all(response.as_bytes()).await;
    };
    if let Some(action) = reply.on_request.take() {
        action();
    }
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
        )
        .await?;
    let mut calls = 0;
    for part in &reply.parts {
        let chunk = match part {
            Part::Thinking(text) => {
                json!({"choices": [{"index": 0, "delta": {"reasoning_content": text}}]})
            }
            Part::Text(text) => json!({"choices": [{"index": 0, "delta": {"content": text}}]}),
            Part::Call {
                id,
                name,
                arguments,
            } => {
                calls += 1;
                json!({"choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": calls - 1,
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments.to_string()},
                }]}}]})
            }
            Part::Usage { prompt, cached } => {
                let mut usage = json!({
                    "prompt_tokens": prompt,
                    "completion_tokens": 1,
                    "total_tokens": prompt + 1,
                });
                if let Some(cached) = cached {
                    usage["prompt_tokens_details"] = json!({"cached_tokens": cached});
                }
                json!({"choices": [], "usage": usage})
            }
        };
        stream
            .write_all(format!("data: {chunk}\n\n").as_bytes())
            .await?;
        stream.flush().await?;
    }
    if reply.hang {
        std::future::pending::<()>().await;
    }
    let finish = json!({"choices": [{"index": 0, "delta": {}, "finish_reason": if calls > 0 { "tool_calls" } else { "stop" }}]});
    stream
        .write_all(format!("data: {finish}\n\ndata: [DONE]\n\n").as_bytes())
        .await
}

/// Reads one HTTP request and returns its body.
async fn read_body(stream: &mut tokio::net::TcpStream) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buffer.extend_from_slice(&chunk[..read]);
        let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&buffer[..end]).to_lowercase();
        let length = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let start = end + 4;
        while buffer.len() < start + length {
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        return Ok(buffer[start..start + length].to_vec());
    }
}
