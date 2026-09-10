<h1><img src="assets/parallel-logo-white.svg" alt="Parallel" width="40" align="absmiddle"> maki-parallel</h1>

Context-efficient web access for [maki](https://github.com/tontinton/maki),
backed by the [Parallel API](https://parallel.ai). Three tools replace the
built-in web tools with retrieval ranked and compressed for agents: search
returns focused excerpts, fetch returns clean markdown, research returns a
cited answer.

## Requirements

- maki 0.4.12 or newer
- A Parallel API key from [platform.parallel.ai](https://platform.parallel.ai)

## Install

Add the package in `~/.config/maki/init.lua`:

```lua
maki.pack.add({
  { src = "https://github.com/bzzimmy/maki-parallel", version = "main" },
})
```

Then run `/reload` in maki.

## Setup

Set the `PARALLEL_API_KEY` environment variable, or run:

```
/parallel login
```

Login with no argument opens a paste window, keeping the key out of chat
history; `/parallel login <key>` also works. The key is stored in the maki
state directory with mode 0600, and the environment variable wins over it.

- `/parallel logout` removes the stored key.
- `/parallel doctor` shows the key source and verifies it with a live call.

## Tools

| Tool | Parallel API | Use it for |
| --- | --- | --- |
| `web_search` | Search (`/v1/search`) | Source discovery: titles, URLs, dates, focused excerpts. Always runs in `fast` mode. |
| `web_fetch` | Extract (`/v1/extract`) | Reading known URLs as markdown. Handles JavaScript-heavy pages and PDFs. Batch up to 20 URLs per call. |
| `web_research` | Responses (`/v1/responses`) | A complete, cited answer to a self-contained question. `effort` sets depth; follow-ups pass `previous_response_id`. |

Disable maki's built-in web tools in your config so the agent picks these.

## Cost

Search API list price per 1,000 queries, in USD. Lower is better. Source:
Artificial Analysis, 10 Sep 2026.

![Search API cost per 1,000 queries: Parallel Search (fast) $1.00, Firecrawl Search $1.98, Perplexity Search (low), Parallel Search (advanced), Brave Search (LLM context), You.com Search (highlights), and Parallel Search (basic) $5.00, Exa Search (auto) $7.00](assets/search-api-cost.png)

`web_search` always runs Parallel Search in `fast` mode, the cheapest tier
shown. The chart covers the Search API only; `web_fetch` and `web_research`
call Extract and Responses, which price separately.

## Permissions

The package requests exactly what it calls: `net` for API requests, `env` to
read `PARALLEL_API_KEY`, `fs_read` and `fs_write` for the stored key.

## Development

Clone the repository and run the checks from its root. The test command requires
`cargo-nextest`.

```sh
git clone https://github.com/bzzimmy/maki-parallel.git
cd maki-parallel
just check && just lint && just test
```
