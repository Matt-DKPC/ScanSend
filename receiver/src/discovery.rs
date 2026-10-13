//! Answers "who's out there?" queries from phones on the LAN.

use anyhow::Result;
use scansend_proto::{Discovery, DISCOVERY_PORT, MULTICAST_ADDR, PROTOCOL_VERSION};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{Ipv4Addr, SocketAddr};
use tokio::net::UdpSocket;

fn bind() -> Result<UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    s.set_reuse_port(true)?;
    s.set_broadcast(true)?;
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT)).into())?;

    // Join the group on every IPv4 interface so a PC with Wi-Fi + Ethernet
    // (or a VPN adapter grabbing the default route) still hears phones.
    let mut joined = 0;
    if let Ok(ifs) = local_ip_address::list_afinet_netifas() {
        for (_, ip) in ifs {
            if let std::net::IpAddr::V4(v4) = ip {
                if !v4.is_loopback() && s.join_multicast_v4(&MULTICAST_ADDR, &v4).is_ok() {
                    joined += 1;
                }
            }
        }
    }
    if joined == 0 {
        s.join_multicast_v4(&MULTICAST_ADDR, &Ipv4Addr::UNSPECIFIED)?;
    }
    s.set_nonblocking(true)?;
    Ok(UdpSocket::from_std(s.into())?)
}

pub async fn run(id: String, name: String, http_port: u16) -> Result<()> {
    let sock = bind()?;
    let reply = Discovery::Announce {
        v: PROTOCOL_VERSION,
        id,
        name,
        port: http_port,
        device: Some(crate::net::device_name()),
        os: Some(scansend_proto::os_label().into()),
    }
    .to_bytes();
    let mut buf = [0u8; 1024];
    loop {
        let (n, from) = match sock.recv_from(&mut buf).await {
            Ok(x) => x,
            Err(_) => continue, // e.g. Windows ICMP "connection reset" noise
        };
        if let Some(Discovery::Query { .. }) = Discovery::from_bytes(&buf[..n]) {
            let _ = sock.send_to(&reply, from).await;
        }
    }
}
