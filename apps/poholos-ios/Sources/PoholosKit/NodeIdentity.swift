// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

import Foundation

#if os(iOS)
    import UIKit
#endif

/// Derives this device's stable node identity, e.g. `iphone-3f2a`.
///
/// The format matches `NodeId` everywhere else in poholos: a name of
/// `[a-z0-9-]` plus a `-xxxx` suffix of 4 lowercase hex characters. A
/// stable identity means telegram-addressed-to-me detection works today
/// (RX-only) and peers can already address this device when it learns
/// to transmit.
public enum NodeIdentity {
    /// The local node's full display name.
    ///
    /// On iOS the suffix derives from `identifierForVendor`, which is
    /// stable while the app stays installed. Elsewhere (and if the
    /// vendor id is momentarily unavailable) a random suffix is created
    /// once and persisted in user defaults.
    public static func local(defaults: UserDefaults = .standard) -> String {
        "\(prefix)-\(suffix(defaults: defaults))"
    }

    private static var prefix: String {
        #if os(iOS)
            return UIDevice.current.userInterfaceIdiom == .pad ? "ipad" : "iphone"
        #else
            return "mac"
        #endif
    }

    private static func suffix(defaults: UserDefaults) -> String {
        #if os(iOS)
            if let vendor = UIDevice.current.identifierForVendor {
                return String(format: "%02x%02x", vendor.uuid.0, vendor.uuid.1)
            }
        #endif
        let key = "com.poholos.monitor.node-suffix"
        if let stored = defaults.string(forKey: key) {
            return stored
        }
        let fresh = String(format: "%04x", UInt16.random(in: .min ... .max))
        defaults.set(fresh, forKey: key)
        return fresh
    }
}
