// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

//! Poholos console chat: a P2P mesh chat over BLE advertising frames.

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use mimalloc::MiMalloc;
use poholos::NodeId;

mod app;
mod transport;

// M-MIMALLOC-APPS: applications set mimalloc as the global allocator.
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// Default UDP port for the LAN test transport.
///
/// 61708 == 0xF10C, the poholos BLE company id — memorable and far from
/// well-known ports. Change freely; it only affects the UDP transport.
const DEFAULT_UDP_PORT: u16 = 61_708;

#[derive(Parser, Debug)]
#[command(name = "poholos", version, about = "P2P Bluetooth mesh console chat")]
struct Args {
    /// Your chosen name (1-16 chars of [a-z0-9-]); a unique 4-hex suffix
    /// is appended automatically, e.g. `alice` becomes `alice-3f2a`.
    #[arg(long, required_unless_present = "id", conflicts_with = "id")]
    name: Option<String>,

    /// Full node id including the 4-hex suffix, e.g. `alice-3f2a`: keeps
    /// a stable identity across restarts (so peers — including embedded
    /// nodes with a baked-in buddy — can address you reliably).
    #[arg(long)]
    id: Option<String>,

    /// Which transport to use.
    #[arg(long, value_enum, default_value_t = TransportKind::Ble)]
    transport: TransportKind,

    /// UDP port for the `udp` transport (LAN broadcast testing).
    #[arg(long, default_value_t = DEFAULT_UDP_PORT)]
    port: u16,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum TransportKind {
    /// Bluetooth Low Energy advertising (the real thing).
    Ble,
    /// UDP broadcast on the local network (for protocol testing).
    Udp,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let node = match (&args.id, &args.name) {
        (Some(id), _) => NodeId::parse(id).with_context(|| format!("invalid node id {id:?}"))?,
        (None, Some(name)) => NodeId::new(name, rand::random())
            .with_context(|| format!("invalid node name {name:?}"))?,
        // clap enforces exactly one of --name/--id.
        (None, None) => unreachable!("clap requires --name or --id"),
    };

    let transport = match args.transport {
        TransportKind::Udp => transport::Transport::udp(args.port)
            .await
            .context("failed to start UDP transport")?,
        TransportKind::Ble => transport::Transport::ble()
            .await
            .context("failed to start BLE transport")?,
    };

    app::run(node, transport).await
}
