//! Controlled transport fixture for native UI acceptance. This is synthetic
//! prose, not a model or a claim about inference quality. Never use with private
//! manuscripts: the fixture prints received request bodies for test evidence.
#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicU64, Ordering};

use axum::{Json, Router, http::StatusCode, routing::post};
use serde_json::{Value, json};

static REQUESTS: AtomicU64 = AtomicU64::new(0);

async fn completion(Json(request): Json<Value>) -> Json<Value> {
    let index = REQUESTS.fetch_add(1, Ordering::Relaxed);
    println!(
        "{}",
        json!({"kind": "synthetic_request", "index": index, "request": request})
    );
    let endings = [
        "the garden grew quiet in the evening light.",
        "a narrow path led toward the old stone wall.",
        "the wind moved softly through the open door.",
        "someone had left a notebook on the table.",
    ];
    Json(json!({
        "id": format!("synthetic-{index}"),
        "model": "loom-acceptance-fixture",
        "choices": [{"index": 0, "text": endings[usize::try_from(index % 4).unwrap_or(0)], "finish_reason": "stop"}],
        // Usage omitted deliberately: an HTTP fixture cannot report model tokens.
    }))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    println!(
        "{}",
        json!({"kind": "synthetic_server", "address": listener.local_addr()?.to_string()})
    );
    let app = Router::new()
        .route("/v1/completions", post(completion))
        .route(
            "/unavailable",
            post(|| async {
                println!("{}", json!({"kind": "synthetic_unavailable"}));
                StatusCode::SERVICE_UNAVAILABLE
            }),
        );
    axum::serve(listener, app).await?;
    Ok(())
}
