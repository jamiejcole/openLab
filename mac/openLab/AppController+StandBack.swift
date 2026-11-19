// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// #708: the operator is never locked out of their own display by a stage covering it. A question
/// of the app's stands the stage there back (`StandBack`); so does an engine that stops answering
/// (the app's black at once here, the stage host's window by its own silence limit).
extension AppController {
    /// How often the engine is asked whether it answers while the app's black covers the display
    /// with the menu bar; one not answered by the next ask counts as not answering.
    static let engineWatchEvery = 1.0

    func questionsChanged(_ asking: Bool) {
        backstop.asking = asking
        let client = session.appClient
        Task { _ = await client.send { .request(id: $0, .setStandBack(on: asking)) } }
    }

    /// Watches the engine's answers while the app's black covers the display with the menu bar
    /// (`covered`), and stops when it no longer does.
    func watchEngine(_ covered: Bool) {
        engineWatch?.cancel()
        engineWatch = nil
        engineAsked = false
        backstop.engineSilent = false
        guard covered else { return }
        let scheduler = engineWatchScheduler
        engineWatch = Task { [weak self] in
            while !Task.isCancelled {
                guard (try? await scheduler.sleep(seconds: Self.engineWatchEvery)) != nil, !Task.isCancelled,
                    let self
                else { return }
                self.askEngine()
            }
        }
    }

    /// One look: an ask still unanswered (or no connection) means the engine is not answering, and
    /// the app's black on the operator's display stands back until an answer comes.
    private func askEngine() {
        if engineAsked || session.connectionState != .connected { backstop.engineSilent = true }
        guard !engineAsked else { return }
        engineAsked = true
        let client = session.appClient
        let asking = StandBack.shared.isAsking
        Task { [weak self] in
            let answer = await client.send { .request(id: $0, .setStandBack(on: asking)) }
            guard let self else { return }
            self.engineAsked = false
            if case .success = answer { self.backstop.engineSilent = false }
        }
    }
}
