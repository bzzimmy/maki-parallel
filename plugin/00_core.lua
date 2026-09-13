-- Core plumbing for the Parallel web tools: constants, API key resolution,
-- HTTP helpers, and shared validation. Loaded first (sorted file names); the
-- tools and the /parallel command build on the `parallel_core` table.
--
-- Files in plugin/ are separate chunks sharing one environment, so
-- `parallel_core` is deliberately global for the sibling chunks.

local MAX_LLM_LINES = 200
local MAX_LLM_BYTES = 40000
local MAX_SEARCH_QUERIES = 3
local MAX_FETCH_URLS = 20
local DEFAULT_TIMEOUT_SECONDS = 30

local API_BASE = "https://api.parallel.ai"
local SEARCH_MODE = "fast"

local truncate = require("maki.truncate")

parallel_core = {
  MAX_FETCH_URLS = MAX_FETCH_URLS,
  SEARCH_MODE = SEARCH_MODE,
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

function parallel_core.llm(text)
  return truncate(text, MAX_LLM_LINES, MAX_LLM_BYTES)
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
