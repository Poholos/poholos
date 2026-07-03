// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Ivan Petrouchtchak

//! C ABI for the poholos routing engine.
//!
//! This crate wraps [`poholos::Router`] in a small, C-callable surface so
//! non-Rust hosts (the iOS monitor app first, via a Swift wrapper over a
//! cbindgen-generated header) run the *actual* protocol engine — the exact
//! duplicate-suppression and delivery semantics every other node uses —
//! instead of re-implementing frame decoding.
//!
//! # Surface
//!
//! Four functions:
//!
//! * [`poholos_router_new`] — creates a router for a display name (the
//!   [`WireId`] is derived from the name, same as everywhere else).
//! * [`poholos_router_free`] — releases a router.
//! * [`poholos_router_ingest`] — feeds received frame bytes in and flattens
//!   the resulting [`ExtRouteAction`] into the C-friendly [`PoholosAction`].
//! * [`poholos_wire_id_of_name`] — derives a wire id for display purposes.
//!
//! Ingestion always runs at the extended (wire version 1) frame capacity,
//! so a host built on this crate is dual-stack: it accepts both the legacy
//! 22-byte frames and the larger BLE 5 extended-advertising frames.
//!
//! # Contract
//!
//! * Every function is panic-safe: panics are caught at the boundary and
//!   surfaced as a sentinel return ([`PoholosStatus::Panic`], a null
//!   pointer, or a zero id) instead of unwinding into foreign frames.
//! * A [`PoholosRouter`] is **not** thread-safe; callers must serialize
//!   access to one handle (the monitor's single scan-callback queue does
//!   this naturally).
//! * [`PoholosAction`] is a plain value: every field is written on a
//!   successful ingest, so callers may pass uninitialized storage.

use std::ffi::{CStr, c_char};
use std::panic::{self, AssertUnwindSafe};
use std::{ptr, slice};

use poholos::{ExtRouteAction, IgnoreReason, Router, WireId};

/// Largest frame [`poholos_router_ingest`] accepts, in bytes.
///
/// This is the extended (wire version 1) frame capacity
/// ([`poholos::MAX_EXT_FRAME_LEN`]); legacy 22-byte frames are a subset.
/// The literal is repeated here because cbindgen cannot resolve constants
/// from other crates; the assert below keeps it honest.
pub const POHOLOS_MAX_FRAME_LEN: usize = 211;

/// Largest payload a single [`PoholosAction`] can carry, in bytes.
///
/// This is the extended hearsay payload limit
/// ([`poholos::MAX_EXT_PAYLOAD_HEARSAY`]); telegrams, with their longer
/// header, stay strictly below it.
pub const POHOLOS_MAX_PAYLOAD_LEN: usize = 204;

/// Bluetooth manufacturer-specific-data company identifier for poholos
/// ([`poholos::COMPANY_ID`]).
///
/// Scanners filter advertisements on this id (little-endian on the air,
/// per the Bluetooth specification) and feed the bytes that follow it to
/// [`poholos_router_ingest`].
pub const POHOLOS_COMPANY_ID: u16 = 0xF10C;

const _: () = assert!(POHOLOS_MAX_FRAME_LEN == poholos::MAX_EXT_FRAME_LEN);
const _: () = assert!(POHOLOS_MAX_PAYLOAD_LEN == poholos::MAX_EXT_PAYLOAD_HEARSAY);
const _: () = assert!(POHOLOS_COMPANY_ID == poholos::COMPANY_ID);

/// Result of a call that crosses the FFI boundary.
#[repr(u8)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PoholosStatus {
    /// The call succeeded; out-parameters are valid.
    Ok = 0,
    /// A required pointer argument was null.
    NullArgument = 1,
    /// The bytes did not decode as a poholos frame. Expected and harmless
    /// in radio environments: foreign advertisements slip through
    /// transport-level filtering.
    DecodeError = 2,
    /// A panic was caught at the FFI boundary; out-parameters are
    /// unspecified. Indicates a bug in the engine, not bad input.
    Panic = 3,
}

/// What the host should do with the frame it fed to
/// [`poholos_router_ingest`] — [`ExtRouteAction`] flattened for C.
#[repr(u8)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PoholosActionKind {
    /// Show the packet to the local user; do not forward.
    Deliver = 0,
    /// Show the packet to the local user and re-broadcast the frame.
    DeliverAndForward = 1,
    /// Not for us: re-broadcast the frame without local delivery.
    Forward = 2,
    /// Do nothing; see [`PoholosAction::ignore_reason`].
    Ignore = 3,
}

/// Why an ingested frame produced no deliver or forward action.
#[repr(u8)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PoholosIgnoreReason {
    /// The action is not [`PoholosActionKind::Ignore`].
    None = 0,
    /// The packet originated from this node (our own echo).
    Own = 1,
    /// The packet was already handled (seen-cache hit).
    Duplicate = 2,
    /// A telegram for another node arrived with no hops left.
    ExpiredTtl = 3,
    /// A reason this binding predates; treat as ignore all the same.
    Other = 4,
}

/// A routing decision, flattened into a plain C struct.
///
/// Which fields are meaningful depends on `kind`:
///
/// * `Deliver` / `DeliverAndForward` — the packet fields (`src`, `dest`,
///   `seq`, `ttl`, `payload`) describe the message to show, with the TTL
///   as received; `DeliverAndForward` additionally carries the re-encoded
///   `frame` (TTL already decremented) to re-broadcast.
/// * `Forward` — `frame` is the bytes to re-broadcast; the packet fields
///   describe the in-transit message (TTL already decremented) so passive
///   monitors can display relayed telegrams too.
/// * `Ignore` — only `ignore_reason` is meaningful.
///
/// Unused fields are zeroed, so `payload_len == 0` / `frame_len == 0`
/// reliably mean "absent".
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct PoholosAction {
    /// Discriminant selecting which fields below are meaningful.
    pub kind: PoholosActionKind,
    /// Why the frame was ignored; [`PoholosIgnoreReason::None`] otherwise.
    pub ignore_reason: PoholosIgnoreReason,
    /// `true` for a telegram (unicast); `false` for hearsay (broadcast).
    pub has_dest: bool,
    /// Remaining hop count of the packet.
    pub ttl: u8,
    /// Per-source sequence number.
    pub seq: u16,
    /// Number of meaningful bytes in `payload`.
    pub payload_len: u16,
    /// Number of meaningful bytes in `frame`; 0 when there is nothing to
    /// re-broadcast.
    pub frame_len: u16,
    /// Originating node's wire id.
    pub src: u32,
    /// Destination wire id; meaningful only when `has_dest` is `true`.
    pub dest: u32,
    /// Message payload bytes (UTF-8 text by convention, not guaranteed).
    pub payload: [u8; POHOLOS_MAX_PAYLOAD_LEN],
    /// Re-encoded frame to hand back to the transport for re-broadcast.
    pub frame: [u8; POHOLOS_MAX_FRAME_LEN],
}

impl PoholosAction {
    const EMPTY: Self = Self {
        kind: PoholosActionKind::Ignore,
        ignore_reason: PoholosIgnoreReason::None,
        has_dest: false,
        ttl: 0,
        seq: 0,
        payload_len: 0,
        frame_len: 0,
        src: 0,
        dest: 0,
        payload: [0; POHOLOS_MAX_PAYLOAD_LEN],
        frame: [0; POHOLOS_MAX_FRAME_LEN],
    };
}

/// Opaque handle to a [`poholos::Router`]. Create with
/// [`poholos_router_new`], release with [`poholos_router_free`].
#[derive(Debug)]
pub struct PoholosRouter {
    inner: Router,
}

/// Creates a router for the node with the given display name.
///
/// The node's [`WireId`] is derived from the full name (e.g.
/// `iphone-3f2a`) exactly as on every other platform, so peers that know
/// the name can address this node. Returns null if `name` is null, not
/// valid UTF-8, or the allocation panics.
///
/// # Safety
/// `name` must be null or a valid nul-terminated C string that outlives
/// the call. The returned pointer must be released with exactly one call
/// to [`poholos_router_free`].
#[unsafe(no_mangle)]
#[must_use]
pub unsafe extern "C" fn poholos_router_new(name: *const c_char) -> *mut PoholosRouter {
    panic::catch_unwind(|| {
        if name.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: non-null and nul-terminated per this function's contract.
        let Ok(name) = unsafe { CStr::from_ptr(name) }.to_str() else {
            return ptr::null_mut();
        };
        Box::into_raw(Box::new(PoholosRouter {
            inner: Router::new(WireId::of_name(name)),
        }))
    })
    .unwrap_or(ptr::null_mut())
}

/// Releases a router created by [`poholos_router_new`]. Null is a no-op.
///
/// # Safety
/// `router` must be null or a pointer obtained from
/// [`poholos_router_new`] that has not already been freed; it must not be
/// used after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn poholos_router_free(router: *mut PoholosRouter) {
    if router.is_null() {
        return;
    }
    drop(panic::catch_unwind(|| {
        // SAFETY: non-null and uniquely owned per this function's contract.
        drop(unsafe { Box::from_raw(router) });
    }));
}

/// Feeds received frame bytes to the router and flattens its decision
/// into `out_action`.
///
/// Accepts both legacy (wire version 0) and extended (wire version 1)
/// frames up to [`POHOLOS_MAX_FRAME_LEN`] bytes. On [`PoholosStatus::Ok`]
/// every field of `out_action` has been written (uninitialized storage is
/// fine); on any other status `out_action` is untouched.
///
/// Feed the manufacturer-data bytes that follow the `0xF10C` company id;
/// expect [`PoholosStatus::DecodeError`] routinely for foreign
/// advertisements that slip through scan filtering.
///
/// # Safety
/// `router` must be a live pointer from [`poholos_router_new`], not
/// accessed concurrently. `bytes` must point to `len` readable bytes
/// (null is allowed when `len` is 0). `out_action` must be valid for
/// writing one [`PoholosAction`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn poholos_router_ingest(
    router: *mut PoholosRouter,
    bytes: *const u8,
    len: usize,
    out_action: *mut PoholosAction,
) -> PoholosStatus {
    panic::catch_unwind(AssertUnwindSafe(|| {
        if router.is_null() || out_action.is_null() || (bytes.is_null() && len != 0) {
            return PoholosStatus::NullArgument;
        }
        let received = if len == 0 {
            &[]
        } else {
            // SAFETY: non-null and readable for `len` bytes per this
            // function's contract.
            unsafe { slice::from_raw_parts(bytes, len) }
        };
        // SAFETY: live and not aliased during the call per this function's
        // contract.
        let router = unsafe { &mut (*router).inner };
        match router.ingest::<POHOLOS_MAX_FRAME_LEN>(received) {
            Ok(action) => {
                // SAFETY: valid for writes per this function's contract.
                unsafe { out_action.write(flatten(&action)) };
                PoholosStatus::Ok
            }
            Err(_) => PoholosStatus::DecodeError,
        }
    }))
    .unwrap_or(PoholosStatus::Panic)
}

/// Derives the wire id of a node from its full display name, for showing
/// which peer an id belongs to. Returns 0 if `name` is null or not valid
/// UTF-8 (a real name hashing to 0 is possible in principle but
/// irrelevant at mesh scales).
///
/// # Safety
/// `name` must be null or a valid nul-terminated C string that outlives
/// the call.
#[unsafe(no_mangle)]
#[must_use]
pub unsafe extern "C" fn poholos_wire_id_of_name(name: *const c_char) -> u32 {
    panic::catch_unwind(|| {
        if name.is_null() {
            return 0;
        }
        // SAFETY: non-null and nul-terminated per this function's contract.
        unsafe { CStr::from_ptr(name) }
            .to_str()
            .map_or(0, |name| WireId::of_name(name).get())
    })
    .unwrap_or(0)
}

fn flatten(action: &ExtRouteAction) -> PoholosAction {
    let mut out = PoholosAction::EMPTY;
    match action {
        ExtRouteAction::Deliver(packet) => {
            out.kind = PoholosActionKind::Deliver;
            fill_packet(&mut out, packet);
        }
        ExtRouteAction::DeliverAndForward(packet, frame) => {
            out.kind = PoholosActionKind::DeliverAndForward;
            fill_packet(&mut out, packet);
            fill_frame(&mut out, frame.as_bytes());
        }
        ExtRouteAction::Forward(frame) => {
            out.kind = PoholosActionKind::Forward;
            fill_frame(&mut out, frame.as_bytes());
            // The relay frame was just encoded by the router, so it always
            // decodes; populating the packet fields lets passive monitors
            // display telegrams that are merely passing through.
            if let Ok(packet) = poholos::decode::<POHOLOS_MAX_FRAME_LEN>(frame.as_bytes()) {
                fill_packet(&mut out, &packet);
            }
        }
        ExtRouteAction::Ignore(reason) => {
            out.kind = PoholosActionKind::Ignore;
            out.ignore_reason = match reason {
                IgnoreReason::Own => PoholosIgnoreReason::Own,
                IgnoreReason::Duplicate => PoholosIgnoreReason::Duplicate,
                IgnoreReason::ExpiredTtl => PoholosIgnoreReason::ExpiredTtl,
                // `IgnoreReason` is non-exhaustive; new reasons still mean
                // "do nothing".
                _ => PoholosIgnoreReason::Other,
            };
        }
    }
    out
}

fn fill_packet(out: &mut PoholosAction, packet: &poholos::ExtPacket) {
    out.has_dest = packet.dest().is_some();
    out.ttl = packet.ttl();
    out.seq = packet.seq();
    out.src = packet.src().get();
    out.dest = packet.dest().map_or(0, WireId::get);
    let payload = packet.payload();
    out.payload[..payload.len()].copy_from_slice(payload);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "payload.len() <= POHOLOS_MAX_PAYLOAD_LEN (204) by packet construction"
    )]
    {
        out.payload_len = payload.len() as u16;
    }
}

fn fill_frame(out: &mut PoholosAction, frame: &[u8]) {
    out.frame[..frame.len()].copy_from_slice(frame);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "frame.len() <= POHOLOS_MAX_FRAME_LEN (211) by frame construction"
    )]
    {
        out.frame_len = frame.len() as u16;
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;

    use poholos::{ExtPacket, Packet, encode};

    use super::*;

    const MONITOR_NAME: &str = "iphone-3f2a";

    /// RAII wrapper so tests can't leak or double-free router handles.
    struct TestRouter(*mut PoholosRouter);

    impl TestRouter {
        fn new(name: &str) -> Self {
            let name = CString::new(name).unwrap();
            // SAFETY: `name` is a valid C string live across the call.
            let router = unsafe { poholos_router_new(name.as_ptr()) };
            assert!(!router.is_null());
            Self(router)
        }

        fn ingest(&self, bytes: &[u8]) -> (PoholosStatus, PoholosAction) {
            let mut action = PoholosAction::EMPTY;
            // SAFETY: the router is live, `bytes` spans `bytes.len()`
            // readable bytes, and `action` is writable.
            let status = unsafe {
                poholos_router_ingest(self.0, bytes.as_ptr(), bytes.len(), &raw mut action)
            };
            (status, action)
        }
    }

    impl Drop for TestRouter {
        fn drop(&mut self) {
            // SAFETY: created by `poholos_router_new`, freed exactly once.
            unsafe { poholos_router_free(self.0) };
        }
    }

    fn wire_id(name: &str) -> u32 {
        let name = CString::new(name).unwrap();
        // SAFETY: `name` is a valid C string live across the call.
        unsafe { poholos_wire_id_of_name(name.as_ptr()) }
    }

    #[test]
    fn wire_id_of_name_matches_core_derivation() {
        assert_eq!(wire_id("alice-3f2a"), WireId::of_name("alice-3f2a").get());
        assert_ne!(wire_id("alice-3f2a"), wire_id("bob-9c01"));
    }

    #[test]
    fn wire_id_of_name_rejects_null_and_invalid_utf8() {
        // SAFETY: null is explicitly allowed by the contract.
        assert_eq!(unsafe { poholos_wire_id_of_name(ptr::null()) }, 0);

        let invalid = CStr::from_bytes_with_nul(&[0xFF, 0]).unwrap();
        // SAFETY: `invalid` is a valid C string live across the call.
        assert_eq!(unsafe { poholos_wire_id_of_name(invalid.as_ptr()) }, 0);
    }

    #[test]
    fn router_new_rejects_null_and_invalid_utf8() {
        // SAFETY: null is explicitly allowed by the contract.
        assert!(unsafe { poholos_router_new(ptr::null()) }.is_null());

        let invalid = CStr::from_bytes_with_nul(&[0xFF, 0]).unwrap();
        // SAFETY: `invalid` is a valid C string live across the call.
        assert!(unsafe { poholos_router_new(invalid.as_ptr()) }.is_null());
    }

    #[test]
    fn router_free_of_null_is_a_no_op() {
        // SAFETY: null is explicitly allowed by the contract.
        unsafe { poholos_router_free(ptr::null_mut()) };
    }

    #[test]
    fn hearsay_delivers_and_forwards() {
        let router = TestRouter::new(MONITOR_NAME);
        let src = WireId::of_name("alice-3f2a");
        let frame = encode(&Packet::hearsay_with(src, 7, b"hi mesh", 16).unwrap());

        let (status, action) = router.ingest(frame.as_bytes());
        assert_eq!(status, PoholosStatus::Ok);
        assert_eq!(action.kind, PoholosActionKind::DeliverAndForward);
        assert_eq!(action.ignore_reason, PoholosIgnoreReason::None);
        assert!(!action.has_dest);
        assert_eq!(action.src, src.get());
        assert_eq!(action.seq, 7);
        assert_eq!(action.ttl, 16, "delivered copy keeps the received TTL");
        assert_eq!(&action.payload[..action.payload_len as usize], b"hi mesh");

        let relayed = &action.frame[..action.frame_len as usize];
        let relayed = poholos::decode::<POHOLOS_MAX_FRAME_LEN>(relayed).unwrap();
        assert_eq!(relayed.ttl(), 15, "relay frame carries the decremented TTL");
        assert_eq!(relayed.payload(), b"hi mesh");
    }

    #[test]
    fn telegram_for_us_is_delivered_without_forwarding() {
        let router = TestRouter::new(MONITOR_NAME);
        let src = WireId::of_name("alice-3f2a");
        let us = WireId::of_name(MONITOR_NAME);
        let frame = encode(&Packet::telegram(src, us, 1, b"psst").unwrap());

        let (status, action) = router.ingest(frame.as_bytes());
        assert_eq!(status, PoholosStatus::Ok);
        assert_eq!(action.kind, PoholosActionKind::Deliver);
        assert!(action.has_dest);
        assert_eq!(action.dest, us.get());
        assert_eq!(&action.payload[..action.payload_len as usize], b"psst");
        assert_eq!(action.frame_len, 0, "the destination consumes the telegram");
    }

    #[test]
    fn telegram_for_other_is_forwarded_with_packet_fields() {
        let router = TestRouter::new(MONITOR_NAME);
        let src = WireId::of_name("alice-3f2a");
        let dest = WireId::of_name("bob-9c01");
        let frame = encode(&Packet::telegram_with(src, dest, 1, b"relay me", 5).unwrap());

        let (status, action) = router.ingest(frame.as_bytes());
        assert_eq!(status, PoholosStatus::Ok);
        assert_eq!(action.kind, PoholosActionKind::Forward);
        assert!(action.frame_len > 0);

        // The in-transit packet fields are populated for monitoring, with
        // the TTL already decremented for the next hop.
        assert!(action.has_dest);
        assert_eq!(action.src, src.get());
        assert_eq!(action.dest, dest.get());
        assert_eq!(action.ttl, 4);
        assert_eq!(&action.payload[..action.payload_len as usize], b"relay me");
    }

    #[test]
    fn duplicate_via_second_route_is_ignored() {
        let router = TestRouter::new(MONITOR_NAME);
        let src = WireId::of_name("alice-3f2a");
        // Same message, different remaining TTLs (two paths through the mesh).
        let first = encode(&Packet::hearsay_with(src, 1, b"hi", 16).unwrap());
        let second = encode(&Packet::hearsay_with(src, 1, b"hi", 9).unwrap());

        let (status, action) = router.ingest(first.as_bytes());
        assert_eq!(status, PoholosStatus::Ok);
        assert_eq!(action.kind, PoholosActionKind::DeliverAndForward);

        let (status, action) = router.ingest(second.as_bytes());
        assert_eq!(status, PoholosStatus::Ok);
        assert_eq!(action.kind, PoholosActionKind::Ignore);
        assert_eq!(action.ignore_reason, PoholosIgnoreReason::Duplicate);
    }

    #[test]
    fn own_echo_is_ignored() {
        let router = TestRouter::new(MONITOR_NAME);
        let us = WireId::of_name(MONITOR_NAME);
        let frame = encode(&Packet::hearsay(us, 1, b"echo").unwrap());

        let (status, action) = router.ingest(frame.as_bytes());
        assert_eq!(status, PoholosStatus::Ok);
        assert_eq!(action.kind, PoholosActionKind::Ignore);
        assert_eq!(action.ignore_reason, PoholosIgnoreReason::Own);
    }

    #[test]
    fn expired_telegram_reports_expired_ttl() {
        let router = TestRouter::new(MONITOR_NAME);
        let src = WireId::of_name("alice-3f2a");
        let dest = WireId::of_name("bob-9c01");
        let frame = encode(&Packet::telegram_with(src, dest, 1, b"late", 1).unwrap());

        let (status, action) = router.ingest(frame.as_bytes());
        assert_eq!(status, PoholosStatus::Ok);
        assert_eq!(action.kind, PoholosActionKind::Ignore);
        assert_eq!(action.ignore_reason, PoholosIgnoreReason::ExpiredTtl);
    }

    #[test]
    fn hearsay_at_ttl_one_delivers_without_forwarding() {
        let router = TestRouter::new(MONITOR_NAME);
        let src = WireId::of_name("alice-3f2a");
        let frame = encode(&Packet::hearsay_with(src, 1, b"last hop", 1).unwrap());

        let (status, action) = router.ingest(frame.as_bytes());
        assert_eq!(status, PoholosStatus::Ok);
        assert_eq!(action.kind, PoholosActionKind::Deliver);
        assert_eq!(action.frame_len, 0);
    }

    #[test]
    fn extended_frame_round_trips_a_long_payload() {
        let router = TestRouter::new(MONITOR_NAME);
        let src = WireId::of_name("alice-3f2a");
        let payload = [0x5A; POHOLOS_MAX_PAYLOAD_LEN];
        let frame = encode(&ExtPacket::hearsay_with(src, 1, &payload, 16).unwrap());
        assert!(frame.len() > poholos::MAX_FRAME_LEN, "wire version 1 frame");

        let (status, action) = router.ingest(frame.as_bytes());
        assert_eq!(status, PoholosStatus::Ok);
        assert_eq!(action.kind, PoholosActionKind::DeliverAndForward);
        assert_eq!(usize::from(action.payload_len), POHOLOS_MAX_PAYLOAD_LEN);
        assert_eq!(&action.payload[..], &payload[..]);
    }

    #[test]
    fn undecodable_bytes_are_decode_errors() {
        let router = TestRouter::new(MONITOR_NAME);
        // Empty, truncated, and oversized inputs.
        assert_eq!(router.ingest(&[]).0, PoholosStatus::DecodeError);
        assert_eq!(router.ingest(&[0x10, 0x00]).0, PoholosStatus::DecodeError);
        assert_eq!(
            router.ingest(&[0u8; POHOLOS_MAX_FRAME_LEN + 1]).0,
            PoholosStatus::DecodeError
        );
        // Reserved wire version (2).
        let mut frame = [0u8; 8];
        frame[0] = 0b1000_0000 | 5;
        assert_eq!(router.ingest(&frame).0, PoholosStatus::DecodeError);
    }

    #[test]
    fn null_arguments_are_rejected() {
        let router = TestRouter::new(MONITOR_NAME);
        let frame = encode(&Packet::hearsay(WireId::new(1), 1, b"x").unwrap());
        let bytes = frame.as_bytes();
        let mut action = PoholosAction::EMPTY;

        // SAFETY: each call passes exactly one null where the contract
        // requires non-null; all other arguments are valid.
        unsafe {
            assert_eq!(
                poholos_router_ingest(
                    ptr::null_mut(),
                    bytes.as_ptr(),
                    bytes.len(),
                    &raw mut action
                ),
                PoholosStatus::NullArgument
            );
            assert_eq!(
                poholos_router_ingest(router.0, ptr::null(), bytes.len(), &raw mut action),
                PoholosStatus::NullArgument
            );
            assert_eq!(
                poholos_router_ingest(router.0, bytes.as_ptr(), bytes.len(), ptr::null_mut()),
                PoholosStatus::NullArgument
            );
        }
    }
}
