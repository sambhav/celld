use super::*;

#[test]
fn each_run_gets_fresh_mutable_state() {
    let runner = compile("items = []\nitems.append(name)\nitems", &["name"]);
    for name in ["alice", "bob", "alice"] {
        assert_eq!(
            runner
                .run(
                    vec![Obj::String(name.into())],
                    tracker(),
                    PrintWriter::Disabled
                )
                .unwrap(),
            Obj::List(vec![Obj::String(name.into())])
        );
    }
}

#[test]
fn overlapping_host_calls_resume_with_their_own_inputs() {
    let runner = program();
    let mut pending = Vec::new();
    for name in ["alice", "bob"] {
        let RunProgress::FunctionCall(call) = runner
            .clone()
            .start(
                inputs(&json!({"name":name}).to_string(), true),
                tracker(),
                PrintWriter::Disabled,
            )
            .unwrap()
        else {
            panic!("missing I/O")
        };
        assert_eq!(
            serde_json::from_str::<Value>(&string(call.args[0].clone()).unwrap()).unwrap(),
            json!({"name":name})
        );
        pending.push((name, call));
    }
    // Complete in reverse order to catch accidentally shared invocation state.
    while let Some((name, call)) = pending.pop() {
        let price = json!({"customer":name,"unit_price_cents":1999,"stock":12,"currency":"USD"});
        let RunProgress::Complete(value) = call
            .resume(Obj::String(price.to_string()), PrintWriter::Disabled)
            .unwrap()
        else {
            panic!("missing result")
        };
        assert_eq!(
            serde_json::from_str::<Value>(&string(value).unwrap()).unwrap(),
            json!({"result":{"customer":name,"total_cents":3998,"currency":"USD","trace":name}})
        );
    }
}

#[test]
fn malformed_upstream_data_is_rejected_after_resume() {
    let runner = program();
    for price in [
        json!({"customer":"wrong","unit_price_cents":1999,"stock":12,"currency":"USD"}),
        json!({"customer":"alice","unit_price_cents":true,"stock":12,"currency":"USD"}),
        json!({"customer":"alice","unit_price_cents":1999,"stock":1,"currency":"USD"}),
    ] {
        let RunProgress::FunctionCall(call) = runner
            .clone()
            .start(
                inputs("{\"name\":\"alice\"}", true),
                tracker(),
                PrintWriter::Disabled,
            )
            .unwrap()
        else {
            panic!()
        };
        let error = call
            .resume(Obj::String(price.to_string()), PrintWriter::Disabled)
            .unwrap_err();
        assert!(error.to_string().contains("invalid price"), "{error}");
    }
}
