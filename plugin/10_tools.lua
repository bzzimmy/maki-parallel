-- The three agent-facing tools: web_search, web_fetch, web_research.
-- Names mirror Parallel's own agent tooling; disable maki's built-in web
-- tools in config to avoid overlap.

local core = parallel_core

local function single_line(text)
  return (text:gsub("\r", "\\r"):gsub("\n", "\\n"))
end

local function append_unique(values, value)
  if value == nil then
    return
  end
  for _, existing in ipairs(values) do
    if existing == value then
      return
    end
  end
  values[#values + 1] = value
end

local function clean_excerpt(text, record)
  local lines = {}
  for line in (text .. "\n"):gmatch("(.-)\n") do
    lines[#lines + 1] = line
  end
  local first = 1
  while first <= #lines do
    local line = lines[first]:gsub("\r$", "")
    if line:match("^    ") or line:match("^ *\t") or line:match("^ ? ? ?```") or line:match("^ ? ? ?~~~") then
      break
    end
    local hashes, title = line:match("^ ? ? ?(#+)[ \t]+(.-)[ \t]*$")
    local heading = hashes and #hashes <= 6 and title:gsub("[ \t]+#+$", "")
    if line == "" or line == record.title or line == record.url or (heading and heading == record.title) then
      first = first + 1
    else
      break
    end
  end
  local content = table.concat(lines, "\n", first)
  if content:find("```", 1, true) or content:find("~~~", 1, true) or content:find("<", 1, true) then
    return content
  end
  for i = first, #lines do
    if lines[i]:match("^[ \t]+%S") then
      return content
    end
  end
  return (content:gsub("\n\n\n+", "\n\n"))
end

local function group_results(results, fetch)
  local groups, by_url = {}, {}
  for _, r in ipairs(results or {}) do
    local group = core.is_nonblank(r.url) and by_url[r.url]
    if not group then
      group = { url = r.url, titles = {}, dates = {}, chunks = {}, excerpts = {}, documents = {} }
      groups[#groups + 1] = group
      if core.is_nonblank(r.url) then
        by_url[r.url] = group
      end
    end
    append_unique(group.titles, r.title)
    append_unique(group.dates, r.publish_date)
    local full = fetch and core.is_nonblank(r.full_content)
    local seen = full and group.documents or group.excerpts
    for _, chunk in ipairs(full and { r.full_content } or r.excerpts or {}) do
      if not seen[chunk] then
        seen[chunk] = true
        local text = full and chunk or clean_excerpt(chunk, r)
        if text ~= "" then
          group.chunks[#group.chunks + 1] = text
        end
      end
    end
  end
  return groups
end

local function render_results(data, fetch)
  local out, navigation = {}, {}
  for i, group in ipairs(group_results(data.results, fetch)) do
    local metadata = {
      "## " .. (fetch and "" or (i .. ". ")) .. single_line(group.titles[1] or group.url or "untitled"),
    }
    for j = 2, #group.titles do
      metadata[#metadata + 1] = "Title: " .. single_line(group.titles[j])
    end
    if group.url then
      metadata[#metadata + 1] = single_line(group.url)
    end
    for _, date in ipairs(group.dates) do
      metadata[#metadata + 1] = "Published: " .. single_line(date)
    end
    local heading = table.concat(metadata, "\n")
    navigation[#navigation + 1] = heading
    out[#out + 1] = heading
    for _, chunk in ipairs(group.chunks) do
      out[#out + 1] = ""
      out[#out + 1] = chunk
    end
    out[#out + 1] = ""
  end
  if fetch then
    for _, e in ipairs(data.errors or {}) do
      local message = ("Failed: %s (%s)"):format(
        single_line(e.url or "unknown URL"),
        single_line(e.message or "unknown error")
      )
      out[#out + 1] = message
      navigation[#navigation + 1] = message
    end
  end
  if #out == 0 then
    return fetch and "No content extracted." or "No results found.", ""
  end
  return table.concat(out, "\n"), table.concat(navigation, "\n\n")
end

local function render_search(data)
  return render_results(data, false)
end

local function render_fetch(data)
  return render_results(data, true)
end

local function render_research(data)
  if data.status ~= "completed" then
    return nil, "research did not complete (status: " .. tostring(data.status) .. ")"
  end
  local texts, sources, seen = {}, {}, {}
  for _, item in ipairs(data.output or {}) do
    if item.type == "message" then
      for _, part in ipairs(item.content or {}) do
        if part.type == "output_text" and type(part.text) == "string" then
          texts[#texts + 1] = part.text
          for _, a in ipairs(part.annotations or {}) do
            if a.type == "url_citation" and core.is_nonblank(a.url) and not seen[a.url] then
              seen[a.url] = true
              sources[#sources + 1] = { url = a.url, title = a.title }
            end
          end
        end
      end
    end
  end
  local answer = table.concat(texts, "\n\n")
  if not answer:match("%S") then
    return nil, "Parallel returned an empty research response"
  end
  local out, navigation = {}, {}
  if core.is_nonblank(data.id) then
    local response_id = "Response ID: " .. single_line(data.id)
    out[#out + 1] = response_id .. "\n"
    navigation[#navigation + 1] = response_id
  end
  out[#out + 1] = answer
  if #sources > 0 then
    out[#out + 1] = "\n### Sources"
    navigation[#navigation + 1] = "\n### Sources"
    for _, s in ipairs(sources) do
      local source = ("- [%s](%s)"):format(
        single_line(core.is_nonblank(s.title) and s.title or s.url),
        single_line(s.url)
      )
      out[#out + 1] = source
      navigation[#navigation + 1] = source
    end
  end
  return table.concat(out, "\n"), nil, table.concat(navigation, "\n")
end

maki.api.register_tool({
  name = "web_search",
  kind = "search",
  description = [[Search the web using Parallel's Search API. Prefer this over generic browser-like search tools for current web results. Returns result titles, URLs, publish dates, and focused excerpts.

Use for source discovery and raw excerpts when you need to investigate sources yourself. For a complete synthesized answer instead, use web_research.]],
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
  description = [[Fetch and extract readable content from URLs using Parallel's Extract API. Prefer this over raw HTML fetch tools for readable content extraction; it handles JavaScript-heavy pages and PDFs and returns clean markdown.

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

maki.api.register_tool({
  name = "web_research",
  kind = "search",
  description = [[Answer a complete question using Parallel's Responses API. Delegates multi-step web research and returns a synthesized answer with sources, not raw search results.

Use this tool sparingly. Prefer web_search for source discovery and web_fetch for reading known URLs. Use web_research only when the task needs deeper, multi-step investigation or a synthesized answer across several sources.

Start with the complete, self-contained question including constraints; research cannot see this conversation or local files. For a focused follow-up on the same investigation, pass the latest returned Response ID as previous_response_id; omit it for unrelated questions.]],
  schema = {
    type = "object",
    properties = {
      query = {
        type = "string",
        description = "The research question and constraints, self-contained. A follow-up with previous_response_id may refer to prior research.",
        required = true,
      },
      effort = {
        type = "string",
        enum = { "low", "medium", "high" },
        description = "Research depth: low for focused lookups (~5-10s), medium for general research (default, ~15-20s), high for extensive research (~30-60s).",
      },
      previous_response_id = {
        type = "string",
        description = "The latest Response ID returned by web_research for this investigation. Reuses prior research context for a follow-up. Omit for a new or unrelated question; never invent an ID.",
      },
    },
  },
  timeout = core.RESEARCH_TIMEOUT_SECONDS,
  handler = function(input)
    if not core.is_nonblank(input.query) then
      return core.fail("query is required")
    end
    if #input.query > core.MAX_RESEARCH_QUERY_CHARS then
      return core.fail(("query exceeds %d characters"):format(core.MAX_RESEARCH_QUERY_CHARS))
    end
    local effort = input.effort or "medium"
    if effort ~= "low" and effort ~= "medium" and effort ~= "high" then
      return core.fail("effort must be low, medium, or high")
    end
    if input.previous_response_id ~= nil then
      local id = input.previous_response_id
      if not core.is_nonblank(id) or #id > core.MAX_RESPONSE_ID_LEN or id:match("[%s%c]") then
        return core.fail("previous_response_id must be a non-empty ID without whitespace")
      end
    end
    local data, req_err = core.post("/v1/responses", {
      model = "parallel",
      input = input.query,
      instructions = core.RESEARCH_INSTRUCTIONS,
      reasoning = { effort = effort },
      previous_response_id = input.previous_response_id,
    }, core.RESEARCH_TIMEOUT_SECONDS)
    if not data then
      return core.fail(req_err)
    end
    local rendered, render_err, navigation = render_research(data)
    if not rendered then
      return core.fail(render_err)
    end
    return { llm_output = core.llm(rendered, navigation), format = "markdown" }
  end,
})
