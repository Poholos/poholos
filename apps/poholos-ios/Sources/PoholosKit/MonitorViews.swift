// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak
//
// The monitor's two screens: the message feed and the diagnostics
// screen (radio state, traffic counters, per-source table) — the
// latter doubling as the field tool for mesh debugging. Kept in the
// package so they compile for macOS too (previews, future Mac app).

import CoreBluetooth
import SwiftUI

/// Tab container for the monitor: Feed + Diagnostics. Starts the
/// scanner when it appears.
public struct MonitorRootView: View {
    @ObservedObject private var model: MonitorModel

    public init(model: MonitorModel) {
        self.model = model
    }

    public var body: some View {
        TabView {
            NavigationStack { FeedView(model: model) }
                .tabItem { Label("Feed", systemImage: "dot.radiowaves.left.and.right") }
            NavigationStack { DiagnosticsView(model: model) }
                .tabItem { Label("Diagnostics", systemImage: "waveform.path.ecg") }
        }
        .onAppear(perform: model.start)
    }
}

/// Newest-first list of received messages.
struct FeedView: View {
    @ObservedObject var model: MonitorModel

    var body: some View {
        Group {
            if model.feed.isEmpty {
                emptyState
            } else {
                List(model.feed) { entry in
                    FeedRow(entry: entry)
                }
                .listStyle(.plain)
            }
        }
        .navigationTitle("poholos — \(model.localName)")
        .inlineNavigationTitle()
    }

    private var emptyState: some View {
        VStack(spacing: 8) {
            Image(systemName: "dot.radiowaves.left.and.right")
                .font(.largeTitle)
                .foregroundStyle(.secondary)
            Text(statusLine)
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .padding()
    }

    private var statusLine: String {
        switch model.bluetoothState {
        case .poweredOn: "scanning as \(model.localID) — no frames heard yet"
        case .unauthorized: "Bluetooth access denied — enable it in Settings"
        case .poweredOff: "Bluetooth is off"
        case .unsupported: "Bluetooth LE is unavailable on this device"
        default: model.engineAvailable ? "starting…" : "engine rejected the node name"
        }
    }
}

/// One feed line: marker + route, message text, and time/RSSI/version
/// on the trailing edge.
struct FeedRow: View {
    let entry: FeedEntry

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Text(entry.marker)
                .font(.system(.body, design: .monospaced).bold())
                .foregroundStyle(markerColor)
            VStack(alignment: .leading, spacing: 2) {
                Text(route)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(.secondary)
                Text(entry.text)
                    .font(entry.kind == .undecodable ? .callout.italic() : .body)
                    .foregroundStyle(entry.kind == .undecodable ? .secondary : .primary)
            }
            Spacer(minLength: 8)
            VStack(alignment: .trailing, spacing: 2) {
                Text(entry.receivedAt, format: .dateTime.hour().minute().second())
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                HStack(spacing: 4) {
                    if entry.isExtended {
                        Text("v1")
                            .font(.caption2.bold())
                            .padding(.horizontal, 4)
                            .background(.tint.opacity(0.2), in: Capsule())
                    }
                    Text("\(entry.rssi) dBm")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding(.vertical, 2)
    }

    private var route: String {
        guard let src = entry.src else { return "not a poholos frame" }
        return "\(src) → \(entry.destLabel ?? "?")"
    }

    private var markerColor: Color {
        switch entry.kind {
        case .hearsay: .blue
        case .telegram: .orange
        case .passingThrough: .secondary
        case .undecodable: .secondary
        }
    }
}

/// Radio state, traffic counters, and the per-source reception table.
struct DiagnosticsView: View {
    @ObservedObject var model: MonitorModel

    var body: some View {
        List {
            Section("Node") {
                LabeledContent("Name", value: model.localName)
                LabeledContent("Wire id", value: "\(model.localID)")
            }
            Section {
                LabeledContent("Bluetooth", value: bluetoothLabel)
                LabeledContent("Extended scan", value: extendedScanLabel)
            } header: {
                Text("Radio")
            } footer: {
                Text(
                    "Validation note: iOS can report extended-scan support "
                        + "while extended advertisements still never reach apps; "
                        + "wire v1 reception was not observed on iPhone."
                )
            }
            Section("Traffic") {
                LabeledContent("Messages delivered", value: "\(model.messagesDelivered)")
                LabeledContent("Duplicates suppressed", value: "\(model.duplicatesSuppressed)")
                LabeledContent("Undecodable frames", value: undecodableLabel)
            }
            Section("Sources") {
                if model.sortedSources.isEmpty {
                    Text("No sources heard yet")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(model.sortedSources, id: \.id) { source in
                        SourceRow(id: source.id, stats: source.stats)
                    }
                }
            }
        }
        .navigationTitle("Diagnostics")
        .inlineNavigationTitle()
    }

    private var bluetoothLabel: String {
        switch model.bluetoothState {
        case .poweredOn: "on, scanning"
        case .poweredOff: "off"
        case .unauthorized: "access denied"
        case .unsupported: "unsupported"
        case .resetting: "resetting"
        default: "starting…"
        }
    }

    private var extendedScanLabel: String {
        switch model.extendedScanSupported {
        case true?: "supported (see note)"
        case false?: "not supported"
        case nil: "unavailable on this platform"
        }
    }

    private var undecodableLabel: String {
        model.undecodableLengths.isEmpty
            ? "0"
            : "\(model.undecodableFrames) (lengths: \(model.undecodableLengths.map(String.init).joined(separator: ", ")))"
    }
}

/// One row of the per-source table: id, message count, and live
/// last-seen/RSSI/TTL of the most recent message.
struct SourceRow: View {
    let id: WireID
    let stats: SourceStats

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 2) {
                Text("\(id)")
                    .font(.system(.body, design: .monospaced))
                Text("\(stats.messages) message\(stats.messages == 1 ? "" : "s")")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            VStack(alignment: .trailing, spacing: 2) {
                Text(stats.lastSeen, style: .relative)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text("\(stats.lastRSSI) dBm · ttl \(stats.lastTTL)")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
    }
}

extension View {
    /// `.navigationBarTitleDisplayMode(.inline)` where it exists.
    @ViewBuilder
    fileprivate func inlineNavigationTitle() -> some View {
        #if os(iOS)
            navigationBarTitleDisplayMode(.inline)
        #else
            self
        #endif
    }
}
