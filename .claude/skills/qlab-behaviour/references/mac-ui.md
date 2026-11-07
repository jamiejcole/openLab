# Mac app behaviour: keys, selection, playhead, typing, lists, windows

## Keys and hotkeys
Q8 | A loaded show whose cue hotkey equals a workspace key (e.g. Space): the workspace key wins, the cue hotkey never fires. QLab shows a yellow clash warning on the field and a non-breaking-warning icon on the cue; the engine side is `HotkeyUsedByWorkspace`, check the UI shows it before claiming it. Two cues with the same hotkey both start, no warning (check K1) | user 2026-10-27, 2026-10-29
Q22 | Every key of the workspace map wins over a cue hotkey, including Load `L` and Pause/Resume `P`; migration leaves `L`/`P` unbound where a cue had them | user 2026-10-29
Q21 | Key map: arrow keys never valid with any modifier; Esc with any modifier refused; refused keys do not appear in the field. No Next/Previous Cue slots: Select Next/Previous are fixed menu items on Command-Down/Up (m: Q38). User: do whatever QLab does | user 2026-10-29
Q71 | Recorder refuses N and Q (edit number/name) and the V/S/L/P map keys as a cue hotkey; a loaded show's cue hotkey on a map key never fires until the workspace moves the key. A hotkey a loaded show already has fires first and opens no editor. Edit keys need the cue list focused, exactly one highlighted cue, not Show Mode; letters typed in a field are text | assumed 2026-11-04
Q78 | A letter held with fn (arrow keys carry fn) is not the plain key: edit keys N and Q ignore it (no chord for it) (m: duplicate row 78) | assumed 2026-11-05
Q80 | A key map naming a key a menu command owns (Cmd-S for Go): the map's key wins at press time (GO sent, Save not run), menu item still shows its key; Warnings tab and log say the menu command lost its key, once per change of the key map | assumed 2026-11-05
Q72 | Esc while a cue-list cell is edited drops the edit (old text shows); Esc is still app-wide panic and also panics | assumed 2026-11-04
Q68 | Every inspector text field: Return commits and the cue list takes focus (Space fires GO); Esc drops the text and does the same; Tab moves to the next field | manual p.36-37 for some fields; rest assumed 2026-11-04
Q62 | Masthead notes: Return and Tab end typing (notes save, focus to the cue list); only Shift-Return types a new line; Option-Return does not | user 2026-11-04
Q69 | Inspector Notes behave as the masthead: Return/Tab end it, Shift-Return newline; Esc drops text | user 2026-11-04 (Esc/Tab assumed)
Q49 | Tools > Load to Time with a negative value loads that far back from the end (QLab). Not built: engine `load` takes 0 or more seconds, a negative time is refused | manual p.149, p.589 (engine follow-up needed)

## Playhead, selection, typing (one policy, rows Q59-Q70)
Q11 | Selecting several cues by hand moves the playhead to the most recently selected one | user 2026-10-29
Q39 | After a paste, playhead goes to the last pasted cue in row order | assumed
Q40 | After Select All or a range selection the playhead is the table's selected row (usually the last), so GO fires that cue | assumed
Q59 | Playhead locked to selection (new workspace default): when the engine moves the playhead the selection moves to the standby cue, opening groups above it; past the end nothing is selected and the inspector shows no cue. A multi-selection that already holds the playhead's cue is kept | assumed 2026-11-01
Q60 | While any text in the window is being typed in (inspector, notes) the selection/inspector do not follow a playhead move (Space types, no GO); when typing ends (Return, Esc, Tab, click away) the selection follows. A click that ends it lands where clicked | assumed 2026-11-01
Q61 | Same for masthead notes: the masthead keeps the cue typed for (text, focus, no hotkeys) until typing ends, then notes save to that cue and the masthead shows the new standby | assumed 2026-11-04
Q63 | Mid-gesture on the selected cue (fader or matrix drag, waveform handle, held checkbox, open popup, hotkey recording) or any menu open: selection and inspector stay until the mouse is up/menu closed; the whole edit lands on the cue it started on, then selection follows. A lost release ends on the next left press, a key with the button up, or the view going away; dragged value still goes to its cue (Show Mode alone gives an uncommitted drag up) | assumed 2026-11-04
Q64 | A contextual menu acts on the cues it was opened on (right-clicked cue or the selection holding it) even if the selection moves meanwhile | assumed 2026-11-04
Q65 | When the selection moves while focus is in the inspector on a non-text control: a control the new cue also shows keeps focus; otherwise the cue list takes it (arrow keys move the selection) | assumed 2026-11-04
Q66 | Switching cue list in the sidebar selects that list's playhead cue (opening groups above); no playhead (empty/past end) shows nothing selected | assumed 2026-11-04
Q67 | A workspace opening, or a snapshot after reconnect/resync, selects the playhead's cue at once (typing and gestures still hold it); nothing is sent to the engine, in Show Mode too | assumed 2026-11-04
Q70 | Text refused in an inspector field as typing ends with a playhead move held behind it: the message stays through that one follow; the next change of cue clears it | assumed 2026-11-04

## Cue list and editing
Q32 | Active Cues row for a paused cue keeps frozen elapsed/remaining and bar, led by the state name ("Paused ..."); stopping row likewise | assumed
Q18 | Pre-Wait, Action, Post-Wait columns count down while running; a running group shows elapsed. Engine sends elapsed and remaining; UI picks display | assumed (asked 2026-10-29, unanswered); manual: waits count down
Q20 | Duration for an Audio cue whose last slice/file loops until devamped: engine sends one-pass length and no `duration`; column decides display | assumed (asked 2026-10-29, unanswered)
Q56 | Duration cell of a running cue: outlined and filled green from the left to the played fraction, still reading the duration; paused: bar stays, yellow; ends: bar goes. Unknown-end cues (looping, group) draw no bar | assumed 2026-11-01 (user's own observation of QLab)
Q57 | Duration header chevron flips a running cue's cell to time left; Pre-Wait/Post-Wait columns keep countdown, flipping to elapsed over total | assumed 2026-11-01
Q42 | New cue list from the sidebar button: "Cue List N" (N = position), after the last list, shown at once | assumed
Q46 | New cue or paste with a Group selected goes inside as its last child (expanded or not; list opens it); with a cue inside a group selected, after that cue in the group | assumed 2026-10-31
Q47 | New Group from several selected cues: placed where the first selected (list order) was, cues in list order, mixed parents all taken in (a selected cue inside a selected group goes with it); one undo step; busy cues moved, never stopped | assumed 2026-10-31
Q73 | Unnamed cue acting on another (Fade, Start, Stop, Pause, Load, Reset, Go To, Arm, Disarm) shows kind plus target name ("Fade and stop" for a stop-fade), as a name hint (nothing stored, a rename of the target is followed); app-only, the engine's `display_name` (OSC) lacks it; unnamed target uses file name then kind | manual p.93, 126 + assumed 2026-11-04
Q74 | Duplicate number on a cell edit: sheet with the inspector's message, cell shows old number; a cue deleted while its cell is open drops the edit and tells the operator | assumed 2026-11-04
Q117 | Clicking the file button in a cue's Target column does not select the row; selection and playhead stay, file goes to the clicked cue | assumed 2026-11-06
Q118 | Target column of a cue whose target was deleted shows the yellow `?` of a cue with no target | assumed 2026-11-06
Q120 | Jump to Targets with several selected cues in different lists: first selected target's list is shown and its targets become the selection (others left out); a target in another list than the cue's is followed; playhead goes to the last target in row order | assumed 2026-11-07
Q58 | Waveform: horizontal trackpad scroll and Shift+wheel pan (stop at file ends); pinch, Option+vertical scroll, plain wheel zoom about the pointer; zoom buttons and Cmd-= / Cmd-- zoom about the middle | manual p.180, 73 + assumed 2026-11-01
Q79 | Notes the engine refuses or cannot receive (or whose cue was deleted): a small floating panel names the cue, says notes not saved, with Copy Text; nothing is held, Save/Show Mode/Quit are not blocked. No panel in Show Mode (footer and log carry it). Every path also appends the full text to `unsaved-notes.txt` in the app support folder; Quit with engine lost appends notes being typed without prompt | assumed 2026-11-05
Q31 | Pre-show check: Window item on Shift-Cmd-W (QLab's Workspace Status key; its other tabs not built) listing broken cues and warnings by kind; choosing an entry selects and reveals the cue (playhead unmoved) and, in Edit Mode, shows the inspector Basics tab for file/target problems. QLab has no separate pre-show check | assumed (asked 2026-10-29, unanswered); manual p.70-73, 164-165

## Show Mode and windows
Q52 | Show Mode hides cue toolbar and inspector; their footer toggles and View items stay but greyed showing hidden; leaving Show Mode restores the operator's earlier choice. Masthead and sidebar toggles unaffected | assumed 2026-10-31
Q53 | GO button is 12% x 13% of the window in Show Mode (7% x 9% in Edit) with a floor of 80 x 56 pt; standby line one line, number dot name, ellipsis when long; notes 1.7 times taller | assumed 2026-10-31
Q95 | Window frame, split positions, shown areas, sidebar tab are remembered per workspace file path on this machine (UserDefaults, never in the show file); Show Mode's hidden areas are not saved as layout; Show Mode itself is not restored by layout. Window layout and recent files live in the app, not engine `settings.json` | assumed 2026-11-05
Q96 | A saved frame off every screen or larger than the screen opens fully on one screen (shrunk, moved inside, or centred on main); damaged layout data is ignored field by field silently | assumed 2026-11-05
