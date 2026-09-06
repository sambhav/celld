//! Native Monty handles belong to the isolate and are dropped with it.
use super::*;
use celld_monty::{exports::Module, Session};
use serde_json::{json, Value};

#[derive(Default)]
struct MontyState {
    modules: HashMap<u32, Module>,
    module_keys: HashMap<String, u32>,
    sessions: HashMap<u32, Session>,
    next: u32,
}

fn execute(state: &mut MontyState, request: Value) -> Result<Value, String> {
    let action = request["action"].as_str().ok_or("missing action")?;
    let id = request["id"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0);
    if action == "drop" {
        state.sessions.remove(&id);
        return Ok(Value::Null);
    }
    if action == "resume" {
        let mut session = state.sessions.remove(&id).ok_or("unknown Monty session")?;
        let mut event = session.resume(request["reply"].clone())?;
        event["id"] = json!(id);
        if event["done"] != true {
            state.sessions.insert(id, session);
        }
        return Ok(event);
    }
    if action == "compile" {
        let source = request["source"].as_str().ok_or("missing source")?;
        if source.len() > 256 * 1024 {
            return Err("Monty source exceeds 256 KiB".into());
        }
        let class = request["class"].as_str();
        let key = json!([source, class]).to_string();
        if let Some(id) = state.module_keys.get(&key) {
            return Ok(json!({"module":id}));
        }
        if state.modules.len() >= 64 {
            return Err("Monty module capacity exceeded".into());
        }
        let module = match class {
            Some(c) => Module::compile_class(source, c),
            None => Module::compile(source),
        }?;
        state.next = state
            .next
            .checked_add(1)
            .ok_or("Monty handle space exhausted")?;
        state.modules.insert(state.next, module);
        state.module_keys.insert(key, state.next);
        return Ok(json!({"module":state.next}));
    }
    if action != "start" {
        return Err("unknown Monty action".into());
    }
    if state.sessions.len() >= 256 {
        return Err("Monty session capacity exceeded".into());
    }
    let module = request["module"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or("missing module handle")?;
    let name = request["name"].as_str().ok_or("missing function name")?;
    let function = state
        .modules
        .get(&module)
        .ok_or("unknown module")?
        .get(name)
        .ok_or("unknown public function")?;
    let (session, mut event) = Session::start(function, &request["args"], &request["context"])?;
    state.next = state
        .next
        .checked_add(1)
        .ok_or("Monty handle space exhausted")?;
    event["id"] = json!(state.next);
    if event["done"] != true {
        state.sessions.insert(state.next, session);
    }
    Ok(event)
}

pub(super) fn op_monty(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let input = args.get(0).to_rust_string_lossy(scope);
    if scope.get_slot::<MontyState>().is_none() {
        scope.set_slot(MontyState::default());
    }
    let result = if input.len() > 2 * 1024 * 1024 {
        Err("Monty request exceeds 2 MiB".into())
    } else {
        serde_json::from_str(&input)
            .map_err(|e| e.to_string())
            .and_then(|request| execute(scope.get_slot_mut::<MontyState>().unwrap(), request))
    };
    let body = match result {
        Ok(value) => value,
        Err(error) => json!({"error":error}),
    }
    .to_string();
    if let Some(value) = v8::String::new(scope, &body) {
        rv.set(value.into());
    }
}
