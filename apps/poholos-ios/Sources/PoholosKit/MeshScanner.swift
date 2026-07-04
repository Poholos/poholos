// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

import CoreBluetooth
import Foundation
import PoholosFFI

/// Listens for poholos frames in BLE advertisements and routes them.
///
/// Wraps a `CBCentralManager` scanning with no service filter and
/// duplicates allowed — poholos nodes repeat the same advertisement
/// continuously, so every event is wanted and the router's seen-cache
/// does the dedup, same as on every other node. Frames ride in
/// manufacturer-specific data under the poholos company id.
///
/// Foreground-only by design (v1 scope): iOS stops undirected scans in
/// the background, and no background mode is requested.
///
/// Callbacks fire on a private serial queue; hop to the main queue
/// before touching UI.
public final class MeshScanner: NSObject {
    /// One received-and-routed advertisement.
    public struct Event {
        /// The routing decision, including the decoded message if any.
        public let action: RouteAction
        /// Received signal strength in dBm.
        public let rssi: Int
        /// When the advertisement arrived.
        public let receivedAt: Date
        /// On-air frame length in bytes; distinguishes wire versions.
        public let frameLength: Int

        /// True for an extended (wire version 1) frame, which cannot fit
        /// the legacy 22-byte advertising budget.
        public var isExtended: Bool { frameLength > 22 }
    }

    /// The poholos Bluetooth company identifier (`0xF10C`).
    public static let companyID = UInt16(POHOLOS_COMPANY_ID)

    /// Whether the local Bluetooth controller supports BLE 5 extended
    /// scanning — the open question for wire version 1 reception on iOS.
    /// Nil where the API is unavailable (macOS).
    public static var supportsExtendedScan: Bool? {
        #if os(iOS) || os(tvOS) || os(watchOS)
            return CBCentralManager.supports(.extendedScanAndConnect)
        #else
            return nil
        #endif
    }

    /// The routing engine, keyed to this node's identity.
    public let router: Router

    /// Called for every advertisement that carried a poholos frame,
    /// including ones the router ignored (duplicates make up most mesh
    /// traffic; consumers filter). Fires on the scanner's queue.
    public var onEvent: ((Event) -> Void)?

    /// Called when Bluetooth availability changes. Fires on the
    /// scanner's queue.
    public var onStateChange: ((CBManagerState) -> Void)?

    /// Called for *every* frame that carried the poholos company id but
    /// failed to decode (frame bytes, RSSI). Such frames repeat
    /// continuously and bypass the seen-cache, so display consumers
    /// should dedup (e.g. one feed line per distinct length) while
    /// counters count.
    ///
    /// This is the platform-validation instrument: a non-protocol test
    /// transmitter (the ext-adv POC) shows up here, and the surfaced
    /// length answers whether the OS exposed the full extended payload.
    public var onUndecodableFrame: ((Data, Int) -> Void)?

    /// Frames that carried the poholos company id but failed to decode.
    public private(set) var undecodableFrameCount = 0

    private let queue = DispatchQueue(label: "com.poholos.monitor.scanner")
    private var central: CBCentralManager?
    private var scanRequested = false

    /// Creates a scanner routing as the node with the given display name.
    public init?(name: String) {
        guard let router = Router(name: name) else { return nil }
        self.router = router
        super.init()
    }

    /// Starts scanning. Instantiating the central manager here (not in
    /// `init`) defers the system Bluetooth permission prompt until the
    /// app actually asks to scan; scanning begins once the controller
    /// reports powered-on.
    public func start() {
        queue.async {
            self.scanRequested = true
            if self.central == nil {
                self.central = CBCentralManager(delegate: self, queue: self.queue)
            } else {
                self.beginScanIfReady()
            }
        }
    }

    /// Stops scanning; `start()` resumes it.
    public func stop() {
        queue.async {
            self.scanRequested = false
            self.central?.stopScan()
        }
    }

    /// Extracts the poholos frame from raw manufacturer-specific data:
    /// the 2-byte little-endian company id (per the Bluetooth
    /// specification) followed by the frame. Nil for foreign ids.
    public static func frame(fromManufacturerData data: Data) -> Data? {
        guard data.count >= 2 else { return nil }
        let id = UInt16(data[data.startIndex]) | UInt16(data[data.startIndex + 1]) << 8
        guard id == companyID else { return nil }
        return data.dropFirst(2)
    }

    private func beginScanIfReady() {
        guard let central, central.state == .poweredOn, scanRequested,
            !central.isScanning
        else { return }
        central.scanForPeripherals(
            withServices: nil,
            options: [CBCentralManagerScanOptionAllowDuplicatesKey: true]
        )
    }
}

extension MeshScanner: CBCentralManagerDelegate {
    public func centralManagerDidUpdateState(_ central: CBCentralManager) {
        beginScanIfReady()
        onStateChange?(central.state)
    }

    public func centralManager(
        _ central: CBCentralManager,
        didDiscover peripheral: CBPeripheral,
        advertisementData: [String: Any],
        rssi RSSI: NSNumber
    ) {
        guard
            let manufacturerData = advertisementData[CBAdvertisementDataManufacturerDataKey]
                as? Data,
            let frame = Self.frame(fromManufacturerData: manufacturerData)
        else { return }

        do {
            let action = try router.ingest(frame)
            onEvent?(
                Event(
                    action: action,
                    rssi: RSSI.intValue,
                    receivedAt: Date(),
                    frameLength: frame.count
                ))
        } catch {
            // A frame under our company id that the engine rejects: a
            // test transmitter, radio corruption, or a foreign device
            // squatting on the id.
            undecodableFrameCount += 1
            onUndecodableFrame?(frame, RSSI.intValue)
        }
    }
}
