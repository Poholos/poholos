// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

//! The interactive chat loop: stdin in, transport frames in, decisions out.

use anyhow::Result;
use poholos::{
    Frame, MAX_PAYLOAD_HEARSAY, MAX_PAYLOAD_TELEGRAM, NodeId, Packet, RouteAction, Router, WireId,
};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::transport::Transport;

/// Runs the chat session until `/quit` or stdin closes.
///
/// Two event sources are multiplexed with `tokio::select!`:
///
/// * lines typed by the user - turned into hearsay (broadcast) or, with
///   `@node-xxxx message` syntax, telegram (unicast) packets;
/// * frames from the transport - fed to the [`Router`], which decides
///   whether to print, re-broadcast, both, or neither.
pub async fn run(node: NodeId, mut transport: Transport) -> Result<()> {
    let mut router = Router::new(node.wire_id());
    // Start at a random sequence number so a restarted node does not
    // collide with its pre-restart packets in peers' seen caches.
    let mut seq: u16 = rand::random();

    println!("you are {node} (wire id {})", node.wire_id());
    println!("type to broadcast, '@node-xxxx message' for unicast, '/quit' to exit");

    let mut lines = BufReader::new(tokio::io::stdin()).lines();

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { break }; // stdin closed
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if line == "/quit" {
                    break;
                }
                match parse_outgoing(&mut seq, router.local(), line) {
                    Ok(packet) => {
                        let frame = router.originate(&packet);
                        if let Err(e) = transport.send_own(&frame).await {
                            eprintln!("! send failed: {e:#}");
                        }
                    }
                    Err(reason) => eprintln!("! {reason}"),
                }
            }
            frame = transport.recv() => {
                let Some(frame) = frame else {
                    eprintln!("! transport closed");
                    break;
                };
                match router.ingest(frame.as_bytes()) {
                    Ok(RouteAction::Deliver(p)) => print_delivery(&p, router.local()),
                    Ok(RouteAction::DeliverAndForward(p, relay)) => {
                        print_delivery(&p, router.local());
                        relay_frame(&mut transport, &relay).await;
                    }
                    Ok(RouteAction::Forward(relay)) => {
                        relay_frame(&mut transport, &relay).await;
                    }
                    // Routine radio noise, stay quiet: duplicates, own
                    // echoes, expired telegrams (Ignore), and foreign or
                    // corrupt advertisements that slipped through
                    // transport filtering (Err).
                    Ok(RouteAction::Ignore(_)) | Err(_) => {}
                }
            }
        }
    }

    println!("bye");
    Ok(())
}

/// Re-broadcasts a frame the router asked us to forward.
///
/// The transport may skip frames it cannot carry (a Mac hearing a full
/// 22-byte frame it can never re-advertise) - that is routine capability
/// mismatch, handled silently inside `send_relay`, not an error here.
async fn relay_frame(transport: &mut Transport, relay: &Frame) {
    if let Err(e) = transport.send_relay(relay).await {
        eprintln!("! relay failed: {e:#}");
    }
}

/// Parses a typed line into an outgoing packet, advancing `seq`.
fn parse_outgoing(seq: &mut u16, src: WireId, line: &str) -> Result<Packet, String> {
    let next_seq = *seq;

    let packet = if let Some(rest) = line.strip_prefix('@') {
        let Some((dest_name, message)) = rest.split_once(' ') else {
            return Err("unicast syntax: @node-xxxx message".to_owned());
        };
        let message = message.trim();
        if message.is_empty() {
            return Err("unicast syntax: @node-xxxx message".to_owned());
        }
        if message.len() > MAX_PAYLOAD_TELEGRAM {
            return Err(format!(
                "message is {} bytes; telegrams carry at most {MAX_PAYLOAD_TELEGRAM} \
                 bytes in this MVP — send several shorter ones",
                message.len()
            ));
        }
        let dest = WireId::of_name(dest_name);
        Packet::telegram(src, dest, next_seq, message.as_bytes())
    } else {
        if line.len() > MAX_PAYLOAD_HEARSAY {
            return Err(format!(
                "message is {} bytes; broadcasts carry at most {MAX_PAYLOAD_HEARSAY} \
                 bytes in this MVP — send several shorter ones",
                line.len()
            ));
        }
        Packet::hearsay(src, next_seq, line.as_bytes())
    }
    .map_err(|e| e.to_string())?;

    *seq = seq.wrapping_add(1);
    Ok(packet)
}

fn print_delivery(packet: &Packet, local: WireId) {
    let text = String::from_utf8_lossy(packet.payload());
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    // A telegram for another node never reaches print_delivery (the
    // router forwards it without delivering), so anything without our
    // dest is hearsay addressed to everyone.
    let to = if packet.dest() == Some(local) {
        "you"
    } else {
        "all"
    };
    println!("{now} [{} \u{2192} {to}] {text}", packet.src());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_line_becomes_hearsay() {
        let mut seq = 5;
        let src = WireId::new(1);
        let p = parse_outgoing(&mut seq, src, "hello").unwrap();
        assert!(!p.is_telegram());
        assert_eq!(p.seq(), 5);
        assert_eq!(p.payload(), b"hello");
        assert_eq!(seq, 6);
    }

    #[test]
    fn at_prefix_becomes_telegram_with_derived_dest() {
        let mut seq = 0;
        let src = WireId::new(1);
        let p = parse_outgoing(&mut seq, src, "@bob-9c01 hi").unwrap();
        assert_eq!(p.dest(), Some(WireId::of_name("bob-9c01")));
        assert_eq!(p.payload(), b"hi");
    }

    #[test]
    fn overlong_messages_are_rejected_with_hint() {
        let mut seq = 0;
        let src = WireId::new(1);
        let err = parse_outgoing(&mut seq, src, "sixteen bytes!!!").unwrap_err();
        assert!(err.contains("15"));
        assert_eq!(seq, 0, "seq not consumed on rejection");

        let err = parse_outgoing(&mut seq, src, "@bob-9c01 twelve bytes!").unwrap_err();
        assert!(err.contains("11"));
    }

    #[test]
    fn bare_at_line_is_a_syntax_error() {
        let mut seq = 0;
        parse_outgoing(&mut seq, WireId::new(1), "@bob-9c01").unwrap_err();
    }

    #[test]
    fn empty_telegram_message_is_rejected() {
        let mut seq = 0;
        parse_outgoing(&mut seq, WireId::new(1), "@bob-9c01    ").unwrap_err();
        assert_eq!(seq, 0, "seq not consumed on rejection");
    }
}
