//! Monty's native worker backend. No V8 isolate, JavaScript bootstrap, promise,
//! or value conversion is involved in a Python invocation. The shared driver
//! owns asynchronous futures; this backend only runs bounded interpreter turns.
use super::*;
use crate::monty_host::Host;
use celld_monty::{
    exports::{durable_classes, Module},
    Failure, Session,
};
use serde_json::{json, Value};

const BODY_LIMIT: usize = 1024 * 1024;

#[derive(Clone)]
pub(super) struct Program {
    http: Module,
    classes: HashMap<String, Module>,
}
impl Program {
    fn compile(source: &str) -> Result<Self, String> {
        if source.len() > 256 * 1024 {
            return Err("Monty source exceeds 256 KiB".into());
        }
        Ok(Self {
            http: Module::compile(source)?,
            classes: durable_classes(source)?
                .into_iter()
                .map(|class| Module::compile_class(source, &class).map(|module| (class, module)))
                .collect::<Result<_, _>>()?,
        })
    }
}

pub struct Worker {
    config: Arc<WorkerConfig>,
    program: Program,
    env: Value,
    cells: storage::Cells,
    live: Arc<()>,
}

struct InputGuard {
    scope: String,
    event: u64,
}
impl InputGuard {
    fn acquire(scope: &str) -> Self {
        let event = NEXT_GATE_EVENT.fetch_add(1, Ordering::Relaxed);
        assert!(
            cell_gates()
                .lock()
                .unwrap()
                .entry(scope.to_owned())
                .or_default()
                .acquire(event),
            "native turn delivered behind a closed gate"
        );
        Self {
            scope: scope.to_owned(),
            event,
        }
    }
}
impl Drop for InputGuard {
    fn drop(&mut self) {
        let mut gates = cell_gates().lock().unwrap();
        if let Some(gate) = gates.get_mut(&self.scope) {
            gate.release(self.event);
        }
        // Release and wake under the same lock used by cell_gate_wait.
        wake_gate_waiters(&self.scope, Ok(()));
    }
}

struct Incoming {
    url: String,
    method: String,
    headers: Vec<(String, String)>,
}

pub(super) struct Execution {
    session: Option<Session>,
    host: Host,
    input: Option<Incoming>,
    request: Value,
    _gate: Option<InputGuard>,
    _live: Arc<()>,
}

impl Worker {
    pub fn load_config(config: Arc<WorkerConfig>) -> Result<Self> {
        let program = config
            .monty_program
            .get_or_init(|| {
                Program::compile(
                    config
                        .src
                        .strip_prefix(crate::python::MAGIC)
                        .expect("native artifact"),
                )
                .map(Mutex::new)
            })
            .as_ref()
            .map_err(|error| anyhow!(error.clone()))?
            .lock()
            .unwrap()
            .clone();
        for class in &config.do_classes {
            anyhow::ensure!(
                program.classes.contains_key(class),
                "unknown Monty durable class: {class}"
            );
        }
        let mut env: serde_json::Map<String, Value> = config
            .vars
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        if let Some(extra) = &config.loader_env {
            for (key, value) in serde_json::from_str::<serde_json::Map<String, Value>>(extra)? {
                if value.is_string() {
                    env.insert(key, value);
                } else {
                    env.remove(&key);
                }
            }
        }
        Ok(Self {
            program,
            config,
            env: Value::Object(env),
            cells: Default::default(),
            live: Arc::new(()),
        })
    }

    pub fn own_cell(
        &mut self,
        scope: &str,
        authority: Option<CellStorage<'_>>,
    ) -> Result<Option<i64>> {
        let _cells = self.cells.install();
        match authority {
            Some(authority) => {
                let class = scope
                    .split_once(':')
                    .map(|(class, _)| class)
                    .ok_or_else(|| anyhow!("invalid durable scope"))?;
                anyhow::ensure!(
                    self.program.classes.contains_key(class),
                    "unknown Monty durable class"
                );
                storage::open_at_epoch(
                    scope,
                    authority.path,
                    authority.epoch,
                    self.config.compat.sqlite_vec,
                )?;
                if let Some(name) = storage::get_actor_name(scope)? {
                    self.validate_name(scope, &name)?;
                }
                Ok(storage::get_alarm(scope))
            }
            None => {
                storage::close(scope);
                Ok(None)
            }
        }
    }
    fn validate_name(&self, scope: &str, name: &str) -> Result<()> {
        let (class, id) = scope
            .split_once(':')
            .ok_or_else(|| anyhow!("invalid durable scope"))?;
        let key = namespace_key(&self.config.script_name, class);
        let expected = durable_object_id_hex(&durable_object_id_for_name(&key, name));
        anyhow::ensure!(
            id == expected,
            "actor name does not match Durable Object ID for {scope}"
        );
        Ok(())
    }
    pub fn set_id_name(&mut self, scope: &str, name: &str) -> Result<()> {
        let _cells = self.cells.install();
        self.validate_name(scope, name)?;
        storage::set_actor_name(scope, name)?;
        Ok(())
    }
    pub fn take_alarm_moves(&mut self) -> Vec<(String, i64)> {
        let _cells = self.cells.install();
        storage::take_alarm_moves()
    }

    fn entry(
        &self,
        reply: Answer,
        scope: Option<String>,
        request_id: Option<RequestId>,
        trace: Option<crate::telemetry::TraceIds>,
    ) -> InFlight {
        let context = IoContext::new();
        let writes_before = scope.as_deref().and_then(storage::write_position);
        context.begin_event();
        if let Some(scope) = &scope {
            context
                .egress
                .lock()
                .unwrap()
                .push((scope.clone(), writes_before.unwrap_or(0)));
        }
        let gate = scope.as_deref().map(InputGuard::acquire);
        InFlight {
            promise: None,
            runtime_state: None,
            context,
            writes_before,
            request_id,
            active_request_id: None,
            reply: Some(reply),
            gated_reply: None,
            background: None,
            ops: Default::default(),
            alarm: None,
            started: Instant::now(),
            trace,
            failure: None,
            native: Some(Execution {
                session: None,
                host: Host::new(scope.clone()),
                input: None,
                request: json!({}),
                _gate: gate,
                _live: self.live.clone(),
            }),
            scope,
        }
    }

    pub fn turn_begin(
        &mut self,
        job: crate::WorkerJob,
        trace: Option<crate::telemetry::TraceIds>,
    ) -> (Option<InFlight>, Vec<Op>) {
        let _cells = self.cells.install();
        let (url, method, body, headers, request_id, reply) = match job {
            crate::WorkerJob::Fetch {
                url,
                method,
                body,
                headers,
                request_id,
                reply,
                ..
            } => (url, method, body, headers, request_id, reply),
            crate::WorkerJob::Rpc { reply, .. } => {
                let _ = reply.send(Err(anyhow!("Monty functions are exposed as POST handlers")));
                return (None, vec![]);
            }
            crate::WorkerJob::Queue { reply, .. } => {
                let _ = reply.send(Err(anyhow!("Monty queue consumers are not supported")));
                return (None, vec![]);
            }
        };
        self.begin_fetch(url, method, body, headers, request_id, reply, None, trace)
    }

    #[allow(clippy::too_many_arguments)]
    fn begin_fetch(
        &self,
        url: String,
        method: String,
        body: RequestBody,
        headers: Vec<(String, String)>,
        request_id: Option<RequestId>,
        reply: tokio::sync::oneshot::Sender<Result<HttpResponse>>,
        scope: Option<String>,
        trace: Option<crate::telemetry::TraceIds>,
    ) -> (Option<InFlight>, Vec<Op>) {
        if Arc::strong_count(&self.live) > 256 {
            let _ = reply.send(http_response(error_event(Failure {
                status: 503,
                code: "overloaded".into(),
                message: "Monty invocation capacity exceeded".into(),
            })));
            return (None, vec![]);
        }
        let mut entry = self.entry(Answer::Fetch(reply), scope, request_id, trace);
        let _context = CurrentGuard::enter(entry.context.clone());
        if take_request_cancellation(request_id) {
            entry.fail(anyhow!("The client has disconnected"));
            return (Some(entry), vec![]);
        }
        let input = Incoming {
            url,
            method,
            headers,
        };
        let result = match body {
            RequestBody::Bytes(body) => self.http(&mut entry, input, &body),
            RequestBody::Stream(id) => {
                entry.context.own_body_stream(id);
                entry.native.as_mut().unwrap().input = Some(input);
                match take_body_stream(id) {
                    Ok(stream) => {
                        asyncrt::enqueue(async move { collect(stream).await });
                        Ok(None)
                    }
                    Err(error) => Err(error.into()),
                }
            }
        };
        self.process(&mut entry, result);
        let ops = adopt(&mut entry);
        (Some(entry), ops)
    }

    fn http(
        &self,
        entry: &mut InFlight,
        input: Incoming,
        body: &[u8],
    ) -> Result<Option<Value>, Failure> {
        if body.len() > BODY_LIMIT {
            return Err(body_limit());
        }
        let url =
            url::Url::parse(&input.url).map_err(|_| Failure::arguments("invalid request URL"))?;
        let path = url.path().strip_prefix('/').unwrap_or("");
        let name = percent_encoding::percent_decode_str(path)
            .decode_utf8()
            .map_err(|_| Failure::arguments("invalid handler name"))?;
        if path.contains('/')
            || name.starts_with('_')
            || (entry.scope.is_none() && self.program.http.get(&name).is_none())
        {
            return Ok(Some(http_event(404, "unknown handler", json!({}))));
        }
        if input.method != "POST" {
            return Ok(Some(http_event(
                405,
                "POST required",
                json!({"allow":"POST"}),
            )));
        }
        let args: Value = if body.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(body)
                .map_err(|_| Failure::arguments("request body must be a UTF-8 JSON object"))?
        };
        let (function, args, metadata) = if let Some(scope) = &entry.scope {
            if name == "alarm" {
                return Err("alarm is dispatched by the host".into());
            }
            let class = scope.split_once(':').ok_or("invalid durable scope")?.0;
            let function = self
                .program
                .classes
                .get(class)
                .and_then(|m| m.get(&name))
                .ok_or("unknown public method")?;
            let wire = args["wire"]
                .as_str()
                .ok_or_else(|| Failure::arguments("missing typed object arguments"))?;
            let arguments = celld_monty::value::to_json(
                &serde_json::from_str(wire)
                    .map_err(|e| Failure::arguments(format!("invalid object arguments: {e}")))?,
            )?;
            let id = storage::get_actor_name(scope)
                .map_err(|e| e.to_string())?
                .ok_or("Monty durable objects require a named ID")?;
            (
                function,
                arguments,
                json!({"id":id,"env":self.env,"request":args["request"]}),
            )
        } else {
            let Some(function) = self.program.http.get(&name) else {
                return Ok(Some(http_event(404, "unknown handler", json!({}))));
            };
            (
                function,
                args,
                json!({"id":null,"env":self.env,"request":{"url":input.url,"method":input.method,"headers":headers_object(&input.headers)}}),
            )
        };
        entry.native.as_mut().unwrap().request = metadata["request"].clone();
        let (session, event) = Session::start(function, &args, &metadata)?;
        entry.native.as_mut().unwrap().session = Some(session);
        Ok(Some(event))
    }

    pub fn turn_begin_cell(
        &mut self,
        job: CellJob,
        trace: Option<crate::telemetry::TraceIds>,
    ) -> (Option<InFlight>, Vec<Op>) {
        let _cells = self.cells.install();
        if Arc::strong_count(&self.live) > 256 {
            job.fail(anyhow!("Monty invocation capacity exceeded"));
            return (None, vec![]);
        }
        let (scope, answer, alarm) = match job {
            CellJob::Fetch {
                scope,
                name,
                url,
                method,
                body,
                headers,
                request_id,
                reply,
                ..
            } => {
                if let Some(name) = name {
                    if let Err(error) = self.set_id_name(&scope, &name) {
                        let _ = reply.send(Err(error));
                        return (None, vec![]);
                    }
                }
                return self.begin_fetch(
                    url,
                    method,
                    body,
                    headers,
                    request_id,
                    reply,
                    Some(scope),
                    trace,
                );
            }
            CellJob::Alarm {
                scope,
                scheduled_ms,
                claim: _,
                reply,
            } => {
                let now = unix_now_ms();
                if now < scheduled_ms {
                    let _ = reply.send(Err(anyhow!("alarm dispatched before its deadline")));
                    return (None, vec![]);
                }
                let Some((scheduled, _retry)) = storage::due_alarm_entry(&scope, now) else {
                    let _ = reply.send(Ok((storage::get_alarm(&scope), None)));
                    return (None, vec![]);
                };
                storage::begin_alarm_handler(&scope, scheduled);
                (
                    scope,
                    Answer::Alarm(reply),
                    Some(AlarmClaim { now_ms: now }),
                )
            }
            job => {
                job.fail(anyhow!("Monty durable objects expose methods and alarms"));
                return (None, vec![]);
            }
        };
        let mut entry = self.entry(answer, Some(scope.clone()), None, trace);
        entry.alarm = alarm;
        let _context = CurrentGuard::enter(entry.context.clone());
        let result = (|| {
            let class = scope.split_once(':').ok_or("invalid durable scope")?.0;
            let function = self
                .program
                .classes
                .get(class)
                .and_then(|module| module.get("alarm"))
                .ok_or("define alarm(self) before scheduling an alarm")?;
            let id = storage::get_actor_name(&scope)
                .map_err(|e| e.to_string())?
                .ok_or("Monty durable objects require a named ID")?;
            let metadata = json!({"id":id, "env":self.env, "request":{}});
            let (session, event) = Session::start(function, &json!({}), &metadata)?;
            entry.native.as_mut().unwrap().session = Some(session);
            Ok(Some(event))
        })();
        self.process(&mut entry, result);
        let ops = adopt(&mut entry);
        (Some(entry), ops)
    }

    pub fn turn_deliver(
        &mut self,
        entry: &mut InFlight,
        op: u64,
        result: Result<asyncrt::OpOut, String>,
    ) -> Vec<Op> {
        let _cells = self.cells.install();
        let _context = CurrentGuard::enter(entry.context.clone());
        entry.ops.remove(&op);
        if take_request_cancellation(entry.request_id) {
            self.turn_cancel(entry);
            return vec![];
        }
        let Some(execution) = entry.native.as_mut() else {
            return vec![];
        };
        let result = if let Some(input) = execution.input.take() {
            match result {
                Ok(asyncrt::OpOut::Bytes(body)) => self.http(entry, input, &body),
                Err(error) if error == "body exceeds 1 MiB" => Err(body_limit()),
                Err(error) => Err(error.into()),
                _ => Err("invalid native body result".into()),
            }
        } else {
            let session = execution.session.as_mut().expect("suspended session");
            match result {
                Ok(asyncrt::OpOut::MontyFetch {
                    status,
                    headers,
                    body,
                }) => session.resume_fetch(status, headers, body),
                Ok(asyncrt::OpOut::Monty(reply)) => session.resume(reply),
                Err(error) => session.resume(json!({"error":error})),
                _ => Err("invalid native operation result".into()),
            }
            .map(Some)
        };
        self.process(entry, result);
        adopt(entry)
    }

    fn process(&self, entry: &mut InFlight, result: Result<Option<Value>, Failure>) {
        let result = result.and_then(|event| match event {
            Some(event) => self.drive(entry, event),
            None => Ok(None),
        });
        match result {
            Ok(Some(event)) => self.finish(entry, Ok(event)),
            Err(error) => self.finish(entry, Err(error)),
            Ok(None) => {}
        }
    }
    fn drive(&self, entry: &mut InFlight, mut event: Value) -> Result<Option<Value>, Failure> {
        loop {
            let execution = entry.native.as_mut().expect("live invocation");
            if event["done"] == true {
                if execution.host.in_transaction() {
                    return Err("unclosed transaction".into());
                }
                return Ok(Some(event));
            }
            let op = event["operation"]
                .as_str()
                .ok_or("missing host operation")?;
            let reply = if Host::handles(op) {
                match execution.host.call(op, &event["args"]) {
                    Ok((reply, alarm)) => {
                        if let (Some(scope), Some(at)) = (&execution.host.scope, alarm) {
                            if at >= 0 {
                                spawn_arm_gate(scope, at, Some(entry.context.clone()));
                            }
                        }
                        reply
                    }
                    Err(error) => json!({"error":error}),
                }
            } else if execution.host.in_transaction() {
                json!({"error":"external I/O is not allowed inside a storage transaction"})
            } else {
                match self.suspend(entry, op, &event["args"]) {
                    Ok(()) => return Ok(None),
                    Err(error) => json!({"error":error}),
                }
            };
            event = entry
                .native
                .as_mut()
                .unwrap()
                .session
                .as_mut()
                .expect("running session")
                .resume(reply)?;
        }
    }

    fn suspend(&self, entry: &mut InFlight, operation: &str, args: &Value) -> Result<(), String> {
        match operation {
            "storage.sync" => {
                if entry.scope.is_none() {
                    return Err("storage requires a durable object".into());
                }
                let gate = egress_gate_request(celld_logic::Channel::Response);
                asyncrt::enqueue(async move {
                    await_egress_gate(gate).await?;
                    Ok(asyncrt::OpOut::Monty(json!({"result":null})))
                });
            }
            "sleep" => {
                let seconds = args[0]
                    .as_f64()
                    .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 30.0)
                    .ok_or("sleep must be between 0 and 30 seconds")?;
                asyncrt::enqueue(async move {
                    asyncrt::sleep(Duration::from_secs_f64(seconds)).await;
                    Ok(asyncrt::OpOut::Monty(json!({"result":null})))
                });
            }
            "fetch" => {
                if self.config.egress == EgressPolicy::Deny {
                    return Err(
                        "This worker is not permitted to access the internet via fetch".into(),
                    );
                }
                let url = args[0].as_str().ok_or("invalid fetch URL")?.to_owned();
                let method = args[1].as_str().ok_or("invalid fetch method")?.to_owned();
                let headers = args[2]
                    .as_object()
                    .ok_or("invalid fetch headers")?
                    .iter()
                    .map(|(k, v)| {
                        v.as_str()
                            .map(|v| (k.clone(), v.to_owned()))
                            .ok_or("invalid header".to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let body = if let Some(bytes) = entry
                    .native
                    .as_mut()
                    .and_then(|e| e.session.as_mut())
                    .and_then(Session::take_fetch_body)
                {
                    Some(RequestBody::Bytes(bytes.into()))
                } else {
                    match &args[3] {
                        Value::Null => None,
                        Value::String(s) => Some(RequestBody::Bytes(s.clone().into())),
                        _ => return Err("invalid fetch body".into()),
                    }
                };
                let future = outbound_fetch(
                    method,
                    url,
                    body,
                    headers,
                    false,
                    RequestBodyGuard(None),
                    entry.trace,
                );
                asyncrt::enqueue(async move {
                    let response = future.await?;
                    let status = response.status().as_u16();
                    let headers = headers_object(
                        &response
                            .headers()
                            .iter()
                            .map(|(k, v)| {
                                (k.to_string(), v.to_str().unwrap_or_default().to_owned())
                            })
                            .collect::<Vec<_>>(),
                    );
                    let body =
                        collect(Box::pin(response.bytes_stream().map(|chunk| {
                            chunk.map(|b| b.to_vec()).map_err(|e| e.to_string())
                        })))
                        .await?;
                    Ok(asyncrt::OpOut::MontyFetch {
                        status,
                        headers,
                        body,
                    })
                });
            }
            "object.call" => {
                let class = args[0].as_str().ok_or("invalid durable class")?;
                if !self.program.classes.contains_key(class) {
                    return Err("unknown durable class".into());
                }
                let name = args[1].as_str().ok_or("invalid durable ID")?.to_owned();
                let method = args[2].as_str().ok_or("invalid method")?.to_owned();
                if method.starts_with('_') || method == "alarm" {
                    return Err("unknown public method".into());
                }
                let key = namespace_key(&self.config.script_name, class);
                let id = durable_object_id_hex(&durable_object_id_for_name(&key, &name));
                let scope = format!("{class}:{id}");
                if entry.scope.as_deref() == Some(&scope) {
                    return Err("call same-object methods through self".into());
                }
                let gate = egress_gate_request(celld_logic::Channel::CellRpc);
                let order = Some(enter_call_order(entry.context.clone(), &scope));
                let request_id = next_do_request_id();
                let (cancel_sender, cancel) = tokio::sync::oneshot::channel();
                do_call_cancels()
                    .lock()
                    .unwrap()
                    .insert(request_id, cancel_sender);
                let mut cancel_guard = DoCallCancelGuard::new(request_id);
                let (reply, receive) = tokio::sync::oneshot::channel();
                let metadata = &entry.native.as_ref().unwrap().request;
                let body = RequestBody::Bytes(
                    json!({"wire":args[3],"request":metadata})
                        .to_string()
                        .into(),
                );
                let request = DoCallReq {
                    request_id: Some(request_id),
                    cancel: Some(cancel),
                    deliver_abort_to_handler: false,
                    scope,
                    name: Some(name),
                    url: format!("http://monty.internal/{method}"),
                    method: "POST".into(),
                    body_guard: RequestBodyGuard::of(&body),
                    body,
                    headers: vec![("content-type".into(), "application/json".into())],
                    reply,
                    order,
                    parent: entry.trace,
                };
                asyncrt::enqueue(async move {
                    gated_channel_send(gate, &DO_CALL_TX, request, "no proxy channel").await?;
                    let response = receive
                        .await
                        .map_err(|_| "durable call dropped".to_string())?
                        .map_err(|e| e.to_string())?;
                    cancel_guard.disarm();
                    if response.body.len() > BODY_LIMIT {
                        return Err("object result exceeds 1 MiB".into());
                    }
                    Ok(asyncrt::OpOut::Monty(
                        serde_json::from_slice(&response.body)
                            .map_err(|e| format!("invalid durable response: {e}"))?,
                    ))
                });
            }
            _ => return Err("unknown asynchronous capability".into()),
        }
        Ok(())
    }

    fn finish(&self, entry: &mut InFlight, result: Result<Value, Failure>) {
        // Drop/rollback while this worker's storage is installed. A suspended
        // invocation never retains an open SQL transaction.
        let response = entry
            .native
            .as_mut()
            .and_then(|e| e.session.as_mut())
            .and_then(Session::take_response);
        entry.native.take();
        let result = match storage_error(entry.scope.as_deref()) {
            Some(error) => Err(error.into()),
            None => result,
        };
        if let Err(error) = &result {
            entry.failure = Some(crate::telemetry::cap_error(error.to_string()));
        }
        entry.context.end_event();
        let gates = entry.context.take_arm_gates();
        let Some(answer) = entry.reply.take() else {
            return;
        };
        entry.gated_reply = match answer {
            Answer::Fetch(reply) => {
                let result = if entry.scope.is_some() {
                    let value = match result {
                        Ok(event) => json!({"wire":event["wire"]}),
                        Err(error) => json!({"error":error.json()}),
                    };
                    Ok(HttpResponse {
                        status: 200,
                        headers: vec![("content-type".into(), "application/json".into())],
                        body: value.to_string().into_bytes(),
                        stream: None,
                        websocket: None,
                        write_position: entry.write_delta(),
                    })
                } else {
                    match (result, response) {
                        (Ok(_), Some(response)) => Ok(HttpResponse {
                            status: response.status,
                            headers: response.headers,
                            body: response.body,
                            stream: None,
                            websocket: None,
                            write_position: None,
                        }),
                        (Ok(event), None) => http_response(event),
                        (Err(error), _) => http_response(error_event(error)),
                    }
                };
                send_answer_after_arm_gates(reply, result, gates)
            }
            Answer::Alarm(reply) => {
                entry.settle_alarm(result.is_ok(), result.is_err());
                let result = result
                    .map(|_| {
                        (
                            entry.scope.as_deref().and_then(storage::get_alarm),
                            entry.write_delta(),
                        )
                    })
                    .map_err(anyhow::Error::from);
                send_answer_after_arm_gates(reply, result, gates)
            }
            answer => answer.fail_with_arm_gates(anyhow!("invalid native answer"), gates),
        };
    }
    pub fn turn_cancel(&mut self, entry: &mut InFlight) -> Vec<Op> {
        let _cells = self.cells.install();
        entry.fail(anyhow!("The client has disconnected"));
        entry.context.end_event();
        vec![]
    }
    pub fn turn_poll(&mut self, _entry: &mut InFlight) -> Vec<Op> {
        vec![]
    }
    pub fn turn_finish_alarm(&mut self, entry: &mut InFlight) {
        let _cells = self.cells.install();
        entry.settle_alarm(false, false);
    }
}

fn storage_error(scope: Option<&str>) -> Option<String> {
    scope.and_then(storage::sql_critical_error)
}
fn body_limit() -> Failure {
    Failure {
        status: 413,
        code: "body_too_large".into(),
        message: "body exceeds 1 MiB".into(),
    }
}
fn http_event(status: u16, body: &str, mut headers: Value) -> Value {
    headers["content-type"] = json!("text/plain;charset=UTF-8");
    json!({"done":true,"response":{"status":status,"body":body,"headers":headers}})
}
fn error_event(error: Failure) -> Value {
    json!({"done":true,"response":{"status":error.status,"headers":{"content-type":"application/json"},"body":json!({"error":{"code":error.code,"message":error.message}}).to_string()}})
}
fn http_response(mut event: Value) -> Result<HttpResponse> {
    let r = &mut event["response"];
    let body = match r["body"].take() {
        Value::Null => vec![],
        Value::String(s) => s.into_bytes(),
        value => serde_json::from_value::<Vec<u8>>(value)?,
    };
    let headers = r["headers"]
        .as_object()
        .ok_or_else(|| anyhow!("invalid response headers"))?
        .iter()
        .map(|(k, v)| {
            v.as_str()
                .map(|v| (k.clone(), v.to_owned()))
                .ok_or_else(|| anyhow!("invalid response header"))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(HttpResponse {
        status: r["status"]
            .as_u64()
            .ok_or_else(|| anyhow!("invalid response status"))? as u16,
        body,
        headers,
        stream: None,
        websocket: None,
        write_position: None,
    })
}
fn headers_object(headers: &[(String, String)]) -> Value {
    let mut result = serde_json::Map::new();
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        match result.get_mut(&name) {
            Some(Value::String(previous)) if name != "set-cookie" => {
                previous.push_str(", ");
                previous.push_str(value);
            }
            _ => {
                result.insert(name, json!(value));
            }
        }
    }
    Value::Object(result)
}
async fn collect(mut stream: HttpChunkStream) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if chunk.len() > BODY_LIMIT - body.len() {
            return Err("body exceeds 1 MiB".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
