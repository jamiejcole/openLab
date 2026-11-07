---
name: mac-app
description: "Load when changing or reviewing the macOS app structure in mac/openLab: windows, EngineClient, WorkspaceStore, cue list, inspector, meters, Show Mode notices and alerts, or the app build and signing setup. Stack rules are in CLAUDE.md."
---
# macOS app structure

Must not break: view controllers never touch the socket; Show Mode never opens an alert or sheet except the modal cases listed below; meters publish nothing while no `MeterView` is on screen.

Stack rules (Swift 6, AppKit, macOS 12.0, no SwiftUI/Observation/third-party packages, SF Symbols, `Localizable.strings`) are in CLAUDE.md.

## Chain
- `EngineClient` (socket, reconnect, message codec; `URLSessionWebSocketTask`, `Codable`) -> `WorkspaceStore` (mirrored model and runtime state, publishes changes) -> AppKit view controllers.
- View controllers never touch the socket.
- One window per open workspace, each with its own `EngineClient` bound to that doc (`hello { workspace }`) and its own `WorkspaceStore`.
- An app-level session holds the unbound connection and owns launch, New/Open/recovery flows, the workspace list, recent files and the quit walk (one save prompt per dirty workspace).
- The front window decides which doc GO, Pause All and other window-scoped keys address. Esc sends the app-scope `panic`.
- Closing a workspace while cues play asks first. Closing the last one shows a launcher window.
- No `NSDocument` subclass. UI is built in code: no storyboards or XIBs, main menu included.
- Main window layout follows QLab 5 (UI-07): toolbar and GO area on top, cue list (`NSOutlineView`, view-based, cell reuse for 2,000+ cues) centre, sidebar, tabbed inspector below. Dark appearance by default.

## Meters (AUD-19, NFR-26)
- `WorkspaceStore.meters` (`MeterBank`) max-folds each `meters` push into preallocated per-source slots and publishes nothing.
- A `MeterView` registers only while on screen (in a window, not hidden or occluded). The bank's 60 Hz timer runs only while a view is registered. A push with no view registered is dropped.
- The bank holds views weakly; a view leaves it when its window closes or hides. Levels fall at a fixed dB/s and only changed views redraw.

## Seeing the app
- `make ui-shots SCENARIO=mac/scenarios/<name>.json [OUT=<dir>]` drives the real workspace window on the real engine (stepped virtual clock) and writes `<OUT>/<nn>-<name>.png` per shot plus `steps.log` (each step and what it hit). OUT defaults to `build/ui-shots/<name>`. It builds for testing only when `mac/` app, test, test-show or project files or the engine changed since the last build; editing a scenario needs no build.
- The runner is `ScenarioRunner` in `mac/openLabTests` (test-only, never in a Release product). The window is never ordered on screen or made key: it takes no focus and never moves the pointer, so it runs while the user works. It takes the same machine-wide lock as `make prepush`'s UI tests (`lockf /tmp/openlab-xcuitest.lock`), so it waits for another mac test run. Presses go to the view under the point with the window's press hook; a standard control in a window that is not key ignores `mouseDown`, so its cell tracks the press instead. A pop-up button is refused (its menu needs real input). Keys go through the app's own key handling (`AppController.handleKeyDown`, Esc's panic sent to the engine), then a key equivalent, then the window. Cue hotkeys and menu-bar commands are not reached. GO moves the selection to the next cue, so select the playing cue again to inspect it.
- Shots render the window's frame view offscreen at 2x with `displayIgnoringOpacity` (the shared `renderView`): every view draws with the clip AppKit gives it on screen, so a view that does not clip to its bounds shows its overdraw. Forced to draw, a tab view paints a blank white bar over its tabs; `renderView` paints the tab buttons again over it. `cacheDisplay` skips layer-backed subviews; do not use it for pictures.
- Scenario: `show` (a `mac/TestShows` name, or a `.openlab` path relative to the scenario, opened in place and never saved), optional `media` (`{"media/a.wav": seconds}`: a test tone written into a test show's copy first), `window` (`{"width", "height"}`, points, default 1200x800), `appearance` (`dark` default, or `light`), `about` (free text), `steps`.
- Steps, one action key each: `select` (cue number, else name); `click` / `double_click` / `drag` (an accessibility identifier; `at`, `from`, `to` as `[x, y]` fractions of the control from its top-left; `modifiers`); `type` (identifier, `text`, `enter` default true); `key` (one character or space, escape, return, tab, delete, left, right, up, down; `modifiers`: shift, control, option, command); `scroll` (identifier; `dx`, `dy`, `precise` default true, `modifiers`, `at`); `magnify` (identifier, `by`); `advance` (seconds of device time, run in tenths); `resize` (`[width, height]`); `tab` (inspector tab identifier); `shot` (name).
- A step that cannot be done stops the run with a reason and a non-zero exit: an unknown or repeated identifier (the identifiers shown are listed), a disabled control, a press that lands on another view, a cue or tab not found. Reusable scenarios live in `mac/scenarios/`.

## Build
- `.xcodeproj` is generated by XcodeGen from `mac/project.yml`, never committed.
- The engine is built universal (arm64 + x86_64 via `lipo`) by a build-phase script calling `cargo`.
- The app is not sandboxed (it spawns the engine and opens arbitrary media paths). Hardened runtime and notarisation need an Apple Developer account; ad-hoc signed until then.
