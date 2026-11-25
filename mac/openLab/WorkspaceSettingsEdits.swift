// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The edits made in the workspace settings window, sent one at a time in the order they were made:
/// each waits for the engine's answer to the one before it, so none overtakes another whatever the
/// engine's delay. An edit decides what it acts on when its turn comes, from what the engine then has.
@MainActor
final class SettingsEdits {
    private let model: WorkspaceSettingsModel
    private var last: Task<Void, Never>?
    private var unanswered = 0
    /// What each control waits to hear about, oldest first (a field's texts; nil for a box or pop-up).
    private var waiting: [String: [String?]] = [:]
    /// The refusals of this burst of edits: an edit of something else that is taken does not hide them.
    private var refused: [(key: String, text: String, serial: Int)] = []
    private var serial = 0
    /// Called after each answer; true when it was a refusal.
    var onAnswer: @MainActor (Bool) -> Void = { _ in }

    init(model: WorkspaceSettingsModel) { self.model = model }

    /// Runs `step` (false: refused, the reason in the model's message) after the edits before it.
    /// `key` names the control it came from, which goes on showing it (`text`) until it is answered.
    func add(_ key: String, showing text: String? = nil, _ step: @escaping @MainActor () async -> Bool) {
        if unanswered == 0 {
            refused = []
            model.message = nil
        }
        unanswered += 1
        waiting[key, default: []].append(text)
        let previous = last
        last = Task { [self] in
            await previous?.value
            let taken = await step()
            let before = refused.count
            refused.removeAll { $0.key == key }
            if !taken {
                serial += 1
                refused.append((key, model.message ?? "", serial))
            }
            if let newest = refused.last {
                model.message = newest.text
            } else if before > 0 {
                model.message = nil
            }
            unanswered -= 1
            waiting[key]?.removeFirst()
            if waiting[key]?.isEmpty == true { waiting[key] = nil }
            onAnswer(!taken)
        }
    }

    /// Whether an edit made with the control `key` has not been answered yet.
    func isWaiting(_ key: String) -> Bool { waiting[key] != nil }

    /// The newest text sent from the field `key` and not answered yet.
    func shown(_ key: String) -> String? { waiting[key]?.last ?? nil }

    /// Waits until every edit made so far, and any made meanwhile, is answered.
    func answered() async { while unanswered > 0, let last { await last.value } }

    /// A point in the edits' order, for `refusedSince`.
    var mark: Int { serial }
    /// Whether an edit answered since `mark` was refused and not made good by a later one.
    func refusedSince(_ mark: Int) -> Bool { refused.contains { $0.serial > mark } }

    /// The page or the window the refusals were about is left.
    func forgetRefusals() {
        refused = []
        model.message = nil
    }
}

/// A text field of the workspace settings window. It shows the engine's value whenever that changes,
/// the caret in it or not, except over text typed in it and not yet committed.
@MainActor
final class SettingsField: NSTextField {
    /// Names the field (its page, row and column), the same each time its table is built.
    let key: String
    /// The table row it stands for, by an identity that never moves to another row; nil outside a table.
    let row: String?
    /// The engine's text for it now; nil when its row is gone.
    var engineText: @MainActor () -> String? = { nil }
    /// Sends what was typed in it; false: refused, the reason in the model's message.
    var send: @MainActor (String) async -> Bool = { _ in true }
    /// The text it was last drawn with or that was last committed.
    private var drawn = ""
    private var marked = false

    init(key: String, row: String? = nil) {
        self.key = key
        self.row = row
        super.init(frame: .zero)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// The caret is in it.
    var isHeld: Bool { currentEditor() != nil }
    private var text: String { currentEditor()?.string ?? stringValue }
    /// Something was typed since it was drawn or committed, also when the text is the old one again.
    private var isTyped: Bool { marked || text != drawn }

    func markTyped() { marked = true }

    /// Whether it can be used (or typed in); set only on a change. AppKit watches a field last typed in, and
    /// setting one of these, even to the same value, orders its window in again when that is closed.
    func allow(enabled: Bool? = nil, editable: Bool? = nil) {
        if let enabled, isEnabled != enabled { isEnabled = enabled }
        if let editable, isEditable != editable { isEditable = editable }
    }

    /// Shows `value` unless something typed here is not committed yet; the caret stays.
    func show(_ value: String) {
        guard !isTyped else { return }
        drawn = value
        if let editor = currentEditor() {
            if editor.string != value { editor.string = value }
        } else if stringValue != value {
            stringValue = value
        }
    }

    /// What was typed, to be committed; nil when nothing was: an untouched field sends nothing.
    func takeTyped() -> String? {
        guard isTyped else { return nil }
        marked = false
        drawn = text
        return drawn
    }

    /// Drops what was typed, and the caret.
    func dropTyped() {
        marked = false
        abortEditing()
        stringValue = drawn
    }
}
