-- Turbo Text: hold a button to auto-tap Cross (or Circle).
local hold = dwrc.buttons[dwrc.options.hold]
local tap = dwrc.buttons[dwrc.options.tap]
local rate = math.max(2, math.floor(dwrc.options.rate))
local n = 0

dwrc.on_input(function(b)
  if (b & hold) ~= 0 then
    n = n + 1
    b = b & ~hold                     -- hide the turbo button from the game
    if (n % rate) < (rate // 2) then
      b = b | tap
    else
      b = b & ~tap
    end
  else
    n = 0
  end
  return b
end)

dwrc.log("Turbo Text ready: hold " .. dwrc.options.hold .. " to tap " .. dwrc.options.tap)
