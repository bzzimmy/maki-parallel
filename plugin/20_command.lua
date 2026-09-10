-- /parallel command: login, logout, doctor.

local core = parallel_core
local TextInput = require("maki.text_input")

local function save_key(key)
  local path = core.key_file_path()
  if not path then
    return nil, "could not locate the maki state directory"
  end
  local ok, err = maki.fs.mkdir(maki.fs.dirname(path), { parents = true })
  if not ok then
    return nil, err
  end
  return maki.fs.atomic_write(path, key .. "\n")
end

local function store_key(key, source)
  local ok, err = save_key(key)
  if not ok then
    maki.notify("failed to store key: " .. tostring(err), "error", { title = "parallel" })
    return
  end
  maki.notify("API key stored (" .. source .. ")", "info", { title = "parallel" })
end

local function login(key)
  if core.is_nonblank(key) then
    store_key(key:match("^%s*(.-)%s*$"), "argument")
    return
  end
  local input = TextInput.new()
  local buf = maki.ui.buf()
  local win = maki.ui.open_win(buf, {
    title = "Parallel login",
    width = "60%",
    height = 3,
    needs_input = true,
    footer = { { "enter", "save" }, { "esc", "cancel" } },
  })
  maki.async.run(function()
    local function render()
      local r = input:render("key: ", 5, win.width)
      buf:set_lines(r.lines)
    end
    render()
    while true do
      local ev = win:recv()
      if not ev then
        break
      end
      if ev.type == "key" then
        if ev.key == "esc" then
          break
        elseif ev.key == "enter" then
          local value = input:value():gsub("%s", "")
          if value ~= "" then
            store_key(value, "paste")
          end
          break
        else
          input:handle_key(ev.key)
          render()
        end
      elseif ev.type == "paste" then
        input:insert_text(ev.text)
        render()
      end
    end
    if win:is_open() then
      win:close()
    end
  end)
end

local function logout()
  local path = core.key_file_path()
  if not path then
    maki.notify("could not locate the maki state directory", "error", { title = "parallel" })
    return
  end
  local ok, err = maki.fs.rm(path, { force = true })
  if not ok then
    maki.notify("failed to remove key: " .. tostring(err), "error", { title = "parallel" })
    return
  end
  maki.notify("stored API key removed (PARALLEL_API_KEY is unaffected)", "info", { title = "parallel" })
end

local function doctor()
  maki.async.run(function()
    local key, source = core.resolve_key()
    if not key then
      maki.notify(source, "error", { title = "parallel doctor" })
      return
    end
    local data, err = core.post("/v1/search", {
      objective = "Connectivity check for the Parallel API",
      search_queries = { "parallel web systems" },
      mode = "turbo",
      max_chars_total = 500,
    }, 15)
    if not data then
      maki.notify("key from " .. source .. " failed verification: " .. err, "error", { title = "parallel doctor" })
      return
    end
    maki.notify(
      ("key from %s verified, search returned %d results"):format(source, #(data.results or {})),
      "info",
      { title = "parallel doctor" }
    )
  end)
end

maki.api.register_command({
  name = "parallel",
  description = "Parallel API access: /parallel login [key] | logout | doctor",
  nargs = "*",
  handler = function(opts)
    local sub = opts.fargs[1]
    if sub == "login" then
      login(opts.fargs[2])
    elseif sub == "logout" then
      logout()
    elseif sub == "doctor" then
      doctor()
    else
      maki.ui.flash("usage: /parallel login [key] | logout | doctor")
    end
  end,
})
