use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use maki_agent::ToolOutput;
use maki_agent::tools::ToolRegistry;
use maki_lua::{PluginHost, PluginPermissions, UiAction};
use serde_json::{Value, json};
use tempfile::TempDir;

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
        ToolOutput::Plain(s) | ToolOutput::Markdown(s) => Ok(s.text),
        other => panic!("unexpected output: {other:?}"),
    })
}

struct Fixture {
    reg: Arc<ToolRegistry>,
    host: PluginHost,
    dir: TempDir,
}

impl Fixture {
    fn new(response: Value) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let plugin = dir.path().join("plugin");
        fs::create_dir(&plugin).unwrap();
        for entry in fs::read_dir(root.join("plugin")).unwrap() {
            let entry = entry.unwrap();
            if entry.path().extension().is_some_and(|ext| ext == "lua") {
                fs::copy(entry.path(), plugin.join(entry.file_name())).unwrap();
            }
        }
        fs::copy(root.join("plugin.toml"), dir.path().join("plugin.toml")).unwrap();
        fs::write(
            plugin.join("000_test.lua"),
            format!(
                r#"
local requests = {{}}
maki.net.request = function(url, opts)
  requests[#requests + 1] = {{ url = url, options = opts }}
  return {{ status = 200, body = [====[{response}]====] }}
end
maki.uv.os_getenv = function(name)
  if name == "PARALLEL_API_KEY" then return "fixture-key" end
end
maki.env.state_dir = function() return [====[{state}]====] end
maki.api.register_tool({{
  name = "fixture_inspect",
  description = "Read a saved fixture result or captured requests.",
  schema = {{ type = "object", properties = {{ path = {{ type = "string" }} }} }},
  handler = function(input)
    if input.path then
      local text, err = maki.fs.read(input.path)
      if not text then return {{ llm_output = err, is_error = true }} end
      return text
    end
    return maki.json.encode(requests)
  end,
}})
maki.api.register_tool({{
  name = "fixture_preview",
  description = "Exercise preview limits and concurrent saves.",
  schema = {{ type = "object", properties = {{
    text = {{ type = "string", required = true }},
    navigation = {{ type = "string" }},
    concurrent = {{ type = "boolean" }},
  }} }},
  handler = function(input)
    local function preview() return parallel_core.llm(input.text, input.navigation) end
    if input.concurrent then
      return maki.json.encode(maki.async.gather({{ preview, preview, preview }}))
    end
    return preview()
  end,
}})
"#,
                state = dir.path().join("state").display(),
            ),
        )
        .unwrap();
        let reg = Arc::new(ToolRegistry::new());
        let host = PluginHost::new(Arc::clone(&reg)).unwrap();
        host.load_package(
            PLUGIN_NAME,
            dir.path(),
            PluginPermissions::from_approved(["env", "fs_read", "fs_write", "net"]),
            Default::default(),
        )
        .unwrap();
        Self { reg, host, dir }
    }

    fn call(&self, name: &str, input: Value) -> String {
        exec_tool(&self.reg, &self.host, name, input).unwrap()
    }

    fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    fn recover(&self, output: &str, expected: &str) -> PathBuf {
        assert_limits(output);
        assert!(output.contains("[Preview truncated]"));
        let path = output
            .lines()
            .find_map(|line| line.strip_prefix("Full result saved to: "))
            .expect("preview must identify its saved result");
        let path = PathBuf::from(path);
        assert!(path.starts_with(self.state().join("maki-parallel/results")));
        assert_eq!(fs::read_to_string(&path).unwrap(), expected);
        assert_eq!(
            self.call("fixture_inspect", json!({ "path": path })),
            expected
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        path
    }
}

fn assert_limits(output: &str) {
    assert!(output.len() <= 40_000, "{} bytes", output.len());
    assert!(output.split('\n').count() <= 200, "too many lines");
    assert!(!output.contains('\u{fffd}'), "invalid UTF-8 was replaced");
}

fn search_input() -> Value {
    json!({ "objective": "goal", "search_queries": ["rust async runtime"] })
}

#[test]
fn exact_url_duplicates_merge_metadata_and_excerpts_only_within_a_source() {
    for tool in ["web_search", "web_fetch"] {
        let fixture = Fixture::new(json!({ "results": [
            { "url": "https://example.com/a", "title": "First", "publish_date": "2026-01-01",
              "excerpts": ["shared excerpt", "shared excerpt", "first only"] },
            { "url": "https://example.com/a", "title": "Alternate", "publish_date": "2026-01-02",
              "excerpts": ["shared excerpt", "second only"] },
            { "url": "https://example.com/a", "title": "Alternate", "publish_date": "2026-01-02",
              "excerpts": ["second only"] },
            { "url": "https://example.com/a/", "title": "Other", "excerpts": ["shared excerpt"] }
        ] }));
        let input = if tool == "web_search" {
            search_input()
        } else {
            json!({ "urls": ["https://example.com/a"] })
        };
        let output = fixture.call(tool, input);
        let (first, second) = if tool == "web_search" {
            ("1. ", "2. ")
        } else {
            ("", "")
        };
        assert_eq!(
            output,
            format!(
                "## {first}First\nTitle: Alternate\nhttps://example.com/a\nPublished: 2026-01-01\nPublished: 2026-01-02\n\nshared excerpt\n\nfirst only\n\nsecond only\n\n## {second}Other\nhttps://example.com/a/\n\nshared excerpt\n"
            )
        );
        assert!(!fixture.state().exists());
    }
}

#[test]
fn excerpt_cleanup_preserves_code_and_nonleading_metadata() {
    let code = "````rust\r\nlet x = 1;  \r\n\r\n\r\n```\r\nhttps://example.com/a\r\n````\r\n\n~~~text\nTitle\n\n\n~~~\n\n    indented  \n\n\n\tmore\n";
    let excerpt = format!(
        "\nTitle\n## Title ##\nhttps://example.com/a\n\nBody  \n\n\nAfter\nTitle\nhttps://example.com/a\n\n{code}"
    );
    let fixture = Fixture::new(json!({ "results": [{
        "url": "https://example.com/a", "title": "Title", "excerpts": [excerpt]
    }] }));
    assert_eq!(
        fixture.call("web_search", search_input()),
        format!(
            "## 1. Title\nhttps://example.com/a\n\nBody  \n\n\nAfter\nTitle\nhttps://example.com/a\n\n{code}\n"
        )
    );
    assert!(!fixture.state().exists());
}

#[test]
fn cleanup_collapses_plain_blanks_but_preserves_nested_code() {
    for (excerpt, expected) in [
        ("Title\n\nBody  \n\n\nAfter", "Body  \n\nAfter"),
        (
            "- ```python\n  value = \"\"\"a\n\n\nb\"\"\"\n  ```",
            "- ```python\n  value = \"\"\"a\n\n\nb\"\"\"\n  ```",
        ),
    ] {
        let fixture = Fixture::new(json!({ "results": [{
            "url": "https://example.com/a", "title": "Title", "excerpts": [excerpt]
        }] }));
        assert_eq!(
            fixture.call("web_search", search_input()),
            format!("## 1. Title\nhttps://example.com/a\n\n{expected}\n")
        );
    }
}

#[test]
fn full_content_is_preserved_exactly_and_deduplicated() {
    let code = "Title\r\nhttps://example.com/a\r\n\r\n\r\n```rust\r\n  let x = 1;  \r\n\r\n\r\n```\r\n\n    code  \n\n\n\tend\n\n";
    for content in [code.to_owned(), code.repeat(100)] {
        let fixture = Fixture::new(json!({ "results": [
        { "url": "https://example.com/a", "title": "Title", "full_content": content,
          "excerpts": ["must not replace full content"] },
        { "url": "https://example.com/a", "title": "Title", "full_content": content },
        { "url": "https://example.com/a", "title": "Title", "full_content": "additional document" }
    ] }));
        let output = fixture.call("web_fetch", json!({ "urls": ["https://example.com/a"] }));
        let expected =
            format!("## Title\nhttps://example.com/a\n\n{content}\n\nadditional document\n");
        if content == code {
            assert_eq!(output, expected);
            assert!(!fixture.state().exists());
        } else {
            fixture.recover(&output, &expected);
        }
    }
}

#[test]
fn oversized_search_and_fetch_keep_last_source_and_fetch_errors() {
    let content = (0..300).map(|i| format!("line {i}\n")).collect::<String>();
    for tool in ["web_search", "web_fetch"] {
        let fixture = Fixture::new(json!({
            "results": [
                { "url": "https://example.com/first", "title": "First", "excerpts": [content] },
                { "url": "https://example.com/last", "title": "Last", "publish_date": "2026-02-01",
                  "excerpts": ["last source details"] }
            ],
            "errors": [{ "url": "https://example.com/failed", "message": "unavailable" }]
        }));
        let (input, first, last, error) = if tool == "web_search" {
            (search_input(), "1. ", "2. ", "")
        } else {
            (
                json!({ "urls": ["https://example.com/first", "https://example.com/last"] }),
                "",
                "",
                "\nFailed: https://example.com/failed (unavailable)",
            )
        };
        let output = fixture.call(tool, input);
        assert!(output.starts_with(&format!(
            "## {first}First\nhttps://example.com/first\n\nline 0\n"
        )));
        let navigation = output.split_once("### Source navigation\n").unwrap().1;
        let navigation_error = error.replace('\n', "\n\n");
        assert_eq!(
            navigation,
            format!(
                "## {first}First\nhttps://example.com/first\n\n## {last}Last\nhttps://example.com/last\nPublished: 2026-02-01{navigation_error}"
            )
        );
        assert!(!output.contains("last source details"));
        fixture.recover(&output, &format!("## {first}First\nhttps://example.com/first\n\n{content}\n\n## {last}Last\nhttps://example.com/last\nPublished: 2026-02-01\n\nlast source details\n{error}"));
    }
}

#[test]
fn research_preview_preserves_response_id_and_unique_sources() {
    let answer = "Evidence with citations.\n".repeat(300);
    let fixture = Fixture::new(json!({
        "id": "resp_fixture", "status": "completed", "output": [{
            "type": "message", "content": [{ "type": "output_text", "text": answer,
                "annotations": [
                    { "type": "url_citation", "url": "https://example.com/a", "title": "A" },
                    { "type": "url_citation", "url": "https://example.com/a", "title": "Duplicate" },
                    { "type": "url_citation", "url": "https://example.com/b", "title": "B" }
                ]
            }]
        }]
    }));
    let output = fixture.call("web_research", json!({ "query": "question" }));
    let sources = "### Sources\n- [A](https://example.com/a)\n- [B](https://example.com/b)";
    assert!(output.starts_with("Response ID: resp_fixture\n\nEvidence with citations.\n"));
    assert_eq!(
        output.split_once("### Source navigation\n").unwrap().1,
        format!("Response ID: resp_fixture\n\n{sources}")
    );
    fixture.recover(
        &output,
        &format!("Response ID: resp_fixture\n\n{answer}\n\n{sources}"),
    );
}

#[test]
fn preview_limits_include_navigation_and_notice_and_preserve_utf8() {
    let fixture = Fixture::new(json!({}));
    for text in [
        "x".repeat(40_000),
        "é".repeat(20_000),
        vec!["line"; 200].join("\n"),
    ] {
        assert_eq!(
            fixture.call("fixture_preview", json!({ "text": text })),
            text
        );
        assert!(!fixture.state().exists());
    }
    for text in [
        "x".repeat(40_001),
        "é🦀".repeat(10_000),
        vec!["line"; 201].join("\n"),
    ] {
        for navigation in [String::new(), "source\n".repeat(100), "界".repeat(4_000)] {
            let output = fixture.call(
                "fixture_preview",
                json!({ "text": text, "navigation": navigation }),
            );
            let preview = output.split_once("\n\n[Preview truncated]").unwrap().0;
            assert!(!preview.is_empty());
            assert!(text.starts_with(preview));
            if !navigation.is_empty() {
                assert!(output.ends_with("[Source navigation truncated; see full result.]"));
            }
            fixture.recover(&output, &text);
        }
    }
}

#[test]
fn repeated_and_concurrent_saves_have_unique_recoverable_paths() {
    let fixture = Fixture::new(json!({}));
    let text = "saved content\n".repeat(300);
    let mut paths = std::collections::HashSet::new();
    for _ in 0..2 {
        let output = fixture.call("fixture_preview", json!({ "text": text }));
        assert!(paths.insert(fixture.recover(&output, &text)));
    }
    let outputs: Vec<Value> = serde_json::from_str(&fixture.call(
        "fixture_preview",
        json!({ "text": text, "concurrent": true }),
    ))
    .unwrap();
    assert_eq!(outputs.len(), 3);
    for output in outputs {
        assert_eq!(output["ok"], true, "{output}");
        assert!(paths.insert(fixture.recover(output["value"].as_str().unwrap(), &text)));
    }
}

#[test]
fn failed_save_returns_an_honest_bounded_preview_without_a_path() {
    let fixture = Fixture::new(json!({}));
    fs::write(fixture.state(), "not a directory").unwrap();
    let text = "important evidence\n".repeat(300);
    let output = fixture.call(
        "fixture_preview",
        json!({ "text": text, "navigation": "last source" }),
    );
    assert_limits(&output);
    assert!(output.starts_with("important evidence\n"));
    assert!(output.contains("[Preview truncated]"));
    let error = output
        .split_once("Full result could not be saved: ")
        .unwrap()
        .1
        .lines()
        .next()
        .unwrap();
    assert!(!error.is_empty());
    assert!(!output.contains("Full result saved to:"));
    assert!(!output.contains("Use read"));
    assert!(!output.contains("result.md"));
    assert!(output.ends_with("### Source navigation\nlast source"));
    let output = fixture.call(
        "fixture_preview",
        json!({ "text": text, "navigation": "source\n".repeat(100) }),
    );
    assert_limits(&output);
    assert!(output.ends_with("[Source navigation truncated.]"));
    assert!(!output.contains("see full result"));
    assert_eq!(
        fs::read_to_string(fixture.state()).unwrap(),
        "not a directory"
    );
}

#[test]
fn api_request_defaults_and_optional_parameters_are_unchanged() {
    let fixture = Fixture::new(json!({ "status": "completed", "output": [{
        "type": "message", "content": [{ "type": "output_text", "text": "answer" }]
    }] }));
    let instructions = "Research the user's question using current web sources. Return a direct, evidence-based answer with citations. State uncertainty when the sources do not support a conclusion.";
    let cases = [
        (
            "web_search",
            search_input(),
            "/v1/search",
            30,
            json!({ "objective": "goal", "search_queries": ["rust async runtime"], "mode": "fast" }),
            "No results found.",
        ),
        (
            "web_fetch",
            json!({ "urls": ["https://example.com"] }),
            "/v1/extract",
            30,
            json!({ "urls": ["https://example.com"] }),
            "No content extracted.",
        ),
        (
            "web_fetch",
            json!({ "urls": ["https://example.com"], "objective": "focus", "search_queries": ["query"] }),
            "/v1/extract",
            30,
            json!({ "urls": ["https://example.com"], "objective": "focus", "search_queries": ["query"] }),
            "No content extracted.",
        ),
        (
            "web_research",
            json!({ "query": "question" }),
            "/v1/responses",
            120,
            json!({ "model": "parallel", "input": "question", "instructions": instructions, "reasoning": { "effort": "medium" } }),
            "answer",
        ),
        (
            "web_research",
            json!({ "query": "follow up", "effort": "high", "previous_response_id": "resp_fixture" }),
            "/v1/responses",
            120,
            json!({ "model": "parallel", "input": "follow up", "instructions": instructions, "reasoning": { "effort": "high" }, "previous_response_id": "resp_fixture" }),
            "answer",
        ),
    ];
    for (i, (tool, input, endpoint, timeout, payload, expected)) in cases.into_iter().enumerate() {
        assert_eq!(fixture.call(tool, input), expected);
        let requests: Vec<Value> =
            serde_json::from_str(&fixture.call("fixture_inspect", json!({}))).unwrap();
        assert_eq!(requests.len(), i + 1);
        let request = &requests[i];
        let body: Value =
            serde_json::from_str(request["options"]["body"].as_str().unwrap()).unwrap();
        assert_eq!(body, payload);
        assert_eq!(
            request,
            &json!({
                "url": format!("https://api.parallel.ai{endpoint}"),
                "options": {
                    "method": "POST", "timeout": timeout, "body": request["options"]["body"],
                    "headers": {
                        "Content-Type": "application/json", "x-api-key": "fixture-key",
                        "Authorization": "Bearer fixture-key", "X-Tool-Calling-Package": "maki:maki-parallel"
                    }
                }
            })
        );
    }
    assert!(!fixture.state().exists());
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
