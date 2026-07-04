// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak
//
// Exercises the Swift wrapper against the real Rust engine (the Mac
// slice of PoholosFFI.xcframework). Frames are built by hand from the
// wire format — byte 0 packs version|has_dest|ttl, then big-endian seq,
// src, and optional dest — which doubles as a cross-check that the
// Swift-side expectations match the encoder.

import Foundation
import XCTest

@testable import PoholosKit

private let monitorName = "mac-3f2a"

/// Builds an on-air frame; version 0 covers everything short, and the
/// same layout at greater length is version 1.
private func frame(
    src: WireID,
    dest: WireID? = nil,
    seq: UInt16 = 1,
    ttl: UInt8 = 16,
    payload: Data,
    version: UInt8 = 0
) -> Data {
    var bytes = Data()
    bytes.append(version << 6 | (dest == nil ? 0 : 0x20) | ttl & 0x1F)
    bytes.append(contentsOf: [UInt8(seq >> 8), UInt8(seq & 0xFF)])
    for shift in stride(from: 24, through: 0, by: -8) {
        bytes.append(UInt8(src.raw >> UInt32(shift) & 0xFF))
    }
    if let dest {
        for shift in stride(from: 24, through: 0, by: -8) {
            bytes.append(UInt8(dest.raw >> UInt32(shift) & 0xFF))
        }
    }
    bytes.append(payload)
    return bytes
}

final class WireIDTests: XCTestCase {
    func testDerivationIsDeterministicAndNameSensitive() {
        XCTAssertEqual(WireID(name: "alice-3f2a"), WireID(name: "alice-3f2a"))
        XCTAssertNotEqual(WireID(name: "alice-3f2a"), WireID(name: "bob-9c01"))
    }

    func testDescriptionIsEightHexDigits() {
        XCTAssertEqual(WireID(raw: 0x0000_00FF).description, "000000ff")
        XCTAssertEqual(WireID(raw: 0xA1B2_C3D4).description, "a1b2c3d4")
    }
}

final class RouterTests: XCTestCase {
    private var router: Router!

    override func setUp() {
        super.setUp()
        router = Router(name: monitorName)
        XCTAssertNotNil(router)
    }

    func testHearsayDeliversAndForwardsWithDecrementedTTL() throws {
        let src = WireID(name: "alice-3f2a")
        let action = try router.ingest(
            frame(src: src, seq: 7, ttl: 16, payload: Data("hi mesh".utf8)))

        guard case .deliverAndForward(let message, let relay) = action else {
            return XCTFail("expected deliverAndForward, got \(action)")
        }
        XCTAssertEqual(message.src, src)
        XCTAssertNil(message.dest)
        XCTAssertEqual(message.seq, 7)
        XCTAssertEqual(message.ttl, 16, "delivered copy keeps the received TTL")
        XCTAssertEqual(message.text, "hi mesh")
        XCTAssertEqual(relay[relay.startIndex] & 0x1F, 15, "relay frame TTL is decremented")
    }

    func testTelegramForUsIsDeliveredWithoutRelay() throws {
        let action = try router.ingest(
            frame(
                src: WireID(name: "alice-3f2a"), dest: router.localID,
                payload: Data("psst".utf8)))

        guard case .deliver(let message) = action else {
            return XCTFail("expected deliver, got \(action)")
        }
        XCTAssertEqual(message.dest, router.localID)
        XCTAssertEqual(message.text, "psst")
    }

    func testTelegramForOtherIsForwardedWithInTransitFields() throws {
        let dest = WireID(name: "bob-9c01")
        let action = try router.ingest(
            frame(src: WireID(name: "alice-3f2a"), dest: dest, ttl: 5, payload: Data("go".utf8)))

        guard case .forward(let message, let relay) = action else {
            return XCTFail("expected forward, got \(action)")
        }
        XCTAssertEqual(message.dest, dest)
        XCTAssertEqual(message.ttl, 4, "in-transit copy carries the decremented TTL")
        XCTAssertFalse(relay.isEmpty)
    }

    func testDuplicateViaSecondRouteIsIgnored() throws {
        let src = WireID(name: "alice-3f2a")
        // Same message, different remaining TTLs (two mesh paths).
        _ = try router.ingest(frame(src: src, ttl: 16, payload: Data("hi".utf8)))
        let second = try router.ingest(frame(src: src, ttl: 9, payload: Data("hi".utf8)))
        XCTAssertEqual(second, .ignore(.duplicate))
    }

    func testOwnEchoIsIgnored() throws {
        let action = try router.ingest(
            frame(src: router.localID, payload: Data("echo".utf8)))
        XCTAssertEqual(action, .ignore(.own))
    }

    func testExpiredTelegramIsIgnored() throws {
        let action = try router.ingest(
            frame(
                src: WireID(name: "alice-3f2a"), dest: WireID(name: "bob-9c01"), ttl: 1,
                payload: Data("late".utf8)))
        XCTAssertEqual(action, .ignore(.expiredTTL))
    }

    func testExtendedFrameRoundTripsALongPayload() throws {
        // 200 bytes cannot fit the legacy budget: wire version 1.
        let payload = Data(repeating: 0x5A, count: 200)
        let action = try router.ingest(
            frame(src: WireID(name: "alice-3f2a"), payload: payload, version: 1))

        guard case .deliverAndForward(let message, _) = action else {
            return XCTFail("expected deliverAndForward, got \(action)")
        }
        XCTAssertEqual(message.payload, payload)
    }

    func testUndecodableBytesThrow() {
        XCTAssertThrowsError(try router.ingest(Data())) { error in
            XCTAssertEqual(error as? RouterError, .notAPoholosFrame)
        }
        XCTAssertThrowsError(try router.ingest(Data([0x10, 0x00]))) { error in
            XCTAssertEqual(error as? RouterError, .notAPoholosFrame)
        }
        XCTAssertThrowsError(try router.ingest(Data(repeating: 0, count: 212))) { error in
            XCTAssertEqual(error as? RouterError, .notAPoholosFrame)
        }
    }
}

final class MeshScannerTests: XCTestCase {
    func testManufacturerDataFrameExtraction() {
        let payload = Data([0x10, 0x00, 0x01, 0xAA, 0xBB, 0xCC, 0xDD, 0x68, 0x69])

        // Company id 0xF10C little-endian, then the frame.
        XCTAssertEqual(
            MeshScanner.frame(fromManufacturerData: Data([0x0C, 0xF1]) + payload), payload)
        // A non-zero-based index (Data slices keep parent offsets).
        let sliced = (Data([0xFF]) + Data([0x0C, 0xF1]) + payload).dropFirst()
        XCTAssertEqual(MeshScanner.frame(fromManufacturerData: sliced), payload)
        // Empty frame after the id is still ours.
        XCTAssertEqual(MeshScanner.frame(fromManufacturerData: Data([0x0C, 0xF1])), Data())

        // Foreign company id (Apple, 0x004C) and truncated data.
        XCTAssertNil(MeshScanner.frame(fromManufacturerData: Data([0x4C, 0x00]) + payload))
        XCTAssertNil(MeshScanner.frame(fromManufacturerData: Data([0x0C])))
        XCTAssertNil(MeshScanner.frame(fromManufacturerData: Data()))
    }

    func testScannerRoutesLikeTheEngine() throws {
        let scanner = try XCTUnwrap(MeshScanner(name: monitorName))
        let bytes = frame(src: WireID(name: "alice-3f2a"), payload: Data("hi".utf8))
        let action = try scanner.router.ingest(bytes)
        guard case .deliverAndForward = action else {
            return XCTFail("expected deliverAndForward, got \(action)")
        }
    }

    func testEventVersionBadgeThreshold() {
        let deliver = RouteAction.deliver(
            MeshMessage(src: WireID(raw: 1), dest: nil, seq: 0, ttl: 1, payload: Data()))
        let legacy = MeshScanner.Event(
            action: deliver, rssi: -40, receivedAt: Date(), frameLength: 22)
        let extended = MeshScanner.Event(
            action: deliver, rssi: -40, receivedAt: Date(), frameLength: 23)
        XCTAssertFalse(legacy.isExtended)
        XCTAssertTrue(extended.isExtended)
    }
}

final class NodeIdentityTests: XCTestCase {
    func testLocalIdentityMatchesNodeIdFormatAndIsStable() throws {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "poholos-tests"))
        defaults.removePersistentDomain(forName: "poholos-tests")

        let name = NodeIdentity.local(defaults: defaults)
        // NodeId format: 1-16 chars of [a-z0-9-], then -xxxx hex suffix.
        XCTAssertNotNil(
            name.range(of: "^[a-z0-9][a-z0-9-]{0,15}-[0-9a-f]{4}$", options: .regularExpression),
            "'\(name)' is not a valid poholos node id")
        XCTAssertEqual(name, NodeIdentity.local(defaults: defaults), "identity must be stable")
    }
}
