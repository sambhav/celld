//! Standalone prototype: public Python functions, named calls and private host I/O.
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use celld_monty_experiment::exports::Module;
use monty::RunProgress;
use monty_types::{MontyObject as Obj, PrintWriter};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone)]
struct App {
    module: Arc<Mutex<Module>>,
    upstream: String,
    client: reqwest::Client,
}
type Error = (StatusCode, String);
fn failure(e: impl ToString) -> Error {
    (StatusCode::UNPROCESSABLE_ENTITY, e.to_string())
}
async fn invoke(
    State(app): State<App>,
    Path(name): Path<String>,
    Json(args): Json<Value>,
) -> Result<Json<Value>, Error> {
    let function = app
        .module
        .lock()
        .unwrap()
        .get(&name)
        .cloned()
        .ok_or((StatusCode::NOT_FOUND, "unknown function".into()))?;
    let progress = function.start(&args).map_err(failure)?;
    let value = tokio::time::timeout(Duration::from_secs(5), run(&app, progress))
        .await
        .map_err(|_| (StatusCode::GATEWAY_TIMEOUT, "call timed out".into()))??;
    Ok(Json(json!({"result":value})))
}
async fn run(app: &App, mut progress: RunProgress) -> Result<Value, Error> {
    let mut calls = 0;
    let mut pending = tokio::task::JoinSet::new();
    loop {
        progress = match progress {
            RunProgress::Complete(Obj::String(s)) => {
                return serde_json::from_str(&s).map_err(failure);
            }
            RunProgress::FunctionCall(call) => {
                calls += 1;
                if calls > 64
                    || call.function_name != "_fetch_json"
                    || call.args.len() != 1
                    || !call.kwargs.is_empty()
                    || app.upstream.is_empty()
                {
                    return Err(failure("unsupported host call"));
                }
                let Obj::String(name) = &call.args[0] else {
                    return Err(failure("_fetch_json expects a name string"));
                };
                let body = json!({"name":name}).to_string();
                let id = call.call_id;
                let client = app.client.clone();
                let url = app.upstream.clone();
                pending.spawn(async move {
                    let response = client
                        .post(url)
                        .header("content-type", "application/json")
                        .body(body)
                        .send()
                        .await
                        .map_err(failure)?
                        .error_for_status()
                        .map_err(failure)?
                        .text()
                        .await
                        .map_err(failure)?;
                    Ok::<_, Error>((id, Obj::String(response).into()))
                });
                call.resume_pending(PrintWriter::Disabled)
                    .map_err(failure)?
            }
            RunProgress::ResolveFutures(waiting) => {
                let result = pending
                    .join_next()
                    .await
                    .ok_or_else(|| failure("missing host future"))?
                    .map_err(failure)??;
                waiting
                    .resume(vec![result], PrintWriter::Disabled)
                    .map_err(failure)?
            }
            _ => return Err(failure("unsupported interpreter suspension")),
        };
    }
}
#[tokio::main]
async fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "usage: monty-functions inspect|client|serve app.py [port=8000] [fixed-upstream-url]"
        );
        std::process::exit(2)
    }
    let source = std::fs::read_to_string(&args[2]).unwrap();
    let module = Module::compile(&source).unwrap();
    if args[1] == "inspect" {
        println!(
            "{}",
            serde_json::to_string_pretty(&module.manifest()).unwrap()
        );
        return;
    }
    if args[1] == "client" {
        print!("{}", module.python_client());
        return;
    }
    assert_eq!(args[1], "serve", "unknown command");
    let app = App {
        module: Arc::new(Mutex::new(module)),
        upstream: args.get(4).cloned().unwrap_or_default(),
        client: reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap(),
    };
    let port = args.get(3).map(String::as_str).unwrap_or("8000");
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    let router = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/call/{name}", post(invoke))
        .with_state(app);
    axum::serve(listener, router).await.unwrap();
}
