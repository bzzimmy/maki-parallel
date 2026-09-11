# Contributing

Small, focused changes to maki-parallel. Install and usage live in the
[README](README.md); this file covers how to build, check, and submit work.

## Setup

- Rust toolchain with `cargo`
- [`just`](https://github.com/casey/just)
- `cargo-nextest` for the test suite
- `stylua` for Lua formatting

```sh
git clone https://github.com/bzzimmy/maki-parallel.git
cd maki-parallel
```

## Checks

CI runs all of these from the repository root. Run the same commands before
opening a pull request:

```sh
just check      # cargo check --tests
just lint       # cargo clippy --tests -- -D warnings
just test       # cargo nextest run
just fmt-lua    # stylua plugin/ (rewrites files)
```

For Rust formatting, run `cargo fmt`. CI checks `cargo fmt --all -- --check`
and `stylua --check plugin/`, so commit formatted code.

Tests live in `tests/` and load the plugin through the real `maki-lua` host
with `PluginHost::load_package`, passing the repository root. Assert
Lua-visible effects such as callback output and mailbox messages, not just
files. Dev-dependencies pin a maki revision; move the pin when the host
changes something the plugin uses.

## Context efficiency

The plugin exists to keep web output small in the model context. Preserve
that in every change:

- Tool output is truncated by `parallel_core.llm` with `MAX_LLM_LINES` and
  `MAX_LLM_BYTES`. Do not raise the limits to fit a case; render less.
- Tool names, descriptions, schemas, and error strings are sent on every
  request. Keep them short and state each fact once.
- Prefer one batched call over several calls, and deduplicate URLs and
  sources before returning.

## Code guidelines

`AGENTS.md` holds the conventions: `MAX_*` constants at the top of each file,
`(value, err)` returns instead of throws, `{ llm_output = msg, is_error = true }`
for tool failures, and `plugin.toml` permissions matched to the calls the code
makes.

## Pull requests

- One change per pull request, described in the body along with the checks you
  ran.
- Write prose in the maki docs voice: plain words, no em-dashes, no
  contractions.
