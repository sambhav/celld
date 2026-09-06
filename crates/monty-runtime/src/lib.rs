//! Native Python execution. The embedding host owns storage, I/O and lifecycle.
pub mod exports;
use monty::{FunctionCall, RunProgress};
use monty_types::{MontyObject, PrintWriter};
use serde_json::{Value, json};

/// Explicit capability registry. Python cannot select arbitrary host methods.
pub const CAPABILITIES: &[&str] = &[
    "storage.get",
    "storage.put",
    "storage.delete",
    "storage.list",
    "storage.delete_all",
    "storage.sql",
    "storage.get_alarm",
    "storage.sync",
    "storage.set_alarm",
    "storage.delete_alarm",
    "storage.transaction_begin",
    "storage.transaction_commit",
    "storage.transaction_rollback",
    "object.call",
    "fetch",
    "sleep",
    "now",
    "uuid",
    "log",
];

pub struct Session {
    pending: Option<FunctionCall>,
    calls: usize,
}
impl Session {
    pub fn start(
        function: &exports::Function,
        args: &Value,
        context: &Value,
    ) -> Result<(Self, Value), String> {
        let mut session = Self {
            pending: None,
            calls: 0,
        };
        let event = session.advance(function.start_with_context(args, context)?)?;
        Ok((session, event))
    }
    pub fn resume(&mut self, reply: Value) -> Result<Value, String> {
        let call = self.pending.take().ok_or("session is not suspended")?;
        let reply = reply.to_string();
        if reply.len() > 1024 * 1024 {
            return Err("host result exceeds 1 MiB".into());
        }
        let progress = call
            .resume(MontyObject::String(reply), PrintWriter::Disabled)
            .map_err(|e| e.to_string())?;
        self.advance(progress)
    }
    fn advance(&mut self, progress: RunProgress) -> Result<Value, String> {
        match progress {
            RunProgress::Complete(MontyObject::String(result)) => {
                if result.len() > 1024 * 1024 {
                    return Err("result exceeds 1 MiB".into());
                }
                Ok(
                    json!({"done":true,"result":serde_json::from_str::<Value>(&result).map_err(|e|e.to_string())?}),
                )
            }
            RunProgress::FunctionCall(call) => {
                self.calls += 1;
                if self.calls > 1000 {
                    return Err("host call limit exceeded".into());
                }
                if call.function_name != "_celld_host"
                    || !call.kwargs.is_empty()
                    || call.object_id.is_some()
                {
                    return Err("unregistered host function".into());
                }
                let [MontyObject::String(operation), MontyObject::String(args)] =
                    call.args.as_slice()
                else {
                    return Err("invalid host arguments".into());
                };
                if !CAPABILITIES.contains(&operation.as_str()) {
                    return Err("unknown capability".into());
                }
                if args.len() > 1024 * 1024 {
                    return Err("host arguments exceed 1 MiB".into());
                }
                let args: Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
                if !args.is_array() {
                    return Err("host arguments must be an array".into());
                }
                let event = json!({"done":false,"operation":operation,"args":args});
                self.pending = Some(call);
                Ok(event)
            }
            _ => Err(
                "unsupported suspension; only registered celld capabilities are available".into(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_context_supports_keywords_classes_and_cross_object_calls() {
        let module = exports::Module::compile("def plain(value:int=1): return value\ndef info(*,ctx): return ctx.name\nasync def forward(ctx): return await ctx.object('other').call('plain',value=2)\n").unwrap();
        let manifest = module.manifest();
        let functions = manifest.as_array().unwrap();
        assert_eq!(
            functions.iter().find(|f| f["name"] == "plain").unwrap()["context"],
            false
        );
        assert_eq!(
            functions.iter().find(|f| f["name"] == "info").unwrap()["parameters"]["properties"],
            json!({})
        );
        assert_eq!(
            Session::start(module.get("plain").unwrap(), &json!({}), &json!({}))
                .unwrap()
                .1["result"],
            1
        );
        assert_eq!(
            Session::start(
                module.get("info").unwrap(),
                &json!({}),
                &json!({"name":"key"})
            )
            .unwrap()
            .1["result"],
            "key"
        );
        let (mut session, event) =
            Session::start(module.get("forward").unwrap(), &json!({}), &json!({})).unwrap();
        assert_eq!(event["operation"], "object.call");
        assert_eq!(
            event["args"],
            json!(["__CELLD_FUNCTIONS","other","plain",{"value":2}])
        );
        assert_eq!(session.resume(json!({"result":2})).unwrap()["result"], 2);
        let class =
            exports::Module::entry("class Default:\n    def info(self,*,ctx): return ctx.name\n")
                .unwrap();
        assert_eq!(
            Session::start(
                class.get("info").unwrap(),
                &json!({}),
                &json!({"name":"class-key"})
            )
            .unwrap()
            .1["result"],
            "class-key"
        );
    }
    #[test]
    fn classes_export_public_methods_and_construct_with_context() {
        let source = "class Counter:\n    def __init__(self, ctx): self.ctx=ctx\n    def add(self, amount:int=1): return self.ctx.storage.get('n',0)+amount\n    def _private(self): pass\n";
        let module = exports::Module::compile_class(source, "Counter").unwrap();
        assert_eq!(module.manifest().as_array().unwrap().len(), 1);
        let (mut session, event) =
            Session::start(module.get("add").unwrap(), &json!({"amount":2}), &json!({})).unwrap();
        assert_eq!(event["operation"], "storage.get");
        assert_eq!(
            session
                .resume(json!({"result":{"found":true,"value":40}}))
                .unwrap()["result"],
            42
        );
        assert!(exports::Module::compile_class(source, "Missing").is_err());
        let module = exports::Module::compile_class(
            "class Hello:\n    def hello(self, name:str='world'): return name\n",
            "Hello",
        )
        .unwrap();
        assert_eq!(
            Session::start(module.get("hello").unwrap(), &json!({}), &json!({}))
                .unwrap()
                .1["result"],
            "world"
        );
    }
    #[test]
    fn context_import_is_resolved_by_the_compiler() {
        let m = exports::Module::compile(
            "from celld import Context\ndef hello(ctx: Context): return ctx.env['GREETING']\n",
        )
        .unwrap();
        assert_eq!(
            Session::start(
                m.get("hello").unwrap(),
                &json!({}),
                &json!({"env":{"GREETING":"hi"}})
            )
            .unwrap()
            .1["result"],
            "hi"
        );
        assert!(exports::Module::compile("from celld import App\ndef hello(): return 1").is_err());
    }
    #[test]
    fn stateless_storage_errors_are_catchable() {
        let m=exports::Module::compile("def hello(ctx):\n    try:\n        ctx.storage.get('x')\n    except RuntimeError:\n        return 'no storage'\n").unwrap();
        let (mut s, _) = Session::start(m.get("hello").unwrap(), &json!({}), &json!({})).unwrap();
        assert_eq!(
            s.resume(json!({"error":"storage requires a durable object"}))
                .unwrap()["result"],
            "no storage"
        );
    }
    #[test]
    fn stored_null_is_distinct_from_a_missing_key() {
        let m = exports::Module::compile("def read(ctx): return ctx.storage.get('x',99)").unwrap();
        for (found, expected) in [(true, Value::Null), (false, json!(99))] {
            let (mut session, _) =
                Session::start(m.get("read").unwrap(), &json!({}), &json!({})).unwrap();
            assert_eq!(
                session
                    .resume(json!({"result":{"found":found,"value":null}}))
                    .unwrap()["result"],
                expected
            );
        }
    }
    #[test]
    fn interpreter_budget_stops_a_busy_loop() {
        let m = exports::Module::compile("def loop():\n    while True: pass\n").unwrap();
        assert!(Session::start(m.get("loop").unwrap(), &json!({}), &json!({})).is_err());
    }
    #[test]
    fn example_counter_runs_a_transaction_from_an_instance_method() {
        let m = exports::Module::compile_class(
            include_str!("../../../examples/monty/worker.py"),
            "Counter",
        )
        .unwrap();
        let (mut s, event) =
            Session::start(m.get("increment").unwrap(), &json!({}), &json!({})).unwrap();
        assert_eq!(event["operation"], "storage.transaction_begin");
        assert_eq!(
            s.resume(json!({"result":null})).unwrap()["operation"],
            "storage.get"
        );
        assert_eq!(
            s.resume(json!({"result":{"found":false,"value":null}}))
                .unwrap()["args"],
            json!(["count", 1])
        );
        assert_eq!(
            s.resume(json!({"result":null})).unwrap()["operation"],
            "storage.transaction_commit"
        );
        assert_eq!(s.resume(json!({"result":null})).unwrap()["result"], 1);
    }
    #[test]
    fn context_is_injected_and_not_a_client_argument() {
        let module = exports::Module::compile("def increment(ctx: Context, amount:int=1):\n    value=ctx.storage.get('count', 0)+amount\n    ctx.storage.put('count', value)\n    return {'value':value, 'id':ctx.id}\n").unwrap();
        let f = module.get("increment").unwrap();
        assert!(Session::start(f, &json!({"ctx":{}}), &json!({})).is_err());
        let (mut session, event) =
            Session::start(f, &json!({"amount":2}), &json!({"id":"object-a"})).unwrap();
        assert_eq!(event["operation"], "storage.get");
        let event = session
            .resume(json!({"result":{"found":true,"value":40}}))
            .unwrap();
        assert_eq!(event["args"], json!(["count", 42]));
        let event = session.resume(json!({"result":null})).unwrap();
        assert_eq!(event["result"], json!({"value":42,"id":"object-a"}));
        assert!(session.resume(json!({})).is_err());
    }
    #[test]
    fn host_errors_are_catchable_and_transactions_rollback() {
        let m=exports::Module::compile("def fail(ctx):\n    def update(storage):\n        storage.put('x', 1)\n        raise ValueError('abort')\n    try:\n        ctx.storage.transaction(update)\n    except ValueError:\n        return 'rolled back'\n").unwrap();
        let (mut s, e) = Session::start(m.get("fail").unwrap(), &json!({}), &json!({})).unwrap();
        assert_eq!(e["operation"], "storage.transaction_begin");
        assert_eq!(
            s.resume(json!({"result":null})).unwrap()["operation"],
            "storage.put"
        );
        assert_eq!(
            s.resume(json!({"result":null})).unwrap()["operation"],
            "storage.transaction_rollback"
        );
        assert_eq!(
            s.resume(json!({"result":null})).unwrap()["result"],
            "rolled back"
        );
    }
    #[test]
    fn undeclared_capabilities_fail_closed() {
        let m = exports::Module::compile("def bad(): return _celld_host('shell', '[]')").unwrap();
        assert!(Session::start(m.get("bad").unwrap(), &json!({}), &json!({})).is_err());
    }
}
