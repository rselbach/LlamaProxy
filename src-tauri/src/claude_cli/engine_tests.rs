use super::*;
use std::collections::BTreeMap;

fn tool_use(input: Value) -> Value {
    json!({"type": "tool_use", "id": "toolu_paintball", "name": "enroll", "input": input})
}

#[test]
fn conversation_key_follows_what_was_said_not_how_it_was_sent() {
    let settings = [7u8; 32];
    let base = vec![
        json!({"role": "user", "content": "Enroll Troy Barnes"}),
        json!({"role": "assistant", "content": [{"type": "text", "text": "On it."}, tool_use(json!({"name": "Troy Barnes"}))]}),
    ];
    let want = conversation_key(&settings, &base);
    let same: BTreeMap<&str, Vec<Value>> = BTreeMap::from([
        (
            "text as a block with a cache mark",
            vec![
                json!({"role": "user", "content": [{"type": "text", "text": "Enroll Troy Barnes", "cache_control": {"type": "ephemeral"}}]}),
                base[1].clone(),
            ],
        ),
        (
            "thinking dropped and a renamed call id",
            vec![
                base[0].clone(),
                json!({"role": "assistant", "content": [{"type": "thinking", "thinking": "hm", "signature": "s"},
                {"type": "text", "text": "On it."},
                {"type": "tool_use", "id": "call_1", "name": "enroll", "input": {"name": "Troy Barnes"}}]}),
            ],
        ),
        (
            "one reply split across two messages",
            vec![
                base[0].clone(),
                json!({"role": "assistant", "content": "On it."}),
                json!({"role": "assistant", "content": [tool_use(json!({"name": "Troy Barnes"}))]}),
            ],
        ),
    ]);
    for (name, messages) in same {
        assert_eq!(conversation_key(&settings, &messages), want, "{name}");
    }
    let different: BTreeMap<&str, Vec<Value>> = BTreeMap::from([
        (
            "other words",
            vec![
                json!({"role": "user", "content": "Enroll Abed Nadir"}),
                base[1].clone(),
            ],
        ),
        (
            "other tool input",
            vec![
                base[0].clone(),
                json!({"role": "assistant", "content": [{"type": "text", "text": "On it."}, tool_use(json!({"name": "Abed Nadir"}))]}),
            ],
        ),
        ("a shorter conversation", vec![base[0].clone()]),
    ]);
    for (name, messages) in different {
        assert_ne!(conversation_key(&settings, &messages), want, "{name}");
    }
    assert_ne!(conversation_key(&[8u8; 32], &base), want, "other settings");
}

#[test]
fn tool_results_match_by_id_then_by_position() {
    let expected = vec![
        ExpectedCall {
            id: "toolu_a".into(),
            name: "enroll".into(),
            input: json!({"name": "Troy Barnes"}),
        },
        ExpectedCall {
            id: "toolu_b".into(),
            name: "enroll".into(),
            input: json!({"name": "Abed Nadir"}),
        },
    ];
    let result =
        |id: &str, text: &str| json!({"type": "tool_result", "tool_use_id": id, "content": text});
    let texts = |messages: &[Value]| {
        match_tool_results(&expected, messages).map(|answers| {
            answers
                .iter()
                .map(|(call, answer)| (call.id.clone(), answer["content"].to_string()))
                .collect::<Vec<_>>()
        })
    };
    let reordered = [
        json!({"role": "user", "content": [result("toolu_b", "abed"), result("toolu_a", "troy")]}),
    ];
    let got = texts(&reordered).unwrap();
    assert!(
        got[0].1.contains("troy") && got[1].1.contains("abed"),
        "{got:?}"
    );

    let renamed = [
        json!({"role": "user", "content": [result("call_1", "troy")]}),
        json!({"role": "user", "content": [result("call_2", "abed"), {"type": "text", "text": "hurry"}]}),
    ];
    let got = texts(&renamed).unwrap();
    assert!(
        got[0].1.contains("troy") && !got[0].1.contains("hurry"),
        "{got:?}"
    );
    assert!(
        got[1].1.contains("abed") && got[1].1.contains("hurry"),
        "{got:?}"
    );

    let missing = [json!({"role": "user", "content": [result("toolu_a", "troy")]})];
    assert!(texts(&missing).is_none());
    let not_an_answer = [json!({"role": "user", "content": "never mind"})];
    assert!(texts(&not_an_answer).is_none());
}

#[test]
fn tool_names_claude_code_would_rewrite_stay_distinct() {
    assert_eq!(
        claude_tool_name("lookup_schedule"),
        "mcp__llamaproxy__lookup_schedule"
    );
    let dotted = claude_tool_name("greendale.enroll");
    let spaced = claude_tool_name("greendale enroll");
    let long = claude_tool_name(&"a".repeat(80));
    for name in [&dotted, &spaced, &long] {
        assert!(name.len() <= TOOL_NAME_LIMIT, "{name}");
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "{name}"
        );
    }
    assert_ne!(dotted, spaced);
    assert_eq!(dotted, claude_tool_name("greendale.enroll"));
}

#[test]
fn claude_code_never_inherits_another_backend_or_this_proxy() {
    let inherited = [
        ("ANTHROPIC_BASE_URL", "http://127.0.0.1:11432"),
        ("anthropic_api_key", "sk-greendale"),
        ("CLAUDE_CODE_USE_BEDROCK", "1"),
        ("DISABLE_TELEMETRY", "0"),
        ("HOME", "/home/annie"),
    ]
    .map(|(key, value)| (OsString::from(key), OsString::from(value)));
    let env = child_env(inherited, Some(512));
    let get = |name: &str| {
        let values: Vec<_> = env
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.to_string_lossy())
            .collect();
        assert!(values.len() <= 1, "{name} is set twice");
        values.first().map(|value| value.to_string())
    };
    for removed in [
        "ANTHROPIC_BASE_URL",
        "anthropic_api_key",
        "CLAUDE_CODE_USE_BEDROCK",
    ] {
        assert_eq!(get(removed), None, "{removed}");
    }
    assert_eq!(get("HOME").as_deref(), Some("/home/annie"));
    assert_eq!(get("DISABLE_TELEMETRY").as_deref(), Some("1"));
    assert_eq!(get("CLAUDE_CODE_MAX_OUTPUT_TOKENS").as_deref(), Some("512"));
}
