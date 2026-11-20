# 787 progress (paused, WIP)

## Done
- FooterBar.swift edited (NOT built, NOT tested): statusButton, overrideBadge, popover, seenXruns, presentation removed; new `alarmButton` (AX.deviceAlarm, hidden unless a device is lost/failed), `FooterBar.alarmText(status)`, `show(status:connected:)` now only shows/clears the alarm. Click goes to onWarnings (Status > Warnings).

## Half done / known breakage
- StatusPresentation.swift still exists: delete it. Strings needed but not yet added to Localizable.strings: footer.alarm, footer.alarm.deviceLost/deviceFailed/outputLost/outputFailed, footer.alarm.tooltip. AX.deviceAlarm not yet added in Localization.swift (remove AX.statusLabel, statusDetails, overrideBadge).
- WorkspaceWindowController.swift:786 `footer.popover?.close()` must go.
- Tests referencing removed footer parts (to rewrite/delete): AppControllerTests (~420-450 popover, held count 8/9), FooterWarningsTests (badge, statusButton, 2 lost-device tests -> assert alarm + Warnings row), StatusLogTests (StatusPresentation tests, ~30-250), MainWindowTests (154, 360-361, 2045-2050), OutputOverrideTests (badge, popover), EngineLifecycleTests 1123-1144, InspectorTests 1092/1100. ScenarioRunner.swift:1028, 1811-1812, 2057 print "footer: presentation.summary".

## Left (plan)
1. Finish footer part (above), build, run FooterWarningsTests/StatusLogTests/OutputOverrideTests; commit.
2. Sidebar: remove 3rd tab (SidebarViewController warningsTable/Count/relink/WarningCell moves to the Warnings page), WindowLayout.sidebarTabCount 3->2, View > Warnings (menu.view.warnings, QLab has it) repointed to showStatus(.warnings), WorkspaceWindowController.showWarnings/onRelink/relinkLocked wiring, AX.sidebarWarnings/warningsCount/warningRelink; scenarios clicking sidebar-tabs at ~0.84-0.85 (device-lost-paused*, warnings-device-lost-go*, icons-states-and-controls*, slice-skip, two-devices) -> use {"focus":"status"} or click footer-warnings; SidebarUITests lines 32-71.
3. Remove Window > Show Log (MainMenu logItem, AppDelegate.showLog, AppController+Transport showLog, WorkspaceWindowController.showLog, string menu.window.log; tests StatusLogTests ~584-594, StatusWindowTests:49, AppControllerTests:434).
4. Warnings page live: rewrite PreshowWindowController to list WarningRows.make(store) (pushed from WorkspaceWindowController.syncFooterWarnings), keep hide/show hidden/inspect/relink; delete Check Again, summary, PreshowReport.swift (PreshowRows), preshow.* section/summary strings, AX.preshowCheckAgain/Summary; StatusWindowController.present no runCheck; ScenarioRunner:922; rewrite PreshowTests, WarningsTests (side.warningRows -> page), PreshowUITests.
5. Delete 4 settings.audio.device* strings (device, playingOn, noStatus, deviceProblem), unused status.* strings, icons statusNormal/statusWaiting if unused.
6. Skill mac-app (lines 21,22,71,94,102 etc., <=500 chars), qlab-behaviour row Q601 fix + rows Q740-Q749, engine-lifecycle mac-supervision.md mentions; scenarios; ui-shots to /private/tmp/claude-501/-Users-jamie-openLab/3fc17084-d5da-4a35-ba64-a516f8047103/scratchpad/qa-extras/; make lint.
- Engine/protocol left alone; unused after this: SystemStatus cpu/engine_load/xruns/disk/logDropped/device format fields, `preshow_check` request (WorkspaceStore.preshowCheck, PreshowReport decode, ClientProtocol.preshowCheck).
- Findings: a lost device already is a Warnings row via workspace `warnings` notice (d:device_lost:<name>); verify in a test.

## Tests run
None yet.
