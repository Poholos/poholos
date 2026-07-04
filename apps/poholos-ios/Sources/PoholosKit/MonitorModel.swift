// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak
//
// Aggregates scanner events into what the monitor UI shows: the feed,
// per-source reception statistics, and traffic counters. Pure
// event-in/state-out on the main actor — the radio stays behind
// MeshScanner — so the whole layer is unit-testable by feeding it
// synthetic events.

import CoreBluetooth
import Foundation

/// One line of the monitor's feed.
public struct FeedEntry: Identifiable, Equatable {
    /// What kind of line, in the firmware displays' visual language.
    public enum Kind: Equatable {
        /// `*` — a broadcast, delivered to everyone.
        case hearsay
        /// `@` — a telegram addressed to this node.
        case telegram
        /// `~` — a telegram for someone else, observed in transit.
        case passingThrough
        /// `?` — carried the poholos company id but did not decode
        /// (test transmitters, corruption, or id squatters).
        case undecodable
    }

    public let id = UUID()
    public let receivedAt: Date
    public let kind: Kind
    /// Originating node; nil for undecodable frames.
    public let src: WireID?
    /// `all`, `you`, or the destination's 8-hex id; nil for undecodable.
    public let destLabel: String?
    /// Message text, or a description for undecodable frames.
    public let text: String
    /// Received signal strength in dBm.
    public let rssi: Int
    /// True for a frame beyond the legacy 22-byte budget (wire v1).
    public let isExtended: Bool

    /// The line's feed marker: `*` / `@` / `~` / `?`.
    public var marker: String {
        switch kind {
        case .hearsay: "*"
        case .telegram: "@"
        case .passingThrough: "~"
        case .undecodable: "?"
        }
    }
}

/// Live per-source reception statistics for the diagnostics screen.
public struct SourceStats: Equatable {
    /// Distinct messages received from this source (dedup already done).
    public internal(set) var messages: Int
    /// When the last message arrived.
    public internal(set) var lastSeen: Date
    /// Signal strength of the last message, in dBm.
    public internal(set) var lastRSSI: Int
    /// Remaining hop count of the last message (16 = heard directly,
    /// lower = arrived through relays).
    public internal(set) var lastTTL: UInt8
}

/// Observable state behind the monitor's screens.
///
/// Owns the scanner and reduces its callbacks into published state on
/// the main actor. The `handle*` methods are the aggregation seam:
/// scanner callbacks land there in production, tests call them directly.
@MainActor
public final class MonitorModel: ObservableObject {
    /// Newest-first feed, capped at ``feedLimit``.
    @Published public private(set) var feed: [FeedEntry] = []
    /// Reception statistics per source node.
    @Published public private(set) var sources: [WireID: SourceStats] = [:]
    /// Bluetooth availability, as last reported by the scanner.
    @Published public private(set) var bluetoothState: CBManagerState = .unknown
    /// `CBCentralManager.supports(.extendedScanAndConnect)`, captured at
    /// powered-on when the answer is authoritative (before that the API
    /// returns a false negative). Nil until then, and on macOS where the
    /// API does not exist. Validation showed the flag can read true while
    /// extended advertisements still never reach the app.
    @Published public private(set) var extendedScanSupported: Bool?
    /// Messages shown to the user (hearsay + telegrams for this node).
    @Published public private(set) var messagesDelivered = 0
    /// Router-suppressed duplicates — most mesh traffic; a healthy,
    /// steadily rising number whenever repeating advertisers are around.
    @Published public private(set) var duplicatesSuppressed = 0
    /// Frames under the poholos company id that failed to decode.
    @Published public private(set) var undecodableFrames = 0
    /// Distinct lengths of undecodable frames, in order of appearance —
    /// the ext-adv platform test reads its answer here.
    @Published public private(set) var undecodableLengths: [Int] = []

    /// This node's full display name, e.g. `iph-3f2a`.
    public let localName: String
    /// The wire id peers use to address this node.
    public let localID: WireID
    /// Oldest feed entries beyond this count are dropped.
    public var feedLimit = 500

    /// False only if the engine rejected the node name outright.
    public var engineAvailable: Bool { scanner != nil }

    private let scanner: MeshScanner?

    public init(name: String) {
        localName = name
        localID = WireID(name: name)
        scanner = MeshScanner(name: name)
        scanner?.onEvent = { [weak self] event in
            Task { @MainActor in self?.handle(event: event) }
        }
        scanner?.onUndecodableFrame = { [weak self] length, rssi in
            Task { @MainActor in self?.handleUndecodable(length: length, rssi: rssi) }
        }
        scanner?.onStateChange = { [weak self] state in
            Task { @MainActor in self?.handleState(state) }
        }
    }

    /// Starts scanning (triggers the Bluetooth permission prompt on
    /// first use).
    public func start() {
        scanner?.start()
    }

    /// Stops scanning; ``start()`` resumes it.
    public func stop() {
        scanner?.stop()
    }

    /// Sources ordered by recency, for the diagnostics table.
    public var sortedSources: [(id: WireID, stats: SourceStats)] {
        sources
            .map { (id: $0.key, stats: $0.value) }
            .sorted { $0.stats.lastSeen > $1.stats.lastSeen }
    }

    /// Reduces one routed advertisement into feed and statistics.
    public func handle(event: MeshScanner.Event) {
        switch event.action {
        case .deliver(let message), .deliverAndForward(let message, _):
            note(message: message, event: event)
            messagesDelivered += 1
            append(entry(for: message, event: event, kind: message.dest == nil ? .hearsay : .telegram))
        case .forward(let message, _):
            note(message: message, event: event)
            append(entry(for: message, event: event, kind: .passingThrough))
        case .ignore(.duplicate):
            duplicatesSuppressed += 1
        case .ignore:
            break
        }
    }

    /// Counts an ours-but-undecodable frame; the first of each distinct
    /// length also gets a feed line (they repeat continuously and have
    /// no seen-cache, so per-frame lines would flood the feed).
    public func handleUndecodable(length: Int, rssi: Int) {
        undecodableFrames += 1
        guard !undecodableLengths.contains(length) else { return }
        undecodableLengths.append(length)
        append(
            FeedEntry(
                receivedAt: Date(),
                kind: .undecodable,
                src: nil,
                destLabel: nil,
                text: "\(length)-byte undecodable frame under 0xF10C",
                rssi: rssi,
                isExtended: length > 22
            ))
    }

    /// Tracks Bluetooth availability; the extended-scan capability is
    /// sampled here, at powered-on, when the API answers truthfully.
    public func handleState(_ state: CBManagerState) {
        bluetoothState = state
        if state == .poweredOn {
            extendedScanSupported = MeshScanner.supportsExtendedScan
        }
    }

    private func entry(
        for message: MeshMessage, event: MeshScanner.Event, kind: FeedEntry.Kind
    ) -> FeedEntry {
        FeedEntry(
            receivedAt: event.receivedAt,
            kind: kind,
            src: message.src,
            destLabel: destLabel(for: message.dest),
            text: message.text,
            rssi: event.rssi,
            isExtended: event.isExtended
        )
    }

    private func destLabel(for dest: WireID?) -> String {
        switch dest {
        case nil: "all"
        case localID?: "you"
        case .some(let other): "\(other)"
        }
    }

    private func note(message: MeshMessage, event: MeshScanner.Event) {
        var stats =
            sources[message.src]
            ?? SourceStats(messages: 0, lastSeen: event.receivedAt, lastRSSI: 0, lastTTL: 0)
        stats.messages += 1
        stats.lastSeen = event.receivedAt
        stats.lastRSSI = event.rssi
        stats.lastTTL = message.ttl
        sources[message.src] = stats
    }

    private func append(_ entry: FeedEntry) {
        feed.insert(entry, at: 0)
        if feed.count > feedLimit {
            feed.removeLast(feed.count - feedLimit)
        }
    }
}
