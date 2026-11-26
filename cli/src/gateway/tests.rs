use super::*;

fn config(address: &str, public: Option<&str>) -> Config {
    Config {
        address: address.parse().unwrap(),
        public_host: public.map(|host| authority(host).unwrap()),
        limits: Limits { call: Duration::from_secs(2), result: 150_000 },
    }
}

fn headers(pairs: &[(&str, &str)]) -> Vec<Header> {
    pairs.iter().map(|&(k, v)| Header::from_bytes(k, v).unwrap()).collect()
}

fn contents(response: HttpResponse, status: u16) -> Value {
    assert_eq!(response.status_code().0, status);
    if status == 202 {
        assert_eq!(response.data_length(), Some(0));
        return Value::Null;
    }
    assert_eq!(single_header(response.headers(), "Content-Type"), Ok(Some(JSON)));
    serde_json::from_reader(response.into_reader()).unwrap()
}

fn request(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": "a", "method": method, "params": params })
}

fn post(message: &Value, pairs: &[(&str, &str)], status: u16) -> Value {
    contents(rpc(&headers(pairs), &message.to_string().into_bytes(), config("127.0.0.1:8000", None).limits,
        |_, _| panic!("unexpected proxy call")), status)
}

#[test]
fn listener_is_loopback_and_options_have_safe_defaults() {
    for (address, valid) in [
        ("127.0.0.1:8000", true), ("[::1]:8000", true),
        ("0.0.0.0:8000", false), ("[::]:8000", false), ("192.0.2.1:8000", false), ("localhost:8000", false),
    ] {
        let got = Config::parse(&["--listen".into(), address.into()]);
        assert_eq!(got.is_ok(), valid, "{address}");
        if let Ok(config) = got {
            assert_eq!(config.limits.call, Duration::from_secs(240));
            assert_eq!(config.limits.result, 150_000);
        }
    }
    for (flag, value, valid) in [
        ("--public-host", "mcp.example.test:8443", true),
        ("--public-host", "https://mcp.example.test", false),
        ("--call-timeout-seconds", "1", true),
        ("--call-timeout-seconds", "0", false),
        ("--max-result-chars", "4000", true),
        ("--max-result-chars", "1", false),
    ] {
        let args = ["--listen", "127.0.0.1:8000", flag, value].map(String::from);
        assert_eq!(Config::parse(&args).is_ok(), valid, "{args:?}");
    }
}

#[test]
fn only_local_and_configured_authorities_and_origins_are_allowed() {
    let config = config("127.0.0.1:8000", Some("mcp.example.test"));
    for (host, origin, allowed) in [
        ("127.0.0.1:8000", None, true),
        ("LOCALHOST:8000", Some("http://localhost:8000"), true),
        ("127.0.0.1:8000", Some("https://foreign.example"), false),
        ("127.0.0.1:8000", Some("http://127.0.0.1:80"), false),
        ("127.0.0.1:8000", Some("null"), false),
        ("localhost:8001", None, false),
        ("foreign.example:8000", None, false),
        ("localhost:8000@foreign.example", None, false),
        ("mcp.example.test", Some("https://MCP.EXAMPLE.TEST:443"), true),
        ("mcp.example.test:8443", None, false),
        ("mcp.example.test", Some("https://mcp.example.test/"), false),
        ("mcp.example.test", Some("https://mcp.example.test@foreign.example"), false),
        ("mcp.example.test", Some("https://mcp.example.test, https://foreign.example"), false),
        ("mcp.example.test", Some("http://mcp.example.test"), false),
    ] {
        let mut fields = headers(&[("Host", host)]);
        if let Some(origin) = origin {
            fields.extend(headers(&[("Origin", origin)]));
        }
        assert_eq!(allowed_origin(&fields, config.address, config.public_host.as_ref()), allowed, "{host} {origin:?}");
    }
    for pairs in [
        vec![],
        vec![("Host", "localhost:8000"), ("HOST", "localhost:8000")],
        vec![("Host", "localhost:8000"), ("Origin", "http://localhost:8000"), ("ORIGIN", "http://localhost:8000")],
    ] {
        assert!(!allowed_origin(&headers(&pairs), config.address, config.public_host.as_ref()));
    }
    assert!(allowed_origin(&headers(&[("Host", "[::1]:8000"), ("Origin", "http://[::1]:8000")]),
        "[::1]:8000".parse().unwrap(), None));
}

#[test]
fn endpoint_accepts_json_posts_only() {
    let config = config("127.0.0.1:8000", None);
    for (method, path, media, accept, status) in [
        (Method::Post, "/mcp", "Application/JSON; charset=utf-8", "application/json, text/event-stream", None),
        (Method::Get, "/mcp", "", "", Some(405)),
        (Method::Delete, "/mcp", "", "", Some(405)),
        (Method::Options, "/mcp", "", "", Some(405)),
        (Method::Post, "/other", "application/json", "application/json, text/event-stream", Some(404)),
        (Method::Post, "/mcp", "", "application/json, text/event-stream", Some(415)),
        (Method::Post, "/mcp", "text/plain", "application/json, text/event-stream", Some(415)),
        (Method::Post, "/mcp", "application/json", "application/json", Some(406)),
        (Method::Post, "/mcp", "application/json", "application/json;q=0, text/event-stream", Some(406)),
    ] {
        let headers = headers(&[("Host", "127.0.0.1:8000"), ("Content-Type", media), ("Accept", accept)]);
        let result = preflight(&method, path, &headers, &config);
        assert_eq!(result.as_ref().map(|r| r.status_code().0), status, "{method:?} {path} {media} {accept}");
        if let Some(response) = result.filter(|r| r.status_code().0 == 405) {
            assert_eq!(single_header(response.headers(), "Allow"), Ok(Some("POST")));
        }
    }
}

#[test]
fn handshake_revisions_negotiate_without_a_session() {
    for (requested, chosen) in [(LEGACY[0], LEGACY[0]), (LEGACY[1], LEGACY[1]), ("2025-03-26", LEGACY[0])] {
        let initialize = request("initialize", json!({
            "protocolVersion": requested, "capabilities": {}, "clientInfo": { "name": "test", "version": "1" },
        }));
        assert_eq!(post(&initialize, &[], 200), mcp::initialize(&initialize["id"], chosen));
        let version = [(VERSION_HEADER, chosen)];
        post(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}), &version, 202);
        assert_eq!(post(&request("ping", json!({})), &version, 200)["result"], json!({}));
    }
    assert_eq!(post(&request("ping", json!({})), &[], 400)["error"]["code"], -32020);
    for unsupported in ["2025-03-26", "2030-01-01"] {
        let result = post(&request("ping", json!({})), &[(VERSION_HEADER, unsupported)], 400);
        assert_eq!(result["error"]["code"], -32022);
        assert_eq!(result["error"]["data"], json!({"requested": unsupported, "supported": VERSIONS}));
    }
}

#[test]
fn modern_revision_validates_per_request_metadata() {
    let mut discover = request("server/discover", json!({ "_meta": { META_VERSION: MODERN, META_CLIENT_CAPS: {} } }));
    let pairs = [(VERSION_HEADER, MODERN), ("Mcp-Method", "server/discover")];
    let result = post(&discover, &pairs, 200);
    assert_eq!(result["result"]["supportedVersions"], json!(VERSIONS));
    assert_eq!(result["result"]["resultType"], "complete");
    assert_eq!(result["result"]["_meta"][META_SERVER], mcp::server_info());
    let list = request("tools/list", discover["params"].clone());
    let list = post(&list, &[(VERSION_HEADER, MODERN), ("Mcp-Method", "tools/list")], 200);
    for response in [result, list] {
        assert_eq!(response["result"]["ttlMs"], 0);
        assert_eq!(response["result"]["cacheScope"], "public");
    }
    for fields in [&pairs[..1], &[(VERSION_HEADER, MODERN), ("Mcp-Method", "Server/Discover")], &pairs[1..]] {
        assert_eq!(post(&discover, fields, 400)["error"]["code"], -32020);
    }
    assert_eq!(post(&discover, &[(VERSION_HEADER, LEGACY[0]), ("Mcp-Method", "server/discover")], 400)["error"]["code"], -32020);
    discover["params"]["_meta"][META_VERSION] = json!(LEGACY[0]);
    assert_eq!(post(&discover, &pairs, 400)["error"]["code"], -32020);
    discover["params"]["_meta"] = json!({ META_VERSION: MODERN });
    assert_eq!(post(&discover, &pairs, 400)["error"]["code"], -32602);
    let ping = request("ping", json!({ "_meta": { META_VERSION: MODERN, META_CLIENT_CAPS: {} } }));
    assert_eq!(post(&ping, &[(VERSION_HEADER, MODERN), ("Mcp-Method", "ping")], 404)["error"]["code"], -32601);
    let call = request("tools/call", json!({
        "name": "copy_to", "_meta": { META_VERSION: MODERN, META_CLIENT_CAPS: {} },
    }));
    for (name, status, code) in [("copy_from", 400, -32020), ("=?base64?Y29weV90bw==?=", 200, -32602)] {
        let fields = [(VERSION_HEADER, MODERN), ("Mcp-Method", "tools/call"), ("Mcp-Name", name)];
        assert_eq!(post(&call, &fields, status)["error"]["code"], code);
    }
}

#[test]
fn transports_share_dispatch_and_only_stdio_offers_local_tools() {
    let version = [(VERSION_HEADER, LEGACY[1])];
    let list = request("tools/list", json!({}));
    let mut local = crate::stdio::handle(&list, |_| unreachable!()).unwrap();
    let offered = local["result"]["tools"].as_array_mut().unwrap();
    for name in ["copy_to", "copy_from", "tunnel"] {
        assert!(offered.iter().any(|t| t["name"] == name));
        let call = request("tools/call", json!({"name": name, "arguments": {"computer": "Desk"}}));
        assert_eq!(post(&call, &version, 200)["error"]["code"], -32602);
    }
    offered.retain(|t| !["copy_to", "copy_from", "tunnel"].iter().any(|name| t["name"] == *name));
    assert_eq!(post(&list, &version, 200), local);
    let tool_request = request("tools/call", json!({"name": "run", "arguments": {"computer": "Desk", "command": "pwd"}}));
    let ask = |operation: Value| {
        assert_eq!(operation["computer"], "Desk");
        let forwarded = &operation["request"];
        assert_eq!(forwarded["params"], json!({"name": "run", "arguments": {"command": "pwd"}}));
        Ok(mcp::reply(&forwarded["id"], json!({ "content": [{"type":"text","text":"/"}] })))
    };
    let local = crate::stdio::handle(&tool_request, ask).unwrap();
    let response = rpc(&headers(&version), tool_request.to_string().as_bytes(), config("127.0.0.1:8000", None).limits, move |m, _| ask(m));
    assert_eq!(contents(response, 200), local);
}

#[test]
fn result_size_counts_json_characters_and_rejects_whole_images() {
    for (result, modern) in [
        (json!({"content": [{"type": "text", "text": "界".repeat(600)}]}), false),
        (json!({"content": [{"type": "image", "mimeType": "image/jpeg", "data": "a".repeat(600)}]}), true),
    ] {
        let mut expected = result.clone();
        if modern { complete(&mut expected); }
        let exact = expected.to_string().chars().count();
        for limit in [exact, exact - 1, MIN_RESULT_CHARS] {
            let limits = Limits { call: Duration::from_secs(2), result: limit };
            let result = result.clone();
            let req = request("tools/call", json!({"name": "screenshot", "arguments": {"computer": "Desk"}}));
            let answer = limits.call(req, modern, move |op, _| Ok(mcp::reply(&op["request"]["id"], result.clone())));
            assert!(answer["result"].to_string().chars().count() <= limit);
            if limit == exact {
                assert_eq!(answer["result"], expected);
            } else {
                assert_eq!(answer["result"]["isError"], true);
                assert_eq!(answer["result"]["structuredContent"]["error"], "too_large");
                assert_eq!(answer["result"]["content"].as_array().unwrap().len(), 1);
                assert_eq!(answer["result"]["content"][0]["type"], "text");
            }
        }
    }
}

#[test]
fn call_timeout_includes_waiting_for_the_proxy() {
    let (release, waiting) = mpsc::channel();
    let limits = Limits { call: Duration::from_millis(10), result: MIN_RESULT_CHARS };
    let req = request("tools/call", json!({"name": "computers"}));
    let answer = limits.call(req, false, move |_, _| {
        waiting.recv().unwrap();
        Ok(json!([]))
    });
    release.send(()).unwrap();
    assert_eq!(answer["result"]["structuredContent"]["error"], "timeout");
    assert_eq!(answer["id"], "a");
}
