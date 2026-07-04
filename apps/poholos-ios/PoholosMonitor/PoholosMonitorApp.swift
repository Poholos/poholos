// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak
//
// App shell: everything lives in PoholosKit (model, screens, scanner,
// engine wrapper), which keeps it compilable and testable on macOS.

import PoholosKit
import SwiftUI

@main
struct PoholosMonitorApp: App {
    @StateObject private var model = MonitorModel(name: NodeIdentity.local())

    var body: some Scene {
        WindowGroup {
            MonitorRootView(model: model)
        }
    }
}
