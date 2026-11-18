// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// The pages of the workspace settings (`id`s are the window's tab identifiers) and the categories
/// they are listed under, as in QLab 5's settings window: a list of categories on the left and the
/// pages of the chosen one on the right (UI map `settings.*`).
enum SettingsPage: String, CaseIterable {
    case general, keys, audio, video, network, osc

    var titleKey: String {
        switch self {
        case .general: return "settings.page.general"
        case .keys: return "settings.page.keys"
        case .audio: return "settings.page.audio"
        case .video: return "settings.page.video"
        case .network: return "settings.page.network"
        case .osc: return "settings.page.osc"
        }
    }

    var category: SettingsCategory {
        switch self {
        case .general: return .general
        case .keys: return .controls
        case .audio: return .audio
        case .video: return .video
        case .network, .osc: return .network
        }
    }
}

enum SettingsCategory: String, CaseIterable {
    case general, controls, audio, video, network

    var titleKey: String { "settings.category.\(rawValue)" }

    /// The category's pages, the first shown when it is picked.
    var pages: [SettingsPage] { SettingsPage.allCases.filter { $0.category == self } }
}
