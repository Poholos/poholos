// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak
//
// App shell around PoholosKit: a placeholder feed list proving the
// scanner -> engine -> UI pipeline end to end. The real two-screen UI
// (feed + diagnostics) replaces FeedScreen in the next workstream.

import CoreBluetooth
import PoholosKit
import SwiftUI

@main
struct PoholosMonitorApp: App {
    var body: some Scene {
        WindowGroup {
            FeedScreen()
        }
    }
}

struct FeedScreen: View {
    @StateObject private var feed = FeedModel()

    var body: some View {
        NavigationStack {
            List {
                Section(feed.status) {
                    ForEach(feed.lines) { line in
                        Text(line.text)
                            .font(.system(.footnote, design: .monospaced))
                    }
                }
            }
            .navigationTitle(feed.title)
            .navigationBarTitleDisplayMode(.inline)
        }
        .onAppear(perform: feed.start)
    }
}

/// Bridges scanner callbacks (background queue) onto the main actor.
@MainActor
final class FeedModel: ObservableObject {
    struct Line: Identifiable {
        let id = UUID()
        let text: String
    }

    @Published private(set) var lines: [Line] = []
    @Published private(set) var status = "starting…"
    private(set) var title = "poholos"

    private var scanner: MeshScanner?

    func start() {
        guard scanner == nil else { return }
        guard let scanner = MeshScanner(name: NodeIdentity.local()) else {
            status = "engine rejected the node name"
            return
        }
        title = "poholos — \(scanner.router.name)"

        let localID = scanner.router.localID
        scanner.onStateChange = { [weak self] state in
            // The wire-v1 platform question, displayed where the
            // validation runs happen: does this device's CoreBluetooth
            // claim BLE 5 extended-scan support? Queried at powered-on,
            // when the answer is authoritative — the API is unreliable
            // before Bluetooth is up and authorized.
            let extendedScan =
                switch MeshScanner.supportsExtendedScan {
                case true?: "ext-scan: yes"
                case false?: "ext-scan: NO"
                case nil: "ext-scan: n/a"
                }
            let text =
                state == .poweredOn
                ? "scanning as \(localID) · \(extendedScan)" : "bluetooth unavailable"
            Task { @MainActor in self?.status = text }
        }
        scanner.onEvent = { [weak self] event in
            guard let text = Self.line(for: event, localID: localID) else { return }
            Task { @MainActor in self?.append(text) }
        }
        // The platform-validation signal: the ext-adv POC transmitter is
        // not a poholos frame, so it surfaces here — the length tells
        // whether iOS exposed the full extended payload.
        scanner.onUndecodableFrame = { [weak self] length, rssi in
            Task { @MainActor in
                self?.append("? \(length)-byte undecodable frame under 0xF10C (\(rssi) dBm)")
            }
        }

        self.scanner = scanner
        scanner.start()
    }

    private func append(_ text: String) {
        lines.insert(Line(text: text), at: 0)
        // Placeholder cap; the real feed store arrives with the UI.
        if lines.count > 200 {
            lines.removeLast()
        }
    }

    /// One feed line, in the firmware displays' visual language:
    /// * hearsay, @ telegram for this node, ~ passing through.
    private static nonisolated func line(
        for event: MeshScanner.Event, localID: WireID
    ) -> String? {
        let marker: String
        let message: MeshMessage
        switch event.action {
        case .deliver(let m), .deliverAndForward(let m, _):
            marker = m.dest == nil ? "*" : "@"
            message = m
        case .forward(let m, _):
            marker = "~"
            message = m
        case .ignore:
            return nil
        }
        let dest =
            switch message.dest {
            case nil: "all"
            case localID: "you"
            case .some(let other): "\(other)"
            }
        let version = event.isExtended ? "v1" : "v0"
        return "\(marker) \(message.src) → \(dest)  \(message.text)"
            + "  (\(event.rssi) dBm, \(version))"
    }
}
