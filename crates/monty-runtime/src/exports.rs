use monty::{MontyRun, RunProgress};
use monty_types::{CompileOptions, MontyObject, PrintWriter, ResourceLimits, ResourceTracker};
use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{Expr, Stmt};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

#[derive(Clone, Debug)]
enum Kind {
    Any,
    Str,
    Int,
    Float,
    Bool,
    None,
    List(Box<Kind>),
    Dict(Box<Kind>),
    Union(Vec<Kind>),
}
impl Kind {
    fn parse(expr: Option<&Expr>) -> Result<Self, String> {
        let Some(expr) = expr else {
            return Ok(Self::Any);
        };
        Ok(match expr {
            Expr::Name(n) => match n.id.as_str() {
                "str" => Self::Str,
                "int" => Self::Int,
                "float" => Self::Float,
                "bool" => Self::Bool,
                "list" => Self::List(Box::new(Self::Any)),
                "dict" => Self::Dict(Box::new(Self::Any)),
                _ => return Err(format!("unsupported annotation {}", n.id)),
            },
            Expr::NoneLiteral(_) => Self::None,
            Expr::BinOp(b) if b.op == ruff_python_ast::Operator::BitOr => Self::Union(vec![
                Self::parse(Some(&b.left))?,
                Self::parse(Some(&b.right))?,
            ]),
            Expr::Subscript(s) => match s.value.as_ref() {
                Expr::Name(n) if n.id.as_str() == "list" => {
                    Self::List(Box::new(Self::parse(Some(&s.slice))?))
                }
                Expr::Name(n) if n.id.as_str() == "dict" => {
                    let Expr::Tuple(t) = s.slice.as_ref() else {
                        return Err("use dict[str, T]".into());
                    };
                    if t.elts.len() != 2 || !matches!(Self::parse(t.elts.first())?, Self::Str) {
                        return Err("JSON dictionary keys must be str".into());
                    }
                    Self::Dict(Box::new(Self::parse(t.elts.get(1))?))
                }
                _ => return Err("unsupported container annotation".into()),
            },
            _ => {
                return Err(
                    "unsupported annotation; Monty supports JSON types and T | None".into(),
                );
            }
        })
    }
    fn python_type(&self) -> String {
        match self {
            Self::Any => "Any".into(),
            Self::Str => "str".into(),
            Self::Int => "int".into(),
            Self::Float => "float".into(),
            Self::Bool => "bool".into(),
            Self::None => "None".into(),
            Self::List(k) => format!("list[{}]", k.python_type()),
            Self::Dict(k) => format!("dict[str, {}]", k.python_type()),
            Self::Union(ks) => ks
                .iter()
                .map(Self::python_type)
                .collect::<Vec<_>>()
                .join(" | "),
        }
    }
    fn valid(&self, v: &Value) -> bool {
        match self {
            Self::Any => true,
            Self::Str => v.is_string(),
            Self::Int => v.is_i64() || v.is_u64(),
            Self::Float => v.is_number(),
            Self::Bool => v.is_boolean(),
            Self::None => v.is_null(),
            Self::List(k) => v.as_array().is_some_and(|v| v.iter().all(|v| k.valid(v))),
            Self::Dict(k) => v
                .as_object()
                .is_some_and(|v| v.values().all(|v| k.valid(v))),
            Self::Union(ks) => ks.iter().any(|k| k.valid(v)),
        }
    }
    fn schema(&self) -> Value {
        match self {
            Self::Any => json!({}),
            Self::Str => json!({"type":"string"}),
            Self::Int => json!({"type":"integer"}),
            Self::Float => json!({"type":"number"}),
            Self::Bool => json!({"type":"boolean"}),
            Self::None => json!({"type":"null"}),
            Self::List(k) => json!({"type":"array","items":k.schema()}),
            Self::Dict(k) => json!({"type":"object","additionalProperties":k.schema()}),
            Self::Union(ks) => json!({"anyOf":ks.iter().map(Self::schema).collect::<Vec<_>>()}),
        }
    }
}

#[derive(Clone)]
struct Parameter {
    kind: Kind,
    required: bool,
}
#[derive(Clone)]
pub struct Function {
    runner: MontyRun,
    parameters: BTreeMap<String, Parameter>,
    schema: Value,
}
impl Function {
    pub fn start(&self, args: &Value) -> Result<RunProgress, String> {
        self.start_with_context(args, &json!({}))
    }
    pub fn start_with_context(&self, args: &Value, context: &Value) -> Result<RunProgress, String> {
        let args = args.as_object().ok_or("arguments must be an object")?;
        for (name, p) in &self.parameters {
            match args.get(name) {
                Some(v) if !p.kind.valid(v) => return Err(format!("invalid type for {name}")),
                None if p.required => return Err(format!("missing argument {name}")),
                _ => (),
            }
        }
        for name in args.keys() {
            if !self.parameters.contains_key(name) {
                return Err(format!("unknown argument {name}"));
            }
        }
        let limits = ResourceLimits {
            max_duration: Some(Duration::from_millis(100)),
            max_recursion_depth: 100,
            ..Default::default()
        };
        self.runner
            .clone()
            .start(
                vec![
                    MontyObject::String(Value::Object(args.clone()).to_string()),
                    MontyObject::Function {
                        name: "_fetch_json".into(),
                        docstring: None,
                    },
                    MontyObject::String(context.to_string()),
                    MontyObject::Function {
                        name: "_celld_host".into(),
                        docstring: None,
                    },
                ],
                ResourceTracker::new(limits),
                PrintWriter::Disabled,
            )
            .map_err(|e| e.to_string())
    }
}

pub struct Module {
    functions: BTreeMap<String, Function>,
}
impl Module {
    /// Discovers only direct, public function declarations in the entry module.
    /// A literal __all__ optionally restricts that set; imports/classes/aliases are excluded.
    pub fn compile(source: &str) -> Result<Self, String> {
        Self::compile_inner(source, None)
    }
    pub fn compile_class(source: &str, class: &str) -> Result<Self, String> {
        Self::compile_inner(source, Some(class))
    }
    fn compile_inner(source: &str, class: Option<&str>) -> Result<Self, String> {
        let original = ruff_python_parser::parse_module(source).map_err(|e| e.to_string())?;
        let mut code = source.as_bytes().to_vec();
        for statement in &original.syntax().body {
            if let Stmt::ImportFrom(import) = statement {
                if import
                    .module
                    .as_ref()
                    .is_some_and(|name| name.as_str() == "celld")
                    && import.level == 0
                {
                    if import.names.len() != 1
                        || import.names[0].name.as_str() != "Context"
                        || import.names[0].asname.is_some()
                    {
                        return Err("Monty provides only `from celld import Context`; use Pyodide for the full SDK".into());
                    }
                    for byte in
                        &mut code[import.range.start().to_usize()..import.range.end().to_usize()]
                    {
                        if *byte != b'\n' && *byte != b'\r' {
                            *byte = b' ';
                        }
                    }
                }
            }
        }
        let source = std::str::from_utf8(&code).map_err(|e| e.to_string())?;
        let parsed = ruff_python_parser::parse_module(source).map_err(|e| e.to_string())?;
        let mut definitions = BTreeMap::new();
        let mut explicit = None;
        let class_def = if let Some(name) = class {
            Some(
                parsed
                    .syntax()
                    .body
                    .iter()
                    .find_map(|stmt| match stmt {
                        Stmt::ClassDef(c) if c.name.as_str() == name => Some(c),
                        _ => None,
                    })
                    .ok_or_else(|| format!("unknown Python class {name}"))?,
            )
        } else {
            None
        };
        let statements = class_def.map_or(&parsed.syntax().body, |c| &c.body);
        for stmt in statements {
            match stmt {
                Stmt::FunctionDef(f) => {
                    if definitions.insert(f.name.to_string(), f).is_some() {
                        return Err(format!("duplicate function {}", f.name));
                    }
                }
                Stmt::Assign(a)
                    if a.targets
                        .iter()
                        .any(|t| matches!(t,Expr::Name(n) if n.id.as_str()=="__all__")) =>
                {
                    if explicit.is_some() || a.targets.len() != 1 {
                        return Err("__all__ must be assigned once".into());
                    }
                    let values = match a.value.as_ref() {
                        Expr::List(l) => &l.elts,
                        Expr::Tuple(t) => &t.elts,
                        _ => return Err("__all__ must be a literal list or tuple".into()),
                    };
                    let mut names = BTreeSet::new();
                    for v in values {
                        let Expr::StringLiteral(s) = v else {
                            return Err("__all__ entries must be literal names".into());
                        };
                        let name = s.value.to_string();
                        if !names.insert(name) {
                            return Err("duplicate __all__ entry".into());
                        }
                    }
                    explicit = Some(names);
                }
                _ => (),
            }
        }
        // Dynamic export changes must fail rather than silently expose a larger API.
        struct ExportUses(usize);
        impl<'a> Visitor<'a> for ExportUses {
            fn visit_expr(&mut self, expr: &'a Expr) {
                if matches!(expr, Expr::Name(n) if n.id.as_str()=="__all__") {
                    self.0 += 1;
                }
                visitor::walk_expr(self, expr);
            }
        }
        let mut uses = ExportUses(0);
        for stmt in statements {
            uses.visit_stmt(stmt);
        }
        if uses.0 != usize::from(explicit.is_some()) {
            return Err("__all__ must be one plain literal assignment; dynamic or annotated exports are unsupported".into());
        }
        let constructor = definitions.get("__init__");
        let construct_with_context = constructor.is_some();
        if let Some(init) = constructor.filter(|_| class.is_some()) {
            let names: Vec<_> = init
                .parameters
                .args
                .iter()
                .map(|p| p.parameter.name.as_str())
                .collect();
            if names != ["self", "ctx"]
                || !init.parameters.kwonlyargs.is_empty()
                || init.parameters.vararg.is_some()
                || init.parameters.kwarg.is_some()
            {
                return Err("Monty class constructors must be __init__(self, ctx)".into());
            }
        }
        let names = explicit.unwrap_or_else(|| {
            definitions
                .keys()
                .filter(|n| !n.starts_with('_'))
                .cloned()
                .collect()
        });
        let mut functions = BTreeMap::new();
        for name in names {
            if name.starts_with('_') {
                return Err("private names cannot be exported".into());
            }
            let f = definitions
                .get(&name)
                .ok_or_else(|| format!("{name} is not a top-level function"))?;
            if !f.parameters.posonlyargs.is_empty()
                || f.parameters.vararg.is_some()
                || f.parameters.kwarg.is_some()
            {
                return Err(format!(
                    "{name}: use named parameters, not positional-only or variadic parameters"
                ));
            }
            let mut parameters = BTreeMap::new();
            let mut context_parameter = false;
            let mut properties = serde_json::Map::new();
            let mut required = Vec::new();
            for p in f
                .parameters
                .args
                .iter()
                .chain(f.parameters.kwonlyargs.iter())
            {
                let pname = p.parameter.name.to_string();
                if class.is_some() && pname == "self" {
                    continue;
                }
                if pname == "ctx" {
                    context_parameter = true;
                    continue;
                }
                if pname.starts_with("_celld_") {
                    return Err("_celld_ parameter names are reserved".into());
                }
                let kind = Kind::parse(p.parameter.annotation.as_deref())
                    .map_err(|e| format!("{name}.{pname}: {e}"))?;
                properties.insert(pname.clone(), kind.schema());
                if p.default.is_none() {
                    required.push(pname.clone())
                }
                parameters.insert(
                    pname,
                    Parameter {
                        kind,
                        required: p.default.is_none(),
                    },
                );
            }
            let schema = json!({"name":name,"async":f.is_async,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}});
            // Only a name parsed from the source is inserted into code. Request arguments
            // are input data, never interpolation/eval. Private functions cannot be selected.
            if class.is_some()
                && f.parameters.args.first().map(|p| p.parameter.name.as_str()) != Some("self")
            {
                return Err(format!("{name}: class methods must take self"));
            }
            if !f.decorator_list.is_empty() {
                return Err(format!(
                    "{name}: method/function decorators are not supported in Monty"
                ));
            }
            let target = class.map_or_else(
                || name.clone(),
                |class| {
                    format!(
                        "{class}({}).{name}",
                        if construct_with_context {
                            "_celld_context"
                        } else {
                            ""
                        }
                    )
                },
            );
            let call = format!(
                "{}{}({}**_celld_json.loads(_celld_args))",
                if f.is_async { "await " } else { "" },
                target,
                if context_parameter {
                    "ctx=_celld_context, "
                } else {
                    ""
                }
            );
            let code = format!(
                "{}\n{source}\n_celld_json.dumps({call})",
                include_str!("context.py")
            );
            let runner = MontyRun::new(
                code,
                "app.py",
                vec![
                    "_celld_args".into(),
                    "_fetch_json".into(),
                    "_celld_metadata".into(),
                    "_celld_host".into(),
                ],
                CompileOptions::default(),
            )
            .map_err(|e| e.to_string())?;
            functions.insert(
                name,
                Function {
                    runner,
                    parameters,
                    schema,
                },
            );
        }
        Ok(Self { functions })
    }
    /// Generates a dependency-free client; optional defaults remain owned by the server.
    pub fn python_client(&self) -> String {
        let mut source = String::from(
            "from __future__ import annotations\nimport json\nfrom typing import Any\nfrom urllib.parse import quote\nfrom urllib.request import Request, urlopen\n\n_celld_unset: Any = object()\n\nclass Client:\n    def __init__(self, base_url: str, *, timeout: float = 30):\n        self._celld_url = base_url.rstrip('/')\n        self._celld_timeout = timeout\n\n    def _celld_call(self, name, args):\n        request = Request(self._celld_url + '/call/' + quote(name, safe=''), data=json.dumps(args).encode(), headers={'content-type': 'application/json'})\n        with urlopen(request, timeout=self._celld_timeout) as response:\n            return json.load(response)['result']\n",
        );
        for (name, f) in &self.functions {
            let params = f
                .parameters
                .iter()
                .map(|(n, p)| {
                    format!(
                        "{n}: {}{}",
                        p.kind.python_type(),
                        if p.required { "" } else { "=_celld_unset" }
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            source.push_str(&format!(
                "\n    def {name}(_celld_client{}):\n        _celld_payload = {{}}\n",
                if params.is_empty() {
                    String::new()
                } else {
                    format!(", *, {params}")
                }
            ));
            for (n, p) in &f.parameters {
                if p.required {
                    source.push_str(&format!("        _celld_payload[{n:?}] = {n}\n"));
                } else {
                    source.push_str(&format!("        if {n} is not _celld_unset:\n            _celld_payload[{n:?}] = {n}\n"));
                }
            }
            source.push_str(&format!(
                "        return _celld_client._celld_call({name:?}, _celld_payload)\n"
            ));
        }
        source
    }
    pub fn get(&self, name: &str) -> Option<&Function> {
        self.functions.get(name)
    }
    pub fn manifest(&self) -> Value {
        Value::Array(self.functions.values().map(|f| f.schema.clone()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn result(f: &Function, args: Value) -> Value {
        let RunProgress::Complete(MontyObject::String(s)) = f.start(&args).unwrap() else {
            panic!("unexpected suspension")
        };
        serde_json::from_str(&s).unwrap()
    }
    #[test]
    fn public_functions_only_and_no_decorator_needed() {
        let m=Module::compile("from math import sqrt\ndef _helper(name): return 'Hello, '+name\ndef hello(name: str='world'): return _helper(name)\nclass Hidden:\n    def method(self): pass\ndef outer():\n    def nested(): return 3\n    return nested()").unwrap();
        assert_eq!(m.manifest().as_array().unwrap().len(), 2);
        assert!(m.get("_helper").is_none());
        assert!(m.get("sqrt").is_none());
        assert!(m.get("method").is_none());
        assert!(m.get("nested").is_none());
        assert_eq!(
            result(m.get("hello").unwrap(), json!({})),
            json!("Hello, world")
        );
        assert_eq!(
            result(m.get("hello").unwrap(), json!({"name":"Ada"})),
            json!("Hello, Ada")
        );
    }
    #[test]
    fn dynamic_exports_fail_closed() {
        for code in [
            "__all__:list[str]=['hello']\ndef hello():pass",
            "__all__=[]\n__all__.append('hello')\ndef hello():pass",
            "if True:\n    __all__=['hello']\ndef hello():pass",
        ] {
            assert!(Module::compile(code).is_err(), "{code}");
        }
    }
    #[test]
    fn explicit_exports_and_bad_exports() {
        let m = Module::compile("__all__=['hello']\ndef hello(): return 1\ndef helper(): return 2")
            .unwrap();
        assert!(m.get("helper").is_none());
        for code in [
            "__all__=['_hidden']\ndef _hidden():pass",
            "__all__=['missing']",
            "__all__=list()",
            "alias=__all__=['hello']\ndef hello():pass",
            "__all__=['x','x']\ndef x():pass",
            "def x(*args):pass",
        ] {
            assert!(Module::compile(code).is_err(), "{code}")
        }
    }
    #[test]
    fn strict_types_and_named_binding() {
        let m = Module::compile(
            "def add(left:int, right:int=1, *, label:str|None=None): return left+right",
        )
        .unwrap();
        let f = m.get("add").unwrap();
        assert_eq!(result(f, json!({"left":2})), json!(3));
        for args in [
            json!({}),
            json!({"left":true}),
            json!({"left":"2"}),
            json!({"left":2,"unknown":3}),
            json!({"left":2,"label":3}),
            json!([]),
        ] {
            assert!(f.start(&args).is_err(), "{args}")
        }
    }
    #[test]
    fn nested_json_annotations() {
        let m = Module::compile(
            "def total(items:list[dict[str,int]]): return sum(x['n'] for x in items)",
        )
        .unwrap();
        let f = m.get("total").unwrap();
        assert_eq!(result(f, json!({"items":[{"n":2},{"n":3}]})), json!(5));
        assert!(f.start(&json!({"items":[{"n":true}]})).is_err());
    }
    #[test]
    fn async_host_call_resumes() {
        let m =
            Module::compile("async def price(name:str): return await _fetch_json(name)").unwrap();
        let RunProgress::FunctionCall(c) = m
            .get("price")
            .unwrap()
            .start(&json!({"name":"Ada"}))
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(c.args, vec![MontyObject::String("Ada".into())]);
        let id = c.call_id;
        let RunProgress::ResolveFutures(p) = c.resume_pending(PrintWriter::Disabled).unwrap()
        else {
            panic!()
        };
        let RunProgress::Complete(MontyObject::String(s)) = p
            .resume(
                vec![(id, MontyObject::Int(42).into())],
                PrintWriter::Disabled,
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(s, "42");
    }
    #[test]
    fn globals_are_fresh_and_inputs_cannot_choose_code() {
        let m = Module::compile(
            "items=[]\ndef append(value:str):\n    items.append(value)\n    return items",
        )
        .unwrap();
        let f = m.get("append").unwrap();
        for name in ["one", "two", "'); _private() #"] {
            assert_eq!(result(f, json!({"value":name})), json!([name]));
        }
    }
}
