-- patrak: logs real watch time and finished lectures for the patrak learning tracker.
-- Installed by `patrak setup`. Appends one JSON line per minute of playback and one when a file ends.

local utils = require "mp.utils"

local data_home = os.getenv("XDG_DATA_HOME") or (os.getenv("HOME") .. "/.local/share")
local log_path = data_home .. "/patrak/mpv-events.jsonl"

local FLUSH_EVERY = 60 -- seconds of playback between log lines

local current = nil -- { path, duration, pos, watched }
local last_tick = nil

local function absolute(path)
    if not path or path:find("^%a[%w+.-]*://") then
        return nil -- streams and URLs are not tracked
    end
    local ok, normalized = pcall(mp.command_native, { "normalize-path", path })
    if ok and normalized then
        return normalized
    end
    return utils.join_path(utils.getcwd(), path)
end

local function emit(eof)
    if not current then
        return
    end
    local f = io.open(log_path, "a")
    if not f then
        return
    end
    f:write(utils.format_json({
        ts = os.time(),
        path = current.path,
        pos = current.pos,
        duration = current.duration,
        watched = current.watched,
        eof = eof,
    }) .. "\n")
    f:close()
    current.watched = 0
end

mp.register_event("file-loaded", function()
    local path = absolute(mp.get_property("path"))
    current = path and { path = path, duration = mp.get_property_number("duration"), pos = 0, watched = 0 } or nil
    last_tick = mp.get_time()
end)

mp.observe_property("time-pos", "number", function(_, pos)
    if current and pos then
        current.pos = pos
    end
end)

mp.observe_property("duration", "number", function(_, duration)
    if current and duration then
        current.duration = duration
    end
end)

mp.add_periodic_timer(1, function()
    local now = mp.get_time()
    local playing = current and not mp.get_property_bool("pause") and not mp.get_property_bool("core-idle")
    if playing and last_tick then
        -- Capped so a suspended laptop doesn't count as watching.
        current.watched = current.watched + math.min(now - last_tick, 2)
        if current.watched >= FLUSH_EVERY then
            emit(false)
        end
    end
    last_tick = now
end)

mp.register_event("end-file", function(event)
    emit(event.reason == "eof")
    current = nil
end)
