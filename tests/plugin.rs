use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use maki_agent::ToolOutput;
use maki_agent::tools::ToolRegistry;
use maki_lua::{PluginHost, PluginPermissions, UiAction};
use serde_json::json;

const PLUGIN_NAME: &str = "maki-parallel";

fn plugin_host() -> (Arc<ToolRegistry>, PluginHost) {
    let reg = Arc::new(ToolRegistry::new());
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    host.load_package(
        PLUGIN_NAME,
        root,
        PluginPermissions::from_approved(["env", "fs_read", "fs_write"]),
        Default::default(),
    )
    .unwrap();
    (reg, host)
}

fn exec_tool(
    reg: &ToolRegistry,
    host: &PluginHost,
    name: &str,
    input: serde_json::Value,
) -> Result<String, String> {
    let entry = reg
        .get(name)
        .unwrap_or_else(|| panic!("tool {name} not registered"));
    let inv = entry.tool.parse(&input).map_err(|err| err.to_string())?;
    let ctx = maki_agent::tools::test_support::stub_ctx(&maki_agent::AgentMode::Build);
    let ui = host.ui_action_rx();
    smol::block_on(smol::future::race(
        inv.execute(&ctx),
        smol::future::race(
            async {
                while let Ok(action) = ui.recv_async().await {
                    match action {
                        UiAction::Session { reply_tx, .. } | UiAction::Model { reply_tx, .. } => {
                            reply_tx.send(Err("no UI in tests".into())).unwrap();
                        }
                        _ => panic!("unexpected UI action"),
                    }
                }
                panic!("UI channel closed");
            },
            async {
                smol::Timer::after(Duration::from_secs(5)).await;
                panic!("tool {name} did not finish within 5 seconds");
            },
        ),
    ))
    .output
    .map_or_else(Err, |out| match out {
        ToolOutput::Plain(s) => Ok(s.text),
        other => panic!("unexpected output: {other:?}"),
    })
}

#[test]
fn tools_register() {
    let (reg, _host) = plugin_host();
    for tool in ["web_search", "web_fetch", "web_research"] {
        assert!(reg.has(tool), "tool {tool} should register");
    }
}

#[test]
fn web_search_validates_input_before_any_network() {
    let (reg, host) = plugin_host();

    let err = exec_tool(
        &reg,
        &host,
        "web_search",
        json!({ "search_queries": ["a"] }),
    )
    .unwrap_err();
    assert_eq!(
        err,
        "invalid parameter 'objective': required, expected string"
    );

    let err = exec_tool(
        &reg,
        &host,
        "web_search",
        json!({ "objective": "  ", "search_queries": ["a"] }),
    )
    .unwrap_err();
    assert!(err.contains("objective is required"), "got: {err}");

    let err = exec_tool(&reg, &host, "web_search", json!({ "objective": "goal" })).unwrap_err();
    assert!(err.contains("search_queries"), "got: {err}");

    let err = exec_tool(
        &reg,
        &host,
        "web_search",
        json!({ "objective": "goal", "search_queries": ["a", "b", "c", "d"] }),
    )
    .unwrap_err();
    assert!(err.contains("at most 3"), "got: {err}");

    let err = exec_tool(
        &reg,
        &host,
        "web_search",
        json!({ "objective": "goal", "search_queries": ["  "] }),
    )
    .unwrap_err();
    assert!(err.contains("non-empty"), "got: {err}");
}

#[test]
fn web_fetch_validates_input_before_any_network() {
    let (reg, host) = plugin_host();

    let err = exec_tool(&reg, &host, "web_fetch", json!({})).unwrap_err();
    assert!(err.contains("urls"), "got: {err}");

    let err = exec_tool(
        &reg,
        &host,
        "web_fetch",
        json!({ "urls": ["ftp://example.com"] }),
    )
    .unwrap_err();
    assert!(err.contains("HTTP/HTTPS"), "got: {err}");

    let urls: Vec<String> = (0..21)
        .map(|i| format!("https://example.com/{i}"))
        .collect();
    let err = exec_tool(&reg, &host, "web_fetch", json!({ "urls": urls })).unwrap_err();
    assert!(err.contains("at most 20"), "got: {err}");
}

#[test]
fn web_research_validates_input_before_any_network() {
    let (reg, host) = plugin_host();

    let err = exec_tool(&reg, &host, "web_research", json!({})).unwrap_err();
    assert_eq!(err, "invalid parameter 'query': required, expected string");

    let err = exec_tool(&reg, &host, "web_research", json!({ "query": "  " })).unwrap_err();
    assert!(err.contains("query is required"), "got: {err}");

    let err = exec_tool(
        &reg,
        &host,
        "web_research",
        json!({ "query": "q", "effort": "extreme" }),
    )
    .unwrap_err();
    assert!(
        err.contains("expected one of [low, medium, high]"),
        "got: {err}"
    );

    let long = "x".repeat(20_001);
    let err = exec_tool(&reg, &host, "web_research", json!({ "query": long })).unwrap_err();
    assert!(err.contains("20000"), "got: {err}");

    let err = exec_tool(
        &reg,
        &host,
        "web_research",
        json!({ "query": "q", "previous_response_id": "has spaces" }),
    )
    .unwrap_err();
    assert!(err.contains("previous_response_id"), "got: {err}");
}

#[test]
fn tools_fail_fast_without_an_api_key() {
    const CHILD: &str = "MAKI_PARALLEL_TEST_WITHOUT_KEY";
    if std::env::var_os(CHILD).is_none() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("maki-parallel-test-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tools_fail_fast_without_an_api_key",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env_remove("PARALLEL_API_KEY")
            .env("HOME", &dir)
            .env("XDG_CONFIG_HOME", &dir)
            .env("XDG_DATA_HOME", &dir)
            .env("XDG_STATE_HOME", &dir)
            .env("XDG_CACHE_HOME", &dir)
            .status();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(status.unwrap().success(), "isolated API-key test failed");
        return;
    }

    let (reg, host) = plugin_host();
    for (name, input) in [
        (
            "web_search",
            json!({ "objective": "goal", "search_queries": ["rust async runtime"] }),
        ),
        ("web_fetch", json!({ "urls": ["https://example.com"] })),
        ("web_research", json!({ "query": "rust async runtime" })),
    ] {
        let err = exec_tool(&reg, &host, name, input).unwrap_err();
        assert!(err.contains("no Parallel API key"), "{name}: {err}");
    }
}
