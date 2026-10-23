//! End-to-end check of the raw HTTP + SSE path against a local mock server.

use futures::StreamExt;
use provider_openai::OpenAiCompatible;
use relay_core::{ChatMessage, ChatRequest, ProviderAdapter, ProviderConfig, StreamEvent};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn serve(listener: TcpListener) {
    loop {
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        tokio::spawn(async move {
            let mut buf = vec![0u8; 16384];
            let mut total = 0;
            loop {
                let n = sock.read(&mut buf[total..]).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                total += n;
                let text = String::from_utf8_lossy(&buf[..total]).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if total >= end + 4 + len {
                        break;
                    }
                }
            }
            let req = String::from_utf8_lossy(&buf[..total]).to_string();
            let authed = req.to_lowercase().contains("authorization: bearer sk-test");
            let (status, ctype, body) = if !authed {
                (
                    "401 Unauthorized",
                    "application/json",
                    r#"{"error":{"message":"bad key sk-test-leak"}}"#.to_string(),
                )
            } else if req.starts_with("GET /v1/models") {
                (
                    "200 OK",
                    "application/json",
                    r#"{"data":[{"id":"alpha"},{"id":"beta"}]}"#.to_string(),
                )
            } else if req.contains("\"stream_options\"") {
                // Mimics servers that predate `stream_options`.
                (
                    "400 Bad Request",
                    "application/json",
                    r#"{"error":{"message":"Unrecognized request argument: stream_options"}}"#
                        .to_string(),
                )
            } else if req.contains("\"max_tokens\":1") && req.contains("\"model\":\"newmodel\"") {
                ("400 Bad Request", "application/json", r#"{"error":{"message":"Unsupported parameter: 'max_tokens'. Use 'max_completion_tokens'."}}"#.to_string())
            } else if req.contains("\"stream\":true") {
                let sse = "data: {\"id\":\"1\",\"model\":\"alpha\",\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n\
: heartbeat\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n\
data: {\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":2},\"choices\":[]}\n\n\
data: [DONE]\n\n";
                ("200 OK", "text/event-stream", sse.to_string())
            } else {
                ("200 OK", "application/json", r#"{"id":"1","model":"alpha","choices":[{"finish_reason":"stop","message":{"content":"pong"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#.to_string())
            };
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        });
    }
}

async fn adapter(key: Option<&str>) -> OpenAiCompatible {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(serve(listener));
    let config = ProviderConfig::openai_compatible("mock", format!("http://127.0.0.1:{port}/v1"));
    OpenAiCompatible::new(config, key.map(str::to_string)).unwrap()
}

#[tokio::test]
async fn discovers_models_tests_connection_and_streams() {
    let p = adapter(Some("sk-test-123")).await;

    let models = p.discover_models().await.unwrap();
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["alpha", "beta"]
    );

    let test = p.test_connection("alpha").await.unwrap();
    assert!(test.ok && test.status == 200);

    let req = ChatRequest {
        model: "alpha".into(),
        messages: vec![ChatMessage::user("hi")],
        ..Default::default()
    };
    assert_eq!(p.chat(req.clone()).await.unwrap().content.len(), 1);

    let events: Vec<_> = p.stream(req).await.unwrap().collect().await;
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            Ok(StreamEvent::TextDelta { text }) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello");
    assert!(events
        .iter()
        .any(|e| matches!(e, Ok(StreamEvent::Usage { .. }))));
    assert!(events.iter().all(|e| e.is_ok()));
}

#[tokio::test]
async fn retries_without_stream_options_when_the_server_rejects_it() {
    let p = adapter(Some("sk-test-123")).await;
    let req = ChatRequest {
        model: "alpha".into(),
        messages: vec![ChatMessage::user("hi")],
        ..Default::default()
    };
    let events: Vec<_> = p.stream(req).await.unwrap().collect().await;
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            Ok(StreamEvent::TextDelta { text }) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello");
    // Without stream_options the server sends no usage; that must not fail.
    assert!(events.iter().all(|e| e.is_ok()));
}

#[tokio::test]
async fn bad_key_reports_failure_and_redacts_secrets() {
    let p = adapter(Some("wrong")).await;
    let test = p.test_connection("alpha").await.unwrap();
    assert!(!test.ok);
    assert_eq!(test.status, 401);
    assert!(
        !test.detail.contains("sk-test-leak"),
        "detail leaked: {}",
        test.detail
    );
}
