//! Only the user running TuxDeck may talk to its ports. They listen on 127.0.0.1, where
//! every local user can connect; tokens, login codes and plugin secrets don't keep other
//! users out, because command lines (where some of them have to be) are readable for all.
//! So each connection's other end is looked up in `/proc/net/tcp`: the socket's owner must
//! be our own user.

use axum::extract::{ConnectInfo, Request};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::Router;
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

/// `router` for `listener`: requests from other users are refused (403). Serve the result
/// with `axum::serve`.
pub fn guard(router: Router, local: SocketAddr) -> axum::extract::connect_info::IntoMakeServiceWithConnectInfo<Router, SocketAddr> {
    router
        .layer(axum::middleware::from_fn(move |ConnectInfo(peer): ConnectInfo<SocketAddr>, req: Request, next: Next| async move {
            if same_user(peer, local) {
                next.run(req).await
            } else {
                tracing::warn!("refused a connection of another user ({peer})");
                StatusCode::FORBIDDEN.into_response()
            }
        }))
        .into_make_service_with_connect_info::<SocketAddr>()
}

/// Whether the socket at `peer`, connected to our `local` address, belongs to our user.
pub fn same_user(peer: SocketAddr, local: SocketAddr) -> bool {
    type Seen = HashMap<(SocketAddr, SocketAddr), Instant>;
    // A connection makes many requests: remember an answer for a moment.
    static SEEN: parking_lot::Mutex<Option<Seen>> = parking_lot::Mutex::new(None);
    let key = (peer, local);
    {
        let mut seen = SEEN.lock();
        let map = seen.get_or_insert_with(HashMap::new);
        map.retain(|_, at| at.elapsed() < Duration::from_secs(2));
        if map.contains_key(&key) {
            return true;
        }
    }
    let me = unsafe { libc::getuid() };
    let ok = match std::fs::read_to_string("/proc/net/tcp") {
        Ok(table) => socket_uid(&table, peer, local) == Some(me),
        // No /proc (not Linux): nothing to check against.
        Err(_) => true,
    };
    if ok {
        SEEN.lock().get_or_insert_with(HashMap::new).insert(key, Instant::now());
    }
    ok
}

/// Owner of the socket with local address `peer` and remote address `local` in a
/// `/proc/net/tcp` table (IPv4).
fn socket_uid(table: &str, peer: SocketAddr, local: SocketAddr) -> Option<u32> {
    let (SocketAddr::V4(peer), SocketAddr::V4(local)) = (peer, local) else { return None };
    table.lines().skip(1).find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        (f.len() > 7 && addr(f[1]) == Some(peer) && addr(f[2]) == Some(local)).then(|| f[7].parse().ok()).flatten()
    })
}

/// `0100007F:1F90` → 127.0.0.1:8080 (the address is the kernel's 32-bit value in hex, in
/// host byte order; the port in hex).
fn addr(s: &str) -> Option<SocketAddrV4> {
    let (ip, port) = s.split_once(':')?;
    let ip = u32::from_str_radix(ip, 16).ok()?;
    Some(SocketAddrV4::new(Ipv4Addr::from(u32::from_be(ip)), u16::from_str_radix(port, 16).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_of_a_connection() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
           0: 0100007F:BAB8 0100007F:0050 01 00000000:00000000 00:00000000 00000000  1001        0 1 1 0000000000000000 20 4 30 10 -1\n\
           1: 0100007F:0050 0100007F:BAB8 01 00000000:00000000 00:00000000 00000000  1001        0 2 1 0000000000000000 20 4 30 10 -1\n\
           2: 0100007F:C000 0100007F:0050 01 00000000:00000000 00:00000000 00000000  1002        0 3 1 0000000000000000 20 4 30 10 -1\n";
        let server: SocketAddr = "127.0.0.1:80".parse().unwrap();
        if cfg!(target_endian = "little") {
            assert_eq!(socket_uid(table, "127.0.0.1:47800".parse().unwrap(), server), Some(1001));
            assert_eq!(socket_uid(table, "127.0.0.1:49152".parse().unwrap(), server), Some(1002));
        }
        assert_eq!(socket_uid(table, "127.0.0.1:1234".parse().unwrap(), server), None);
        assert_eq!(socket_uid(table, "[::1]:47800".parse().unwrap(), server), None);
    }

    #[test]
    fn a_real_connection_is_ours() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let local = l.local_addr().unwrap();
        let _c = std::net::TcpStream::connect(local).unwrap();
        let (_s, peer) = l.accept().unwrap();
        assert!(same_user(peer, local));
        // An address nobody uses has no owner.
        assert!(!same_user("127.0.0.1:1".parse().unwrap(), local));
    }
}
