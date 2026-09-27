-- The two agent-facing tools: web_search, web_fetch.
-- Names mirror Parallel's own agent tooling; disable maki's built-in web
-- tools in config to avoid overlap.

local core = parallel_core

local function render_search(data)
  local out = {}
  for i, r in ipairs(data.results or {}) do
    out[#out + 1] = ("## %d. %s"):format(i, r.title or r.url or "untitled")
    if r.url then
      out[#out + 1] = r.url
    end
    if r.publish_date then
      out[#out + 1] = "Published: " .. r.publish_date
    end
    for _, excerpt in ipairs(r.excerpts or {}) do
      out[#out + 1] = ""
      out[#out + 1] = excerpt
    end
    out[#out + 1] = ""
  end
  if #out == 0 then
    return "No results found."
  end
  return table.concat(out, "\n")
end

local function render_fetch(data)
  local out = {}
  for _, r in ipairs(data.results or {}) do
    out[#out + 1] = "## " .. (r.title or r.url or "untitled")
    if r.url then
      out[#out + 1] = r.url
    end
    if r.publish_date then
      out[#out + 1] = "Published: " .. r.publish_date
    end
    local chunks = r.excerpts or {}
    if core.is_nonblank(r.full_content) then
      chunks = { r.full_content }
    end
    for _, chunk in ipairs(chunks) do
      out[#out + 1] = ""
      out[#out + 1] = chunk
    end
    out[#out + 1] = ""
  end
  for _, e in ipairs(data.errors or {}) do
    out[#out + 1] = ("Failed: %s (%s)"):format(e.url or "unknown URL", e.message or "unknown error")
  end
  if #out == 0 then
    return "No content extracted."
  end
  return table.concat(out, "\n")
end

maki.api.register_tool({
  name = "web_search",
  kind = "search",
  description = [[Search the web for current information. Returns result titles, URLs, publish dates, and focused excerpts.

Use for source discovery and raw excerpts, then read the promising sources with web_fetch.]],
  schema = {
    type = "object",
    properties = {
      objective = {
        type = "string",
        description = "Natural-language description of the underlying question or goal driving the search. Self-contained, with enough context to understand the intent.",
        required = true,
      },
      search_queries = {
        type = "array",
        items = { type = "string" },
        description = "1-3 concise keyword search queries, 3-6 words each. Provide 2-3 for best results.",
        required = true,
      },
    },
  },
  handler = function(input)
    if not core.is_nonblank(input.objective) then
      return core.fail("objective is required")
    end
    local ok, err = core.validate_queries(input.search_queries)
    if not ok then
      return core.fail(err)
    end
    local data, req_err = core.post("/v1/search", {
      objective = input.objective,
      search_queries = input.search_queries,
      mode = core.SEARCH_MODE,
      session_id = core.session_id(),
      client_model = core.client_model(),
    })
    if not data then
      return core.fail(req_err)
    end
    return { llm_output = core.llm(render_search(data)), format = "markdown" }
  end,
})

maki.api.register_tool({
  name = "web_fetch",
  kind = "read",
  description = [[Fetch URLs and return their readable content as clean markdown. Handles JavaScript-heavy pages and PDFs.

Batch multiple URLs into one call instead of many single-URL calls. Pass objective and search_queries to focus excerpts on what matters; omit them for whole-page content.]],
  schema = {
    type = "object",
    properties = {
      urls = {
        type = "array",
        items = { type = "string" },
        description = "List of URLs to extract content from. Must be valid HTTP/HTTPS URLs. Up to 20 URLs.",
        required = true,
      },
      objective = {
        type = "string",
        description = "Natural-language description of the goal driving the request. Focuses excerpts on the most relevant content.",
      },
      search_queries = {
        type = "array",
        items = { type = "string" },
        description = "Optional keyword queries used together with objective to focus excerpts.",
      },
    },
  },
  handler = function(input)
    if type(input.urls) ~= "table" or #input.urls == 0 then
      return core.fail("urls must be a non-empty array")
    end
    if #input.urls > core.MAX_FETCH_URLS then
      return core.fail(("urls accepts at most %d entries"):format(core.MAX_FETCH_URLS))
    end
    for _, url in ipairs(input.urls) do
      if type(url) ~= "string" or not url:match("^https?://") then
        return core.fail("urls entries must be HTTP/HTTPS URLs, got: " .. tostring(url))
      end
    end
    if input.search_queries ~= nil then
      local ok, err = core.validate_queries(input.search_queries)
      if not ok then
        return core.fail(err)
      end
    end
    local data, req_err = core.post("/v1/extract", {
      urls = input.urls,
      objective = input.objective,
      search_queries = input.search_queries,
      session_id = core.session_id(),
      client_model = core.client_model(),
    })
    if not data then
      return core.fail(req_err)
    end
    return { llm_output = core.llm(render_fetch(data)), format = "markdown" }
  end,
})
