use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use maki_agent::ToolOutput;
use maki_agent::tools::ToolRegistry;
use maki_lua::{Key, PluginHost, PluginPermissions, UiAction, WinCommand, WinEvent};
use serde_json::json;

const PLUGIN_NAME: &str = "maki-parallel";
const TIMEOUT: Duration = Duration::from_secs(5);

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
        ToolOutput::Plain(s) | ToolOutput::Markdown(s) => Ok(s.text),
        other => panic!("unexpected output: {other:?}"),
    })
}

fn response_host(body: serde_json::Value) -> (Arc<ToolRegistry>, PluginHost) {
    let reg = Arc::new(ToolRegistry::new());
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let body = serde_json::to_string(&body.to_string()).unwrap();
    host.load_source(
        PLUGIN_NAME,
        &format!(
            "{}\n\
             parallel_core.resolve_key = function() return 'test-key' end\n\
             maki.net.request = function() return {{ status = 200, body = {body} }} end\n\
             {}",
            include_str!("../plugin/00_core.lua"),
            include_str!("../plugin/10_tools.lua"),
        ),
    )
    .unwrap();
    (reg, host)
}

#[test]
fn web_search_renders_null_result_fields() {
    let (reg, host) = response_host(json!({
        "results": [{
            "title": null,
            "url": "https://example.com",
            "publish_date": null,
            "excerpts": ["Search excerpt", null]
        }]
    }));
    let output = exec_tool(
        &reg,
        &host,
        "web_search",
        json!({ "objective": "goal", "search_queries": ["query"] }),
    )
    .unwrap();
    assert_eq!(
        output,
        "## 1. https://example.com\nhttps://example.com\n\nSearch excerpt\n"
    );
}

#[test]
fn web_fetch_renders_null_result_fields() {
    let (reg, host) = response_host(json!({
        "results": [{
            "title": null,
            "url": "https://example.com",
            "publish_date": null,
            "excerpts": ["Fetched excerpt", null],
            "full_content": null
        }],
        "errors": null
    }));
    let output = exec_tool(
        &reg,
        &host,
        "web_fetch",
        json!({ "urls": ["https://example.com"] }),
    )
    .unwrap();
    assert_eq!(
        output,
        "## https://example.com\nhttps://example.com\n\nFetched excerpt\n"
    );
}

#[test]
fn tools_register() {
    let (reg, _host) = plugin_host();
    for tool in ["web_search", "web_fetch"] {
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

/// Reruns {test} in a child process whose home and XDG dirs are a fresh temp
/// dir, without `PARALLEL_API_KEY`, so no real key is read or written.
/// Returns that dir in the child, and `None` in the parent once the child passed.
fn isolated(test: &str) -> Option<PathBuf> {
    const CHILD_DIR: &str = "MAKI_PARALLEL_TEST_DIR";
    if let Some(dir) = std::env::var_os(CHILD_DIR) {
        return Some(dir.into());
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("maki-parallel-test-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(CHILD_DIR, &dir)
        .env_remove("PARALLEL_API_KEY")
        .env("HOME", &dir)
        .env("XDG_CONFIG_HOME", &dir)
        .env("XDG_DATA_HOME", &dir)
        .env("XDG_STATE_HOME", &dir)
        .env("XDG_CACHE_HOME", &dir)
        .status();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(status.unwrap().success(), "isolated test {test} failed");
    None
}

fn key_file(dir: &Path) -> PathBuf {
    dir.join("maki").join("maki-parallel").join("api-key")
}

/// Runs `/parallel login` without a key and returns the paste window's channels.
fn open_login(host: &PluginHost) -> (flume::Sender<WinEvent>, flume::Receiver<WinCommand>) {
    let ui = host.ui_action_rx();
    host.event_handle().run_command(
        Arc::from(PLUGIN_NAME),
        Arc::from("/parallel"),
        "login".into(),
        0,
    );
    loop {
        if let UiAction::OpenWin {
            event_tx, cmd_rx, ..
        } = ui.recv_timeout(TIMEOUT).expect("login window did not open")
        {
            return (event_tx, cmd_rx);
        }
    }
}

fn press(win: &flume::Sender<WinEvent>, key: &str) {
    let key = Key::parse(key).unwrap();
    win.send(WinEvent::Key { key }).unwrap();
}

fn wait_closed(win: &flume::Receiver<WinCommand>) {
    while !matches!(
        win.recv_timeout(TIMEOUT)
            .expect("login window did not close"),
        WinCommand::Close
    ) {}
}

fn flashes(host: &PluginHost) -> Vec<String> {
    host.ui_action_rx()
        .try_iter()
        .filter_map(|action| match action {
            UiAction::Flash(msg) => Some(msg),
            _ => None,
        })
        .collect()
}

#[test]
fn login_window_cancels_on_esc_and_ctrl_c() {
    let Some(dir) = isolated("login_window_cancels_on_esc_and_ctrl_c") else {
        return;
    };
    let (_reg, host) = plugin_host();
    for key in ["<Esc>", "<C-c>"] {
        let (events, cmds) = open_login(&host);
        press(&events, "x");
        press(&events, key);
        wait_closed(&cmds);
    }
    assert_eq!(flashes(&host), Vec::<String>::new());
    assert!(!key_file(&dir).exists());
}

#[test]
fn login_window_stores_pasted_key_on_enter() {
    let Some(dir) = isolated("login_window_stores_pasted_key_on_enter") else {
        return;
    };
    let (_reg, host) = plugin_host();
    let (events, cmds) = open_login(&host);
    let text = " pk-test\n".to_owned();
    events.send(WinEvent::Paste { text }).unwrap();
    press(&events, "<CR>");
    wait_closed(&cmds);
    assert_eq!(flashes(&host), ["parallel: API key stored (paste)"]);
    assert_eq!(
        std::fs::read_to_string(key_file(&dir)).unwrap(),
        "pk-test\n"
    );
}

#[test]
fn tools_fail_fast_without_an_api_key() {
    if isolated("tools_fail_fast_without_an_api_key").is_none() {
        return;
    }

    let (reg, host) = plugin_host();
    for (name, input) in [
        (
            "web_search",
            json!({ "objective": "goal", "search_queries": ["rust async runtime"] }),
        ),
        ("web_fetch", json!({ "urls": ["https://example.com"] })),
    ] {
        let err = exec_tool(&reg, &host, name, input).unwrap_err();
        assert!(err.contains("no Parallel API key"), "{name}: {err}");
    }
}
