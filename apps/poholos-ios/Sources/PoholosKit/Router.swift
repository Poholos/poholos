// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak
//
// Swift face of the Rust routing engine. This file is the only place
// that touches the raw C surface (PoholosFFI); everything above it works
// with Swift values. It also hides one cbindgen artifact: for pre-C23
// compilers each repr(u8) enum imports as a C enum type (the constants)
// plus a separate uint8_t typedef (the ABI), so comparisons here go
// through the constants' rawValue exactly once.

import Foundation
import PoholosFFI

/// Compact 32-bit node identity used on the wire.
public struct WireID: Hashable, Sendable, CustomStringConvertible {
    public let raw: UInt32

    public init(raw: UInt32) {
        self.raw = raw
    }

    /// Derives the wire id from a node's full display name (FNV-1a 64
    /// truncated to 32 bits) — the same derivation as on every other
    /// platform, so any node that knows a peer's name can address it.
    public init(name: String) {
        self.raw = poholos_wire_id_of_name(name)
    }

    /// The 8-hex-digit form used across the poholos displays.
    public var description: String {
        String(format: "%08x", raw)
    }
}

/// A decoded poholos message.
public struct MeshMessage: Equatable, Sendable {
    /// Originating node.
    public let src: WireID
    /// Destination for a telegram (unicast); nil for hearsay (broadcast).
    public let dest: WireID?
    /// Per-source sequence number.
    public let seq: UInt16
    /// Remaining hop count.
    public let ttl: UInt8
    /// Payload bytes (UTF-8 text by convention, not guaranteed).
    public let payload: Data

    /// The payload as text, with invalid UTF-8 shown as replacement
    /// characters.
    public var text: String {
        String(decoding: payload, as: UTF8.self)
    }
}

/// Why the router ignored a frame.
public enum IgnoreReason: Equatable, Sendable {
    /// The packet originated from this node (our own echo).
    case own
    /// The packet was already handled (seen-cache hit).
    case duplicate
    /// A telegram for another node arrived with no hops left.
    case expiredTTL
    /// A reason this binding predates; ignore all the same.
    case other
}

/// What to do with an ingested frame, mirroring the engine's `RouteAction`.
public enum RouteAction: Equatable {
    /// Show the message locally; nothing to re-broadcast.
    case deliver(MeshMessage)
    /// Show the message locally and re-broadcast `relay` (TTL already
    /// decremented). A receive-only monitor treats this as `deliver`.
    case deliverAndForward(MeshMessage, relay: Data)
    /// Not for us: re-broadcast `relay`. The message describes the
    /// in-transit packet (TTL already decremented) so monitors can
    /// display relayed telegrams too.
    case forward(MeshMessage, relay: Data)
    /// Do nothing.
    case ignore(IgnoreReason)
}

/// Failure of a call into the routing engine.
public enum RouterError: Error, Equatable {
    /// The bytes did not decode as a poholos frame. Routine in radio
    /// environments: foreign advertisements slip through scan filtering.
    case notAPoholosFrame
    /// The engine reported an internal failure (a caught panic or a
    /// rejected argument); indicates a bug, not bad input.
    case engineFailure
}

/// The poholos routing state machine for one node: feed received frame
/// bytes to ``ingest(_:)`` and act on the returned ``RouteAction``.
///
/// Wraps the Rust engine, so duplicate suppression and delivery follow
/// the exact semantics of every other node. Not thread-safe: confine
/// each instance to one queue (``MeshScanner`` does this).
public final class Router {
    private let handle: OpaquePointer

    /// The node's full display name, e.g. `iphone-3f2a`.
    public let name: String

    /// The wire id peers use to address this node.
    public let localID: WireID

    /// Creates a router for the node with the given display name.
    /// Returns nil only if the engine rejects the name outright.
    public init?(name: String) {
        guard let handle = poholos_router_new(name) else { return nil }
        self.handle = handle
        self.name = name
        self.localID = WireID(name: name)
    }

    deinit {
        poholos_router_free(handle)
    }

    /// Processes received frame bytes (the manufacturer data minus the
    /// company id) and decides what to do with them.
    public func ingest(_ frame: Data) throws -> RouteAction {
        var action = PoholosAction()
        let status = frame.withUnsafeBytes { (buffer: UnsafeRawBufferPointer) in
            poholos_router_ingest(
                handle,
                buffer.bindMemory(to: UInt8.self).baseAddress,
                buffer.count,
                &action
            )
        }
        switch UInt32(status) {
        case POHOLOS_STATUS_OK.rawValue:
            return RouteAction(flattened: action)
        case POHOLOS_STATUS_DECODE_ERROR.rawValue:
            throw RouterError.notAPoholosFrame
        default:
            throw RouterError.engineFailure
        }
    }
}

extension RouteAction {
    /// Lifts the C out-struct back into the Swift enum.
    init(flattened action: PoholosAction) {
        let message = MeshMessage(
            src: WireID(raw: action.src),
            dest: action.has_dest ? WireID(raw: action.dest) : nil,
            seq: action.seq,
            ttl: action.ttl,
            payload: withUnsafeBytes(of: action.payload) {
                Data($0.prefix(Int(action.payload_len)))
            }
        )
        let relay = withUnsafeBytes(of: action.frame) {
            Data($0.prefix(Int(action.frame_len)))
        }

        switch UInt32(action.kind) {
        case POHOLOS_ACTION_KIND_DELIVER.rawValue:
            self = .deliver(message)
        case POHOLOS_ACTION_KIND_DELIVER_AND_FORWARD.rawValue:
            self = .deliverAndForward(message, relay: relay)
        case POHOLOS_ACTION_KIND_FORWARD.rawValue:
            self = .forward(message, relay: relay)
        default:
            switch UInt32(action.ignore_reason) {
            case POHOLOS_IGNORE_REASON_OWN.rawValue:
                self = .ignore(.own)
            case POHOLOS_IGNORE_REASON_DUPLICATE.rawValue:
                self = .ignore(.duplicate)
            case POHOLOS_IGNORE_REASON_EXPIRED_TTL.rawValue:
                self = .ignore(.expiredTTL)
            default:
                self = .ignore(.other)
            }
        }
    }
}
