# Native Shuttle refocus and relaunch corrections

The exact `d7220989d62d` Loom archive produced a correlated 19-byte inline preview
at stream sequence 7 while all four approved Gemma runs were still open. It then
passed idle/resume, fan cycling, Option-Right acceptance and exact Option-Left
reversal. This exercises the compatible-fan driver correction natively.

The next step failed after Shuttle accepted 16 bytes. The native driver's
`focusWritingSurface` always set and checked the original 103-unit caret, even
though the current manuscript ended at 119. That moved the caret before the
accepted word, invalidated the family, and let Option-Left perform ordinary
word navigation to 99. The generation guard correctly rejected the resulting
second four-run family. The product was not changed to accommodate this driver.

Refocus now takes the exact expected persisted bytes. It refuses a changed
manuscript and derives the UTF-16 caret from those bytes. Initial focus receives
the initial manuscript; post-Shuttle focus receives the verified accepted
manuscript. The compiled contract includes accepted-text, Unicode, invalid-text
and changed-prefix cases. Replacing its calculation with the old original-caret
decision must fail the accepted-word regression.

Mom's earlier native smoke positively joined its owned workers, then the next
launch encountered the same exited PID still registered with LaunchServices.
The existing 30-second quit wait now also requires the owned registration to
disappear. A lookup failure is an error, never proof of absence. The prelaunch
check still refuses any other process sharing the bundle identifier. Shell
regressions cover delayed registration removal, timeout and lookup failure.

All failed observations and SQLite snapshots remain in the local evidence
directory. These corrections still require a new packaged native run; neither
the individual passing stages nor the compiled contracts certify the complete
Visual/Source/Ghost/Loompad journey.
