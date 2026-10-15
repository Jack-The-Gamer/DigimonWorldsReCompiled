-- Play Time Tracker: persistent per-mod data example.
local d = dwrc.data
d.frames = d.frames or 0
d.sessions = (d.sessions or 0) + 1

local function minutes() return d.frames / 60 / 60 end

dwrc.log(("Session #%d for %s. Total play time so far: %.1f minutes."):format(d.sessions, dwrc.game, minutes()))

dwrc.on_frame(function(ctx)
  d.frames = d.frames + 1
  if d.frames % 3600 == 0 then      -- about once a minute
    dwrc.save()
  end
end)
