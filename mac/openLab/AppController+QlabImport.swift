// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import UniformTypeIdentifiers

/// File > New Workspace from QLab File…: choose the `.qlab5` file, ask the engine what it converts to,
/// ask the operator what to do with the cues openLab lacks and where the outputs go, then open the
/// converted workspace as a new, unsaved one and show its notes.
extension AppController {
    /// Any workspace in Show Mode: the import's windows are modal and would hold GO.
    var qlabImportBlocked: Bool { controllers.values.contains(where: \.isShowMode) }

    func newWorkspaceFromQlab() async {
        await afterLaunch()
        guard !qlabImportBlocked else { return beep() }
        guard let url = pickQlabURL() else { return }
        let report: QlabReport
        switch await session.inspectQlab(path: url.path) {
        case .success(let r): report = r
        case .failure(let e):
            gatedAlerts.showError(qlabRefusal(e))
            return
        }
        let local = await session.localOutputs()
        guard let choice = chooseQlabImport(report, local.devices, local.displays) else { return }
        let result = await opened { await self.session.importQlab(path: url.path, choice: choice) }
        guard case .success = result, !qlabImportBlocked else { return }
        showQlabNotes(
            QlabNotesInfo(
                fileName: url.lastPathComponent, report: report, converted: choice.converted,
                placeholders: choice.placeholders, deleted: choice.deleted))
    }

    /// An alert's words for a file the engine refused, in our own words.
    func qlabRefusal(_ e: EngineError) -> String {
        if case .engine(let code, _) = e {
            switch code {
            case .notQlabWorkspace: return loc("qlab.error.notQlab")
            case .qlabTooOld: return loc("qlab.error.tooOld")
            case .qlabTooNew: return loc("qlab.error.tooNew")
            case .badShowFile: return loc("qlab.error.damaged")
            default: break
            }
        }
        return String(format: loc("qlab.error.generic"), describe(e))
    }

    static func qlabPanel() -> URL? {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.message = loc("qlab.panel.message")
        panel.prompt = loc("qlab.panel.prompt")
        if let type = UTType(filenameExtension: "qlab5") { panel.allowedContentTypes = [type] }
        return StandBack.runModal(panel) == .OK ? panel.url : nil
    }

    static func runQlabImport(_ report: QlabReport, _ devices: [DeviceEntry], _ displays: [StageDisplay])
        -> QlabImportChoice?
    {
        QlabImportController(report: report, devices: devices, displays: displays).run()
    }

    func runQlabNotes(_ info: QlabNotesInfo) {
        let sheet = QlabNotesController(info: info)
        sheet.onError = { [weak self] in self?.gatedAlerts.showError($0) }
        sheet.run()
    }
}
