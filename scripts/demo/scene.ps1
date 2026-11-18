# The README demo, played by guest.ps1 inside the sandbox. Hints come from the sandbox's
# window set (EX/EP the two Explorer windows, CH Character Map, T Task Manager, R Registry Editor, ...).
# Mark names become screenshot timestamps (see render.ps1).

# 1. Open, type a hint: Task Manager.
Mark open; Tap $VK.RAlt; Wait 2.2
Mark hints; Type-Text 't'; Wait 1.6

# 2. Two Explorer windows share a first letter: one letter narrows, the second picks.
Tap $VK.RAlt; Wait 1.2
Type-Text 'e'; Wait 0.4; Mark narrow; Wait 1.2
Type-Text 'p'; Wait 1.6

# 3. Hold Left Shift for a live preview; arrows move through the windows.
Tap $VK.RAlt; Wait 1
[K]::Down($VK.LShift); Wait 1.4; Mark preview
Tap $VK.Down; Wait 1.2; Tap $VK.Down; Wait 1.2
[K]::Up($VK.LShift); Wait 0.3; Tap $VK.Enter; Wait 1.6

# 4. Right Alt + Right Shift: fuzzy search.
[K]::Down($VK.RAlt); Wait 0.15; Tap $VK.RShift; [K]::Up($VK.RAlt); Wait 1
Type-Text 'char' 220; Wait 0.3; Mark search; Wait 1.3
Tap $VK.Enter; Wait 1.6

# 5. Back to the start for a clean loop.
Tap $VK.RAlt; Wait 1
Type-Text 'ex'; Wait 1.5
