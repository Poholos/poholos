// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

//! UDP broadcast transport: the protocol over `255.255.255.255` for testing.
//!
//! This transport exists so the whole stack - packets, wire format, router,
//! chat loop - can be exercised on a normal LAN (or even one machine with
//! several terminals) without any Bluetooth hardware. Frames are sent as
//! single datagrams to the IPv4 broadcast address and received on the same
//! port.
//!
//! Note that broadcast datagrams loop back to their sender on most stacks;
//! the router discards those as `Own` echoes.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};
use poholos::{Frame, MAX_FRAME_LEN};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

/// How many received frames may queue up before the reader applies
/// backpressure. Frames are 22 bytes and the chat loop drains fast, so a
/// small buffer is plenty; raising it only delays detection of a stuck UI.
const RECV_QUEUE: usize = 64;

/// UDP broadcast transport bound to a fixed port.
#[derive(Debug)]
pub struct UdpTransport {
    socket: Arc<UdpSocket>,
    rx: mpsc::Receiver<Frame>,
    port: u16,
}

impl UdpTransport {
    /// Binds `0.0.0.0:port`, enables broadcast, and starts the reader task.
    ///
    /// The socket is created with `SO_REUSEADDR` (and `SO_REUSEPORT` on
    /// Linux) *before* binding, so several poholos instances on one machine
    /// can share the port and all hear each other's broadcasts — the
    /// two-terminals quick start depends on it. macOS achieves the same
    /// effect with `SO_REUSEADDR` alone (BSD semantics for UDP).
    ///
    /// # Errors
    /// Fails if the socket cannot be created or bound, or broadcast cannot
    /// be enabled.
    #[expect(
        clippy::unused_async,
        reason = "transport constructors share an async signature; BLE awaits"
    )]
    pub async fn new(port: u16) -> Result<Self> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .context("creating UDP socket")?;
        socket
            .set_reuse_address(true)
            .context("enabling SO_REUSEADDR")?;
        #[cfg(target_os = "linux")]
        socket
            .set_reuse_port(true)
            .context("enabling SO_REUSEPORT")?;
        socket
            .set_nonblocking(true)
            .context("setting the socket non-blocking")?;
        socket
            .bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())
            .with_context(|| format!("binding UDP 0.0.0.0:{port}"))?;

        let socket =
            UdpSocket::from_std(socket.into()).context("registering the socket with tokio")?;
        socket
            .set_broadcast(true)
            .context("enabling SO_BROADCAST")?;

        let socket = Arc::new(socket);
        let (tx, rx) = mpsc::channel(RECV_QUEUE);
        tokio::spawn(read_loop(Arc::clone(&socket), tx));

        Ok(Self { socket, rx, port })
    }

    /// Broadcasts `frame` as one datagram.
    ///
    /// # Errors
    /// Fails on socket errors.
    pub async fn send(&mut self, frame: &Frame) -> Result<()> {
        self.socket
            .send_to(frame.as_bytes(), (Ipv4Addr::BROADCAST, self.port))
            .await
            .context("UDP broadcast send")?;
        Ok(())
    }

    /// Waits for the next frame; `None` if the reader task died.
    pub async fn recv(&mut self) -> Option<Frame> {
        self.rx.recv().await
    }
}

/// How many `recv_from` failures in a row we tolerate before concluding
/// the socket is permanently broken. Transient errors arrive in ones and
/// twos; a dead socket fails every call.
const MAX_CONSECUTIVE_RECV_ERRORS: u32 = 64;

/// Receives datagrams forever, forwarding plausible frames to the channel.
async fn read_loop(socket: Arc<UdpSocket>, tx: mpsc::Sender<Frame>) {
    // One byte larger than a valid frame so oversized datagrams are
    // detectable: Linux truncates them to the buffer (caught by the frame
    // length check below), Windows fails the recv with WSAEMSGSIZE
    // (caught by the error arm below).
    let mut buf = [0_u8; MAX_FRAME_LEN + 1];
    let mut consecutive_errors = 0_u32;
    loop {
        // Recv errors are routine on Windows: WSAEMSGSIZE for oversized
        // foreign datagrams on our port, WSAECONNRESET bounced back via
        // ICMP after our own broadcasts. Neither means the socket is
        // dead, so keep receiving — but give up if *nothing* succeeds
        // anymore, which signals the chat loop via channel drop.
        let Ok((n, _peer)) = socket.recv_from(&mut buf).await else {
            consecutive_errors += 1;
            if consecutive_errors >= MAX_CONSECUTIVE_RECV_ERRORS {
                return;
            }
            continue;
        };
        consecutive_errors = 0;
        // Foreign datagrams on our port that are not frame-sized are not
        // ours to interpret; drop quietly like radio noise.
        let Ok(frame) = Frame::copy_from(&buf[..n]) else {
            continue;
        };
        if tx.send(frame).await.is_err() {
            return; // transport dropped
        }
    }
}
