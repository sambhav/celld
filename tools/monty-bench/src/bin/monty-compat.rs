use monty::{Dump, MontyRepl, MontyRun, Session, SessionRef, dump};
use monty_types::{CompileOptions, MontyObject as Obj, PrintWriter, ResourceTracker};
use serde_json::json;
use std::time::Instant;
fn main() {
    let cases = [
        ("pydantic_import", "from pydantic import BaseModel"),
        ("pydantic_core_import", "import pydantic_core"),
        ("local_python_import", "import helper"),
        (
            "class_inheritance",
            "class Base: pass\nclass Model(Base): pass",
        ),
        ("metaclass", "class Model(metaclass=type): pass"),
        (
            "get_annotations",
            "def f(x:int)->str: return str(x)\nf.__annotations__",
        ),
        (
            "dataclass",
            "from dataclasses import dataclass\n@dataclass\nclass Input:\n    value:int\nInput(42).value",
        ),
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
            .and_then(|r| r.run(vec![], ResourceTracker::default(), PrintWriter::Disabled));
            match result {
                Ok(v) => json!({"name":name,"ok":true,"value":format!("{v:?}")}),
                Err(e) => json!({"name":name,"ok":false,"error":e.to_string()}),
            }
        })
        .collect();
    let code = "def hello(name:str):\n    return isinstance(name,str)";
    let mut repl = MontyRepl::new(
        "app.py",
        ResourceTracker::default(),
        CompileOptions::default(),
    );
    repl.feed_run(code, vec![], PrintWriter::Disabled).unwrap();
    let direct = repl
        .call_function(
            "hello",
            vec![Obj::String("Ada".into())],
            PrintWriter::Disabled,
        )
        .map(|v| format!("{v:?}"))
        .map_err(|e| e.to_string());
    let feed = repl
        .feed_run(
            "hello(name)",
            vec![("name".into(), Obj::String("Ada".into()))],
            PrintWriter::Disabled,
        )
        .map(|v| format!("{v:?}"))
        .map_err(|e| e.to_string());
    let mut state = MontyRepl::new(
        "counter.py",
        ResourceTracker::default(),
        CompileOptions::default(),
    );
    state.feed_run("counter={'n':0}\ndef increment(amount:int=1):\n    counter['n'] += amount\n    return counter['n']",vec![],PrintWriter::Disabled).unwrap();
    assert_eq!(
        state
            .call_function("increment", vec![Obj::Int(2)], PrintWriter::Disabled)
            .unwrap(),
        Obj::Int(2)
    );
    let bytes = dump("counter.py", None, SessionRef::Idle(&state)).unwrap();
    let Session::Idle(mut restored) = Dump::load(&bytes).unwrap().state else {
        panic!()
    };
    assert_eq!(
        restored
            .call_function("increment", vec![Obj::Int(3)], PrintWriter::Disabled)
            .unwrap(),
        Obj::Int(5)
    );
    // The working feed API reparses a small call expression. Measure its cost and
    // retained serialized state, rather than assuming it is a free warm-call fix.
    let mut warm = MontyRepl::new(
        "worker.py",
        ResourceTracker::default(),
        CompileOptions::default(),
    );
    warm.feed_run(
        include_str!("../../worker.py"),
        vec![],
        PrintWriter::Disabled,
    )
    .unwrap();
    let snapshot_before = dump("worker.py", None, SessionRef::Idle(&warm))
        .unwrap()
        .len();
    let mut batches = Vec::new();
    for batch in 0..5 {
        let started = Instant::now();
        for _ in 0..1000 {
            let result = warm
                .feed_run(
                    "handle(raw, False)",
                    vec![("raw".into(), Obj::String("{\"name\":\"test\"}".into()))],
                    PrintWriter::Disabled,
                )
                .unwrap();
            let Obj::String(text) = result else {
                panic!("unexpected value")
            };
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&text).unwrap(),
                json!({"result":"Hello, test"})
            );
        }
        let ns = started.elapsed().as_nanos() as f64 / 1000.0;
        batches.push(json!({"calls_total":(batch+1)*1000,"ns_per_call":ns,"snapshot_bytes":dump("worker.py",None,SessionRef::Idle(&warm)).unwrap().len()}));
    }
    println!(
        "MONTY_COMPAT={}",
        json!({"probes":probes,"warm_direct_call":direct,"warm_feed_call":feed,"stateful_function_restore":true,"state_bytes":bytes.len(),"warm_feed_snapshot_before":snapshot_before,"warm_feed_batches":batches})
    );
}
