use super::*;
use std::time::Duration;

fn parse_netstat_cidr(destination: &str) -> Option<Cidr> {
    if destination == "default" {
        return Cidr::new(Ipv4Addr::UNSPECIFIED, 0).ok();
    }
    let (ip, prefix) = if let Some((ip, prefix)) = destination.split_once('/') {
        (ip, Some(prefix.parse().ok()?))
    } else {
        (destination, None)
    };
    let mut octets = [0u8; 4];
    let mut length = 0;
    for (i, v) in ip.split('.').enumerate() {
        *octets.get_mut(i)? = v.parse().ok()?;
        length += 1;
    }
    Cidr::new(octets.into(), prefix.unwrap_or(8 * length)).ok()
}

#[test]
fn sysctl_route_snapshot_matches_live_macos_network() {
    let apple = Apple::new(None);
    let t0 = std::time::Instant::now();
    let net = apple.network().unwrap();
    let elapsed = t0.elapsed();

    let table = command("/usr/sbin/netstat", &["-rn", "-f", "inet"]).unwrap();
    let mut expected_routes = Vec::new();
    let mut expected_identity = Vec::new();
    for line in table.lines() {
        let row: Vec<_> = line.split_whitespace().collect();
        if row.len() < 4 || row[2].contains('W') || !row[2].contains('U') {
            continue;
        }
        let Some(cidr) = parse_netstat_cidr(row[0]) else {
            continue;
        };
        let name = std::ffi::CString::new(row[3]).unwrap();
        let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
        if index == 0 {
            continue;
        }
        if cidr.prefix == 0 && !row[3].starts_with("utun") {
            expected_identity.push(format!("{}:{}", row[3], row[1]));
        }
        expected_routes.push(Underlay {
            cidr,
            index,
            metric: if row[2].contains('I') { 1 } else { 0 },
        });
    }
    expected_routes.sort_unstable_by_key(|r| (r.cidr.ip, r.cidr.prefix, r.metric, r.index));
    expected_routes.dedup();
    expected_identity.sort_unstable();

    let expected_marker = if !expected_identity.is_empty() && !net.addresses.is_empty() {
        format!("{expected_identity:?}/0")
    } else {
        String::new()
    };
    eprintln!(
        "sysctl_route_snapshot elapsed={elapsed:?}, routes={} (netstat={})",
        net.routes.len(),
        expected_routes.len()
    );
    assert_eq!(net.marker, expected_marker);
    assert_eq!(net.routes, expected_routes);
    assert!(
        elapsed < Duration::from_millis(50),
        "sysctl network snapshot took too long: {elapsed:?}"
    );
}
