-- Example mod script. Everything here is optional.

if dwrc.options.verbose then
  dwrc.log("example-template loaded for " .. dwrc.game)
end

-- Runs after every video frame. `ctx` reads and writes console memory.
dwrc.on_frame(function(ctx)
  -- local value = ctx:read16(0x80012345)
  -- ctx:write16(0x80012345, value)
end)

-- Runs before every frame with the controller state; return a new state.
dwrc.on_input(function(buttons)
  return buttons
end)

-- Runs whenever the CPU is about to execute the instruction at an address.
-- Return true if you changed the program counter (e.g. ctx:ret(v) to replace
-- a whole function with your own logic).
--
-- dwrc.on_exec(0x80012345, function(ctx)
--   local a0 = ctx:reg("a0")
--   return false
-- end)
