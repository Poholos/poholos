// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak
//
// Drives the UI aggregation layer with synthetic events — no radio,
// no CBCentralManager (the model defers creating one until start(),
// which these tests never call).

import Foundation
import XCTest

@testable import PoholosKit

@MainActor
final class MonitorModelTests: XCTestCase {
    private let alice = WireID(name: "alice-3f2a")

    private func makeModel() -> MonitorModel {
        MonitorModel(name: "mac-3f2a")
    }

    private func event(
        _ action: RouteAction, rssi: Int = -60, frameLength: Int = 22
    ) -> MeshScanner.Event {
        MeshScanner.Event(
            action: action, rssi: rssi, receivedAt: Date(), frameLength: frameLength)
    }

    private func message(
        src: WireID, dest: WireID? = nil, ttl: UInt8 = 16, text: String = "hi"
    ) -> MeshMessage {
        MeshMessage(src: src, dest: dest, seq: 1, ttl: ttl, payload: Data(text.utf8))
    }

    func testHearsayBecomesAFeedEntryAndSourceStats() throws {
        let model = makeModel()
        model.handle(event: event(.deliver(message(src: alice, text: "hello")), rssi: -48))

        XCTAssertEqual(model.feed.count, 1)
        let entry = try XCTUnwrap(model.feed.first)
        XCTAssertEqual(entry.kind, .hearsay)
        XCTAssertEqual(entry.marker, "*")
        XCTAssertEqual(entry.src, alice)
        XCTAssertEqual(entry.destLabel, "all")
        XCTAssertEqual(entry.text, "hello")
        XCTAssertEqual(entry.rssi, -48)
        XCTAssertFalse(entry.isExtended)
        XCTAssertEqual(entry.frameLength, 22)
        XCTAssertEqual(
            entry.message, message(src: alice, text: "hello"),
            "full message kept for the details screen")
        XCTAssertEqual(model.messagesDelivered, 1)

        let stats = try XCTUnwrap(model.sources[alice])
        XCTAssertEqual(stats.messages, 1)
        XCTAssertEqual(stats.lastRSSI, -48)
        XCTAssertEqual(stats.lastTTL, 16)
    }

    func testTelegramForThisNodeIsMarkedAndLabeled() throws {
        let model = makeModel()
        model.handle(event: event(.deliver(message(src: alice, dest: model.localID))))

        let entry = try XCTUnwrap(model.feed.first)
        XCTAssertEqual(entry.kind, .telegram)
        XCTAssertEqual(entry.marker, "@")
        XCTAssertEqual(entry.destLabel, "you")
    }

    func testInTransitTelegramShowsDestinationAndSkipsDeliveredCount() throws {
        let model = makeModel()
        let bob = WireID(name: "bob-9c01")
        model.handle(
            event: event(.forward(message(src: alice, dest: bob, ttl: 4), relay: Data())))

        let entry = try XCTUnwrap(model.feed.first)
        XCTAssertEqual(entry.kind, .passingThrough)
        XCTAssertEqual(entry.destLabel, "\(bob)")
        XCTAssertEqual(model.messagesDelivered, 0, "in-transit is not a delivery")
        XCTAssertEqual(model.sources[alice]?.lastTTL, 4)
    }

    func testDuplicatesCountWithoutFeedEntries() throws {
        let model = makeModel()
        model.handle(event: event(.ignore(.duplicate)))
        model.handle(event: event(.ignore(.duplicate)))
        model.handle(event: event(.ignore(.own)))

        XCTAssertEqual(model.duplicatesSuppressed, 2)
        XCTAssertTrue(model.feed.isEmpty)
        XCTAssertTrue(model.sources.isEmpty)
    }

    func testUndecodableFramesCountAllButFeedOncePerLength() throws {
        let model = makeModel()
        let big = Data(0..<200)
        model.handleUndecodable(frame: big, rssi: -60)
        model.handleUndecodable(frame: big, rssi: -61)
        model.handleUndecodable(frame: Data(repeating: 0xEE, count: 10), rssi: -62)

        XCTAssertEqual(model.undecodableFrames, 3)
        XCTAssertEqual(model.undecodableLengths, [200, 10])
        XCTAssertEqual(model.feed.count, 2, "one feed line per distinct length")
        XCTAssertEqual(model.feed.map(\.kind), [.undecodable, .undecodable])
        XCTAssertTrue(model.feed[1].isExtended, "200-byte frame is beyond the legacy budget")
        XCTAssertFalse(model.feed[0].isExtended)
        XCTAssertEqual(model.feed[1].rawFrame, big, "raw bytes kept for the details screen")
        XCTAssertNil(model.feed[1].message)
    }

    func testFeedIsCappedNewestFirst() throws {
        let model = makeModel()
        model.feedLimit = 3
        for n in 1...5 {
            model.handle(event: event(.deliver(message(src: alice, text: "m\(n)"))))
        }

        XCTAssertEqual(model.feed.map(\.text), ["m5", "m4", "m3"])
        XCTAssertEqual(model.messagesDelivered, 5)
        XCTAssertEqual(model.sources[alice]?.messages, 5)
    }

    func testSourcesSortByRecency() throws {
        let model = makeModel()
        let bob = WireID(name: "bob-9c01")
        model.handle(event: event(.deliver(message(src: alice))))
        model.handle(event: event(.deliver(message(src: bob, text: "later"))))

        XCTAssertEqual(model.sortedSources.map(\.id), [bob, alice])
    }

    func testPoweredOnSamplesExtendedScanSupport() throws {
        let model = makeModel()
        XCTAssertNil(model.extendedScanSupported)
        model.handleState(.poweredOn)
        XCTAssertEqual(model.bluetoothState, .poweredOn)
        // Value is platform-dependent; poweredOn must sample it (macOS: nil).
        XCTAssertEqual(model.extendedScanSupported, MeshScanner.supportsExtendedScan)
    }
}
