-- Core plumbing for the Parallel web tools: constants, API key resolution,
-- HTTP helpers, and shared validation. Loaded first (sorted file names); the
-- tools and the /parallel command build on the `parallel_core` table.
--
-- Files in plugin/ are separate chunks sharing one environment, so
-- `parallel_core` is deliberately global for the sibling chunks.

local MAX_LLM_LINES = 200
local MAX_LLM_BYTES = 40000
local MAX_NAVIGATION_LINES = 80
local MAX_NAVIGATION_BYTES = 10000
local MAX_RESEARCH_QUERY_CHARS = 20000
local MAX_RESPONSE_ID_LEN = 512
local MAX_SEARCH_QUERIES = 3
local MAX_FETCH_URLS = 20
local DEFAULT_TIMEOUT_SECONDS = 30
local RESEARCH_TIMEOUT_SECONDS = 120

local API_BASE = "https://api.parallel.ai"
local SEARCH_MODE = "fast"
local RESEARCH_INSTRUCTIONS =
  "Research the user's question using current web sources. Return a direct, evidence-based answer with citations. State uncertainty when the sources do not support a conclusion."

parallel_core = {
  MAX_RESEARCH_QUERY_CHARS = MAX_RESEARCH_QUERY_CHARS,
  MAX_RESPONSE_ID_LEN = MAX_RESPONSE_ID_LEN,
  MAX_FETCH_URLS = MAX_FETCH_URLS,
  SEARCH_MODE = SEARCH_MODE,
  RESEARCH_INSTRUCTIONS = RESEARCH_INSTRUCTIONS,
  RESEARCH_TIMEOUT_SECONDS = RESEARCH_TIMEOUT_SECONDS,
}

function parallel_core.key_file_path()
  local dir = maki.env.state_dir()
  if not dir then
    return nil
  end
  return maki.fs.joinpath(dir, "maki-parallel", "api-key")
end

function parallel_core.is_nonblank(s)
  return type(s) == "string" and s:match("%S") ~= nil
end

function parallel_core.resolve_key()
  local env = maki.uv.os_getenv("PARALLEL_API_KEY")
  if parallel_core.is_nonblank(env) then
    return (env:match("^%s*(.-)%s*$")), "PARALLEL_API_KEY"
  end
  local path = parallel_core.key_file_path()
  if path then
    local content = maki.fs.read(path)
    if content then
      content = content:match("^%s*(.-)%s*$")
      if content ~= "" then
        return content, path
      end
    end
  end
  return nil, "no Parallel API key configured: set PARALLEL_API_KEY or run /parallel login"
end

local function request(method, path, payload, timeout)
  local key, err = parallel_core.resolve_key()
  if not key then
    return nil, err
  end
  local body
  if payload then
    body, err = maki.json.encode(payload)
    if not body then
      return nil, "failed to encode request: " .. err
    end
  end
  local res, req_err = maki.net.request(API_BASE .. path, {
    method = method,
    headers = {
      ["Content-Type"] = "application/json",
      ["x-api-key"] = key,
      Authorization = "Bearer " .. key,
      ["X-Tool-Calling-Package"] = "maki:maki-parallel",
    },
    body = body,
    timeout = timeout or DEFAULT_TIMEOUT_SECONDS,
  })
  if not res then
    return nil, req_err
  end
  local data = maki.json.decode(res.body)
  if res.status >= 400 then
    local msg = res.body:sub(1, 300)
    if type(data) == "table" then
      msg = (type(data.error) == "table" and data.error.message) or data.message or msg
    end
    return nil, ("Parallel API error (HTTP %d): %s"):format(res.status, tostring(msg))
  end
  if not data then
    return nil, "failed to decode response body"
  end
  return data
end

function parallel_core.post(path, payload, timeout)
  return request("POST", path, payload, timeout)
end

function parallel_core.session_id()
  local ok, id = pcall(maki.session.current)
  if ok then
    return id
  end
  return nil
end

function parallel_core.client_model()
  local ok, model = pcall(maki.model.get)
  if ok and type(model) == "table" then
    return model.model
  end
  return nil
end

local function head(text, max_lines, max_bytes)
  local finish = math.min(#text, max_bytes)
  while finish > 0 do
    local next_byte = text:byte(finish + 1)
    if not next_byte or next_byte < 128 or next_byte >= 192 then
      break
    end
    finish = finish - 1
  end
  local start = 1
  for line = 1, max_lines do
    local newline = text:find("\n", start, true)
    if not newline or newline > finish then
      break
    end
    if line == max_lines then
      finish = newline - 1
      break
    end
    start = newline + 1
  end
  return text:sub(1, finish)
end

local function save_result(text)
  local state = maki.env.state_dir()
  if not state then
    return nil, "could not locate the maki state directory"
  end
  local root = maki.fs.joinpath(state, "maki-parallel", "results")
  local ok, err = maki.fs.mkdir(root, { parents = true })
  if not ok then
    return nil, err
  end
  for _ = 1, 3 do
    local dir = maki.fs.joinpath(root, ("%d-%d"):format(os.time(), math.random(1, 2147483647)))
    ok, err = maki.fs.mkdir(dir)
    if ok then
      local path = maki.fs.joinpath(dir, "result.md")
      ok, err = maki.fs.atomic_write(path, text)
      if not ok then
        return nil, err
      end
      return path
    end
  end
  return nil, err
end

function parallel_core.llm(text, navigation)
  if head(text, MAX_LLM_LINES, MAX_LLM_BYTES) == text then
    return text
  end
  local path, err = save_result(text)
  local notice
  if path then
    notice = "Full result saved to: " .. path .. "\nUse read with offset and limit only if omitted details are needed."
  else
    notice = "Full result could not be saved: " .. head(tostring(err):gsub("[\r\n]", " "), 1, 300)
  end
  local nav = head(navigation or "", MAX_NAVIGATION_LINES, MAX_NAVIGATION_BYTES)
  if nav ~= (navigation or "") then
    nav = nav .. (path and "\n[Source navigation truncated; see full result.]" or "\n[Source navigation truncated.]")
  end
  local footer = "\n\n[Preview truncated]\n" .. notice
  if nav ~= "" then
    footer = footer .. "\n\n### Source navigation\n" .. nav
  end
  local _, footer_lines = footer:gsub("\n", "")
  return head(text, MAX_LLM_LINES - footer_lines, MAX_LLM_BYTES - #footer) .. footer
end

function parallel_core.fail(msg)
  return { llm_output = "error: " .. msg, is_error = true }
end

function parallel_core.validate_queries(queries)
  if type(queries) ~= "table" or #queries == 0 then
    return nil, "search_queries must be a non-empty array"
  end
  if #queries > MAX_SEARCH_QUERIES then
    return nil, ("search_queries accepts at most %d queries"):format(MAX_SEARCH_QUERIES)
  end
  for _, q in ipairs(queries) do
    if not parallel_core.is_nonblank(q) then
      return nil, "search_queries entries must be non-empty strings"
    end
  end
  return true
end
