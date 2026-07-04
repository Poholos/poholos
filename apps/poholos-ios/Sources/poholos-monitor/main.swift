// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak
//
// Terminal mesh monitor — the Mac dev loop for the iOS app. Runs the
// same PoholosKit pipeline (CoreBluetooth scanner -> Rust router) and
// prints the feed, so the whole stack is debuggable against the live
// mesh without provisioning a phone.
//
//     swift run poholos-monitor [name] [--verbose]
//
// macOS prompts for Bluetooth access on first run (System Settings >
// Privacy & Security > Bluetooth, granted to your terminal app).

import CoreBluetooth
import Foundation
import PoholosKit

let arguments = CommandLine.arguments.dropFirst()
let verbose = arguments.contains("--verbose")
let name = arguments.first { !$0.hasPrefix("--") } ?? NodeIdentity.local()

guard let scanner = MeshScanner(name: name) else {
    FileHandle.standardError.write(Data("error: engine rejected node name '\(name)'\n".utf8))
    exit(1)
}

let clock = DateFormatter()
clock.dateFormat = "HH:mm:ss"

func line(_ message: MeshMessage, _ event: MeshScanner.Event, marker: String) -> String {
    let dest =
        switch message.dest {
        case nil: "all"
        case scanner.router.localID: "you"
        case .some(let other): "\(other)"
        }
    let version = event.isExtended ? "v1" : "v0"
    return "\(clock.string(from: event.receivedAt)) \(marker) \(message.src) → \(dest)"
        + "  \(message.text)  (ttl \(message.ttl), \(event.rssi) dBm, \(version))"
}

print("poholos monitor — \(name) (wire id \(scanner.router.localID)), receive-only")

scanner.onStateChange = { state in
    let label =
        switch state {
        case .poweredOn: "powered on, scanning"
        case .poweredOff: "powered off — enable Bluetooth"
        case .unauthorized: "unauthorized — grant Bluetooth access to your terminal"
        case .unsupported: "unsupported on this machine"
        default: "state \(state.rawValue)"
        }
    print("· bluetooth: \(label)")
}

scanner.onEvent = { event in
    switch event.action {
    case .deliver(let message), .deliverAndForward(let message, _):
        // Reuses the firmware displays' visual language: * hearsay,
        // @ telegram for this node.
        print(line(message, event, marker: message.dest == nil ? "*" : "@"))
    case .forward(let message, _):
        // A telegram passing through; a transmitting node would relay it.
        print(line(message, event, marker: "~"))
    case .ignore(let reason):
        if verbose {
            print("\(clock.string(from: event.receivedAt)) · ignored (\(reason))")
        }
    }
}

// First frame of each distinct undecodable length: the signal the
// ext-adv POC transmitter produces (its payload is not a poholos frame).
scanner.onUndecodableFrame = { length, rssi in
    print("? \(length)-byte undecodable frame under company id f10c (\(rssi) dBm)")
}

scanner.start()
dispatchMain()
