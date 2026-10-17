//! Pretends to be the phone app, for testing a receiver without a phone.
//!
//!   cargo run -p scansend-receiver --example fake_phone -- <PIN> "text to send" [more texts...]

use scansend_proto::*;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let pin = args.next().expect("usage: fake_phone <PIN> <text>...");
    let texts: Vec<String> = args.collect();

    // 1. Discovery, exactly as the phone does it.
    let sock = tokio::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    sock.set_broadcast(true)?;
    let q = Discovery::query().to_bytes();
    for group in MULTICAST_GROUPS {
        sock.send_to(&q, (group, DISCOVERY_PORT)).await?;
    }
    let _ = sock.send_to(&q, (Ipv4Addr::BROADCAST, DISCOVERY_PORT)).await;
    let _ = sock.send_to(&q, (Ipv4Addr::LOCALHOST, DISCOVERY_PORT)).await;
    let mut buf = [0u8; 1024];
    let (n, from) = tokio::time::timeout(Duration::from_secs(2), sock.recv_from(&mut buf)).await??;
    let Some(Discovery::Announce { name, port, id, .. }) = Discovery::from_bytes(&buf[..n]) else {
        anyhow::bail!("bad announce");
    };
    let base = format!("http://{}", SocketAddr::new(from.ip(), port));
    println!("found \"{name}\" (id {id}) at {base}");

    // 2. Pair.
    let http = reqwest::Client::new();
    let r = http
        .post(format!("{base}/api/v1/pair"))
        .json(&PairRequest { pin, device_name: "fake phone".into(), phone_id: None })
        .send()
        .await?;
    println!("pair -> {}", r.status());
    let pair: PairResponse = r.error_for_status()?.json().await?;

    // 3. Send scans (each twice with the same id to check de-duplication).
    for (i, text) in texts.into_iter().enumerate() {
        for _ in 0..2 {
            let r = http
                .post(format!("{base}/api/v1/scan"))
                .bearer_auth(&pair.token)
                .json(&Scan { text: text.clone(), format: Some("QR_CODE".into()), id: Some(format!("t{i}")), device_name: None })
                .send()
                .await?;
            println!("scan {text:?} -> {}", r.status());
        }
    }

    // 4. A bad token must be rejected.
    let r = http
        .post(format!("{base}/api/v1/scan"))
        .bearer_auth("nope")
        .json(&Scan { text: "x".into(), format: None, id: None, device_name: None })
        .send()
        .await?;
    println!("bad token -> {}", r.status());
    Ok(())
}
