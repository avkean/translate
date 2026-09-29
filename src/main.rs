//! Serves the translator page and passes translations to an OpenAI-compatible
//! chat API. In production that's the Privatemode proxy running beside it.

use std::{
    collections::HashMap, env, fmt::Write, io, net::SocketAddr, sync::Arc, sync::Mutex,
    time::Duration,
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{Stream, StreamExt, stream};
use serde::Deserialize;
use serde_json::{Value, json};

const PAGE: &str = include_str!("../static/index.html");
const CSS: &str = include_str!("../static/app.css");
const JS: &str = include_str!("../static/app.js");
// Versioned file name, so browsers can cache it for good.
const FONT: &[u8] = include_bytes!("../static/fonts/nunito-sans-5.3.0.woff2");

const MAX_CHARS: usize = 5000;
const MAX_BODY: usize = 64 * 1024;
const RATE_PER_IP: u32 = 30;
// Caps total use per minute, so many IPs together can't drain the API quota.
const RATE_TOTAL: u32 = 600;
const MAX_TRACKED_IPS: usize = 20_000;

const SYSTEM_PROMPT: &str = "You are a translation engine. Output only the translated text. No quotes, prefixes, or commentary.";

// Must match the values in static/app.js. Anything else is rejected, so the
// language fields can't be used to slip other instructions into the prompt.
const LANGUAGES: &[&str] = &[
    "English",
    "Spanish",
    "French",
    "German",
    "Italian",
    "Portuguese",
    "Russian",
    "Chinese Simplified",
    "Chinese Traditional",
    "Japanese",
    "Korean",
    "Arabic",
    "Hindi",
    "Bengali",
    "Dutch",
    "Swedish",
    "Norwegian",
    "Danish",
    "Finnish",
    "Polish",
    "Czech",
    "Turkish",
    "Greek",
    "Hebrew",
    "Romanian",
    "Hungarian",
    "Ukrainian",
    "Thai",
    "Vietnamese",
    "Indonesian",
    "Malay",
    "Catalan",
    "Swahili",
];

const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
    font-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

struct App {
    http: reqwest::Client,
    upstream: String,
    api_key: Option<String>,
    model: String,
    hits: Mutex<Hits>,
}

// Request counts for the current minute. Memory only, wiped every minute.
#[derive(Default)]
struct Hits {
    per_ip: HashMap<String, u32>,
    total: u32,
}

#[derive(Deserialize)]
struct TranslateRequest {
    text: String,
    from: String,
    to: String,
}

#[tokio::main]
async fn main() {
    let port = env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);
    let app = Arc::new(App {
        http: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("building HTTP client"),
        upstream: env::var("UPSTREAM_URL")
            .unwrap_or_else(|_| "http://privatemode-proxy:8080/v1/chat/completions".into()),
        api_key: env::var("UPSTREAM_API_KEY").ok().filter(|k| !k.is_empty()),
        model: env::var("MODEL").unwrap_or_else(|_| "gpt-oss-120b".into()),
        hits: Mutex::default(),
    });

    let wiper = app.clone();
    tokio::spawn(async move {
        let mut every_minute = tokio::time::interval(Duration::from_secs(60));
        loop {
            every_minute.tick().await;
            *wiper.hits.lock().unwrap() = Hits::default();
        }
    });

    let router = Router::new()
        .route("/", get(|| async { Html(PAGE) }))
        .route(
            "/app.css",
            get(|| async { asset("text/css; charset=utf-8", CSS) }),
        )
        .route(
            "/app.js",
            get(|| async { asset("text/javascript; charset=utf-8", JS) }),
        )
        .route("/fonts/nunito-sans-5.3.0.woff2", get(font))
        .route("/translate", post(translate))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::map_response(security_headers))
        .with_state(app);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("binding port");
    println!("translate listening on {addr}");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("running server");
}

fn asset(content_type: &'static str, body: &'static str) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
}

async fn font() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "font/woff2"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        FONT,
    )
}

async fn security_headers(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static("same-origin"),
    );
    h.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=(), interest-cohort=()"),
    );
    res
}

async fn translate(State(app): State<Arc<App>>, headers: HeaderMap, body: Bytes) -> Response {
    if !app.allow(client_ip(&headers)) {
        return error(StatusCode::TOO_MANY_REQUESTS, "Too many requests");
    }
    let Ok(req) = serde_json::from_slice::<TranslateRequest>(&body) else {
        return error(StatusCode::BAD_REQUEST, "Bad request");
    };
    let known = |lang: &str| LANGUAGES.contains(&lang);
    if !known(&req.to) || !(req.from == "auto" || known(&req.from)) {
        return error(StatusCode::BAD_REQUEST, "Unknown language");
    }
    if req.text.trim().is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }
    // Count length the way the browser does, so it matches the page's maxlength.
    if req.text.encode_utf16().count() > MAX_CHARS {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "Text too long");
    }

    let prompt = if req.from == "auto" {
        format!("Translate to {}:\n{}", req.to, req.text)
    } else {
        format!("Translate from {} to {}:\n{}", req.from, req.to, req.text)
    };
    let payload = json!({
        "model": app.model,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": prompt },
        ],
        "stream": true,
        "temperature": 0.1,
        "max_tokens": 4096,
        "reasoning_effort": "low",
    });

    let mut call = app
        .http
        .post(&app.upstream)
        .json(&payload)
        .timeout(Duration::from_secs(60));
    if let Some(key) = &app.api_key {
        call = call.bearer_auth(key);
    }
    let upstream = match call.send().await {
        Ok(res) if res.status().is_success() => res,
        Ok(res) => {
            eprintln!("upstream returned {}", res.status());
            return match res.status() {
                StatusCode::TOO_MANY_REQUESTS => {
                    error(StatusCode::TOO_MANY_REQUESTS, "Too many requests")
                }
                _ => error(StatusCode::BAD_GATEWAY, "Translation service error"),
            };
        }
        Err(e) => {
            eprintln!("upstream unreachable: {e}");
            return error(StatusCode::BAD_GATEWAY, "Translation service error");
        }
    };

    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(text_only(upstream)))
        .unwrap()
}

impl App {
    fn allow(&self, ip: String) -> bool {
        let mut hits = self.hits.lock().unwrap();
        if hits.total >= RATE_TOTAL {
            return false;
        }
        if !hits.per_ip.contains_key(&ip) && hits.per_ip.len() >= MAX_TRACKED_IPS {
            return false;
        }
        hits.total += 1;
        let count = hits.per_ip.entry(ip).or_insert(0);
        *count += 1;
        *count <= RATE_PER_IP
    }
}

// Bunny sets X-Real-IP, and the origin only accepts requests coming through Bunny.
fn client_ip(headers: &HeaderMap) -> String {
    let header = |name| headers.get(name).and_then(|v| v.to_str().ok());
    header("x-real-ip")
        .or_else(|| header("x-forwarded-for").and_then(|v| v.split(',').next()))
        .map_or_else(|| "unknown".into(), |ip| ip.trim().to_owned())
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

// Re-emits the upstream event stream with only the translated text, dropping
// the model's reasoning and everything else. Keeps the `data: {...}` shape the
// page reads.
fn text_only(upstream: reqwest::Response) -> impl Stream<Item = io::Result<Bytes>> {
    let mut pending = Vec::new();
    upstream
        .bytes_stream()
        .map(move |chunk| {
            let chunk = chunk.map_err(|e| {
                eprintln!("upstream stream failed: {e}");
                io::Error::other(e)
            })?;
            pending.extend_from_slice(&chunk);
            let mut out = String::new();
            while let Some(end) = pending.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = pending.drain(..=end).collect();
                let line = String::from_utf8_lossy(&line);
                let Some(data) = line.trim_end().strip_prefix("data: ") else {
                    continue;
                };
                let Ok(event) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                if let Some(text) = event["choices"][0]["delta"]["content"]
                    .as_str()
                    .filter(|t| !t.is_empty())
                {
                    let delta = json!({ "choices": [{ "delta": { "content": text } }] });
                    let _ = write!(out, "data: {delta}\n\n");
                }
            }
            Ok(Bytes::from(out))
        })
        .chain(stream::once(async {
            Ok(Bytes::from_static(b"data: [DONE]\n\n"))
        }))
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("listening for SIGTERM");
        tokio::select! {
            _ = ctrl_c => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    ctrl_c.await.ok();
}
