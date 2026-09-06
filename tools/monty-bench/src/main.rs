//! Fixed-code feasibility experiment, NOT a production untrusted-code host.
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use monty::{Dump, MontyRepl, MontyRun, RunProgress, Session, SessionRef, dump};
use monty_types::{
    CompileOptions, MontyObject as Obj, PrintWriter, ResourceLimits, ResourceTracker,
};
use serde_json::{Value, json};
use std::{
    hint::black_box,
    time::{Duration, Instant},
};

thread_local! { static RUNNER: MontyRun = program(); }

const CODE: &str = include_str!("../worker.py");
const REV: &str = "af272c3116e2525249103f960b79086fd250bcef";
fn tracker() -> ResourceTracker {
    ResourceTracker::new(ResourceLimits {
        max_duration: Some(Duration::from_secs(2)),
        ..Default::default()
    })
}
fn compile(code: &str, inputs: &[&str]) -> MontyRun {
    MontyRun::new(
        code.to_owned(),
        "worker.py",
        inputs.iter().map(|s| s.to_string()).collect(),
        CompileOptions::default(),
    )
    .unwrap()
}
fn program() -> MontyRun {
    compile(
        &format!("{CODE}\nhandle(raw, io)"),
        &["raw", "io", "get_price"],
    )
}
fn inputs(raw: &str, io: bool) -> Vec<Obj> {
    vec![
        Obj::String(raw.to_owned()),
        Obj::Bool(io),
        Obj::Function {
            name: "get_price".into(),
            docstring: None,
        },
    ]
}
fn string(value: Obj) -> Result<String, String> {
    if let Obj::String(s) = value {
        Ok(s)
    } else {
        Err(format!("expected string, got {value:?}"))
    }
}
#[derive(Clone)]
struct App {
    upstream: String,
    client: reqwest::Client,
    native: bool,
}
async fn execute(app: &App, raw: &str) -> Result<String, String> {
    if app.native {
        return native(app, raw).await;
    }
    let mut progress = RUNNER
        .with(Clone::clone)
        .start(
            inputs(raw, !app.upstream.is_empty()),
            tracker(),
            PrintWriter::Disabled,
        )
        .map_err(|e| e.to_string())?;
    // This fixed fixture has at most one external call. No implicit filesystem/network access.
    let mut calls = 0;
    loop {
        progress = match progress {
            RunProgress::Complete(value) => return string(value),
            RunProgress::FunctionCall(call) => {
                calls += 1;
                if calls > 1
                    || call.function_name != "get_price"
                    || call.args.len() != 1
                    || !call.kwargs.is_empty()
                {
                    return Err("unexpected host call".into());
                }
                let body = string(call.args[0].clone())?;
                let value = upstream(app, body).await?;
                call.resume(Obj::String(value), PrintWriter::Disabled)
                    .map_err(|e| e.to_string())?
            }
            other => return Err(format!("unexpected suspension {other:?}")),
        };
    }
}
async fn upstream(app: &App, body: String) -> Result<String, String> {
    app.client
        .post(&app.upstream)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())
}
async fn native(app: &App, raw: &str) -> Result<String, String> {
    let args: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| (1..=128).contains(&s.chars().count()))
        .ok_or("invalid name")?;
    if app.upstream.is_empty() {
        return Ok(json!({"result":format!("Hello, {name}")}).to_string());
    }
    let text = upstream(app, json!({"name":name}).to_string()).await?;
    let price: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if price["customer"] != name
        || !price["unit_price_cents"].is_i64()
        || price["stock"].as_i64().is_none_or(|n| n < 2)
        || !price["currency"].is_string()
    {
        return Err("invalid price".into());
    }
    Ok(json!({"result":{"customer":name,"total_cents":price["unit_price_cents"].as_i64().unwrap()*2,"currency":price["currency"],"trace":name}}).to_string())
}
async fn handler(
    State(app): State<App>,
    body: Bytes,
) -> Result<([(&'static str, &'static str); 1], String), (StatusCode, String)> {
    let raw = std::str::from_utf8(&body).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    execute(&app, raw)
        .await
        .map(|s| ([("content-type", "application/json")], s))
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))
}
fn ns(mut f: impl FnMut(), count: usize) -> f64 {
    for _ in 0..100 {
        f();
    }
    let start = Instant::now();
    for _ in 0..count {
        f();
    }
    start.elapsed().as_nanos() as f64 / count as f64
}
fn micro() {
    let raw = "{\"name\":\"benchmark\"}";
    let start = Instant::now();
    let runner = program();
    let compile_us = start.elapsed().as_secs_f64() * 1e6;
    let start = Instant::now();
    let value = runner
        .run(inputs(raw, false), tracker(), PrintWriter::Disabled)
        .unwrap();
    let first_us = start.elapsed().as_secs_f64() * 1e6;
    assert_eq!(
        serde_json::from_str::<Value>(&string(value).unwrap()).unwrap(),
        json!({"result":"Hello, benchmark"})
    );
    let compile_ns = ns(
        || {
            black_box(program());
        },
        1000,
    );
    let run_ns = ns(
        || {
            black_box(
                runner
                    .run(inputs(raw, false), tracker(), PrintWriter::Disabled)
                    .unwrap(),
            );
        },
        10000,
    );
    let start_ns = ns(
        || {
            black_box(
                runner
                    .clone()
                    .start(inputs(raw, false), tracker(), PrintWriter::Disabled)
                    .unwrap(),
            );
        },
        10000,
    );
    let mut repl = MontyRepl::new(
        "worker.py",
        ResourceTracker::default(),
        CompileOptions::default(),
    );
    repl.feed_run(
        &format!("{CODE}\nhandle(\"{{\\\"name\\\":\\\"warm\\\"}}\", False)"),
        vec![],
        PrintWriter::Disabled,
    )
    .unwrap();
    let snapshot = dump("worker.py", None, SessionRef::Idle(&repl)).unwrap();
    // Probe the warm API independently: current upstream may reject this fixture.
    let warm_probe = repl.call_function(
        "handle",
        vec![Obj::String(raw.into()), Obj::Bool(false)],
        PrintWriter::Disabled,
    );
    let (repl_ns, repl_error) = match warm_probe {
        Ok(_) => (
            Some(ns(
                || {
                    black_box(
                        repl.call_function(
                            "handle",
                            vec![Obj::String(raw.into()), Obj::Bool(false)],
                            PrintWriter::Disabled,
                        )
                        .unwrap(),
                    );
                },
                1000,
            )),
            None,
        ),
        Err(e) => (None, Some(e.to_string())),
    };
    let restore_ns = ns(
        || {
            black_box(Dump::load(&snapshot).unwrap());
        },
        1000,
    );
    let mut state = MontyRepl::new(
        "state.py",
        ResourceTracker::default(),
        CompileOptions::default(),
    );
    state
        .feed_run("counter = 40", vec![], PrintWriter::Disabled)
        .unwrap();
    let bytes = dump("state.py", None, SessionRef::Idle(&state)).unwrap();
    let Session::Idle(mut restored) = Dump::load(&bytes).unwrap().state else {
        panic!()
    };
    assert_eq!(
        restored
            .feed_run("counter += 2\ncounter", vec![], PrintWriter::Disabled)
            .unwrap(),
        Obj::Int(42)
    );
    let cases = [
        (
            "decorator_annotations",
            "def deco(f):\n    return f\n@deco\ndef f(x: int) -> int:\n    return x+1\nf(1)",
        ),
        (
            "dataclass",
            "from dataclasses import dataclass\n@dataclass\nclass Input:\n    name: str\nInput('ok').name",
        ),
        ("pydantic", "from pydantic import BaseModel"),
        ("numpy", "import numpy"),
        ("inspect", "import inspect"),
        ("function_metadata", "def f(): pass\nf.__name__"),
        ("asyncio_sleep", "import asyncio\nasyncio.sleep(0)"),
    ];
    let probes: Vec<_> = cases
        .iter()
        .map(|(name, code)| {
            let result = MontyRun::new(
                code.to_string(),
                "probe.py",
                vec![],
                CompileOptions::default(),
            )
            .and_then(|p| p.run(vec![], tracker(), PrintWriter::Disabled));
            match result {
                Ok(value) => json!({"feature":name,"ok":true,"result":format!("{value:?}")}),
                Err(e) => json!({"feature":name,"ok":false,"error":e.to_string()}),
            }
        })
        .collect();
    // Exercise true async suspension and correlate its pending future.
    let async_run = compile(
        "async def f():\n    return await get_price('body')\nawait f()",
        &["get_price"],
    );
    let progress = async_run
        .start(
            vec![Obj::Function {
                name: "get_price".into(),
                docstring: None,
            }],
            tracker(),
            PrintWriter::Disabled,
        )
        .unwrap();
    let RunProgress::FunctionCall(call) = progress else {
        panic!("missing async call")
    };
    let id = call.call_id;
    let RunProgress::ResolveFutures(futures) = call.resume_pending(PrintWriter::Disabled).unwrap()
    else {
        panic!("missing pending future")
    };
    let RunProgress::Complete(value) = futures
        .resume(
            vec![(id, Obj::String("ok".into()).into())],
            PrintWriter::Disabled,
        )
        .unwrap()
    else {
        panic!("missing async completion")
    };
    assert_eq!(value, Obj::String("ok".into()));
    println!(
        "MONTY_MICRO={}",
        json!({"revision":REV,"initial_compile_us":compile_us,"first_run_us":first_us,"compile_ns":compile_ns,"fresh_heap_run_ns":run_ns,"clone_start_ns":start_ns,"warm_repl_call_ns":repl_ns,"warm_repl_error":repl_error,"restore_ns":restore_ns,"snapshot_bytes":snapshot.len(),"state_restore":true,"async_resume":true,"probes":probes})
    );
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_none_or(|s| s == "micro") {
        micro();
        return;
    }
    let port = &args[1];
    let threads: usize = args[2].parse().unwrap();
    let app = App {
        upstream: args.get(3).cloned().unwrap_or_default(),
        client: reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap(),
        native: args.get(4).is_some_and(|s| s == "rust"),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        let router = Router::new()
            .route("/health", get(|| async { "ok" }))
            .route("/hello", post(handler))
            .with_state(app);
        axum::serve(listener, router).await.unwrap();
    });
}
