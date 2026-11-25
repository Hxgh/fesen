use serde::{Deserialize, Serialize};
use std::{fmt::Display, net::Ipv4Addr, str::FromStr};

pub const MAX_ADDRESSES: u64 = 65_536;
pub const MAX_RANGES: usize = 32;
pub const VIRTUAL: Range<Ipv4Addr> = Range {
    start: Ipv4Addr::new(198, 18, 0, 0),
    end: Ipv4Addr::new(198, 19, 255, 255),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range<T> {
    pub start: T,
    pub end: T,
}

impl<T: Copy + Ord> Range<T> {
    pub fn contains(&self, value: T) -> bool {
        self.start <= value && value <= self.end
    }

    pub fn intersects(&self, other: &Self) -> bool {
        self.start <= other.end && other.start <= self.end
    }
}

impl<T: Display + PartialEq> Display for Range<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.start)?;
        if self.start != self.end {
            write!(f, "-{}", self.end)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rules {
    pub ips: Vec<Range<Ipv4Addr>>,
    pub ports: Vec<Range<u16>>,
}

impl Rules {
    pub fn parse(ips: &str, ports: &str) -> Result<Self, &'static str> {
        let items = |s: &str| {
            s.split([',', '，', '\n'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let value = Self {
            ips: items(ips)
                .iter()
                .map(|s| {
                    if s.contains('/') {
                        s.parse::<Cidr>().map(Cidr::range)
                    } else {
                        parse_range(s, "IPv4 地址格式错误")
                    }
                })
                .collect::<Result<_, _>>()?,
            ports: items(ports)
                .iter()
                .map(|s| parse_range(s, "端口格式错误"))
                .collect::<Result<_, _>>()?,
        };
        value.checked()
    }

    pub fn checked(mut self) -> Result<Self, &'static str> {
        if self.ips.is_empty()
            || self.ports.is_empty()
            || self.ips.len() > MAX_RANGES
            || self.ports.len() > MAX_RANGES
        {
            return Err("地址和端口分别需要 1–32 段");
        }
        if self
            .ips
            .iter()
            .any(|r| r.start > r.end || RESERVED.iter().any(|v| v.intersects(r)))
        {
            return Err("地址范围无效或包含保留地址");
        }
        if self.ports.iter().any(|r| r.start == 0 || r.start > r.end) {
            return Err("端口范围无效");
        }
        let ips = union(
            self.ips
                .iter()
                .map(|r| (u32::from(r.start), u32::from(r.end))),
        );
        if count(&ips) > MAX_ADDRESSES {
            return Err("共享的 IPv4 地址不能超过 65536 个");
        }
        self.ips = ips
            .into_iter()
            .map(|(start, end)| Range {
                start: start.into(),
                end: end.into(),
            })
            .collect();
        self.ports = union(
            self.ports
                .iter()
                .map(|r| (u32::from(r.start), u32::from(r.end))),
        )
        .into_iter()
        .map(|(start, end)| Range {
            start: start as u16,
            end: end as u16,
        })
        .collect();
        Ok(self)
    }

    pub fn allows(&self, ip: Ipv4Addr, port: u16) -> bool {
        self.ips.iter().any(|r| r.contains(ip)) && self.ports.iter().any(|r| r.contains(port))
    }

    pub fn conflicts(&self, other: &Self) -> bool {
        self.ips
            .iter()
            .any(|a| other.ips.iter().any(|b| a.intersects(b)))
            && self
                .ports
                .iter()
                .any(|a| other.ports.iter().any(|b| a.intersects(b)))
    }
}

const RESERVED: [Range<Ipv4Addr>; 5] = [
    Range {
        start: Ipv4Addr::new(0, 0, 0, 0),
        end: Ipv4Addr::new(0, 255, 255, 255),
    },
    Range {
        start: Ipv4Addr::new(127, 0, 0, 0),
        end: Ipv4Addr::new(127, 255, 255, 255),
    },
    Range {
        start: Ipv4Addr::new(169, 254, 0, 0),
        end: Ipv4Addr::new(169, 254, 255, 255),
    },
    VIRTUAL,
    Range {
        start: Ipv4Addr::new(224, 0, 0, 0),
        end: Ipv4Addr::BROADCAST,
    },
];

fn parse_range<T: FromStr + Copy>(s: &str, error: &'static str) -> Result<Range<T>, &'static str> {
    let parse = |s: &str| s.trim().parse::<T>().map_err(|_| error);
    let (start, end) = match s.split_once('-') {
        Some((a, b)) => (parse(a)?, parse(b)?),
        None => {
            let value = parse(s)?;
            (value, value)
        }
    };
    Ok(Range { start, end })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Cidr {
    pub ip: Ipv4Addr,
    pub prefix: u8,
}

impl Cidr {
    pub fn new(ip: Ipv4Addr, prefix: u8) -> Result<Self, &'static str> {
        if prefix > 32 {
            return Err("IPv4 前缀非法");
        }
        let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
        Ok(Self {
            ip: (u32::from(ip) & mask).into(),
            prefix,
        })
    }

    pub fn range(self) -> Range<Ipv4Addr> {
        Range {
            start: self.ip,
            end: (u32::from(self.ip) | u32::MAX.checked_shr(u32::from(self.prefix)).unwrap_or(0))
                .into(),
        }
    }

    pub fn cover((mut start, end): (u32, u32)) -> Vec<Self> {
        let mut out = Vec::new();
        while start <= end {
            let bits = start
                .trailing_zeros()
                .min((u64::from(end) - u64::from(start) + 1).ilog2());
            out.push(Self {
                ip: start.into(),
                prefix: 32 - bits as u8,
            });
            match u32::try_from(u64::from(start) + (1u64 << bits)) {
                Ok(next) => start = next,
                Err(_) => break,
            }
        }
        out
    }
}

impl FromStr for Cidr {
    type Err = &'static str;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (ip, prefix) = s.split_once('/').ok_or("需要 IPv4 CIDR")?;
        Self::new(
            ip.parse().map_err(|_| "IPv4 地址格式错误")?,
            prefix.parse().map_err(|_| "IPv4 前缀非法")?,
        )
    }
}

impl Display for Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.ip, self.prefix)
    }
}

impl TryFrom<String> for Cidr {
    type Error = &'static str;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}
impl From<Cidr> for String {
    fn from(cidr: Cidr) -> Self {
        cidr.to_string()
    }
}

pub type Interval = (u32, u32);

pub fn union(ranges: impl IntoIterator<Item = Interval>) -> Vec<Interval> {
    let mut sorted: Vec<_> = ranges.into_iter().collect();
    sorted.sort_unstable();
    let mut out: Vec<Interval> = Vec::new();
    for (a, b) in sorted {
        if let Some(last) = out.last_mut().filter(|r| a <= r.1.saturating_add(1)) {
            last.1 = last.1.max(b);
        } else {
            out.push((a, b));
        }
    }
    out
}

pub fn count(ranges: &[Interval]) -> u64 {
    ranges
        .iter()
        .map(|&(a, b)| u64::from(b) - u64::from(a) + 1)
        .sum()
}

pub fn difference(ranges: &[Interval], exclude: &[Interval]) -> Vec<Interval> {
    let mut out = Vec::new();
    let mut index = 0;
    for &(a, b) in ranges {
        let mut start = u64::from(a);
        while let Some(&(low, high)) = exclude.get(index) {
            if low > b {
                break;
            }
            if u64::from(low) > start {
                out.push((start as u32, low - 1));
            }
            start = start.max(u64::from(high) + 1);
            if high >= b {
                break;
            }
            index += 1;
        }
        if start <= u64::from(b) {
            out.push((start as u32, b));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_match_and_conflict() {
        let rules = Rules::parse("10.2.1.4-10.2.1.8,10.3.0.0/30", "80,440-443").unwrap();
        assert!(rules.allows("10.2.1.8".parse().unwrap(), 443));
        assert!(!rules.allows("10.2.1.8".parse().unwrap(), 444));
        assert!(rules.conflicts(&Rules::parse("10.3.0.3", "443").unwrap()));
        assert!(!rules.conflicts(&Rules::parse("10.3.0.3", "444").unwrap()));
        assert!(!rules.conflicts(&Rules::parse("10.3.0.4", "443").unwrap()));
        assert!(Rules::parse("10.0.0.0/8", "80").is_err());
        assert!(Rules::parse("198.18.2.1", "443").is_err());
        assert!(Rules::parse("1.1.1.1", "0,80").is_err());
    }

    #[test]
    fn intervals_cover_and_subtract() {
        assert_eq!(
            difference(&[(2, 10), (15, 20)], &[(0, 2), (6, 6), (9, 18)]),
            vec![(3, 5), (7, 8), (19, 20)]
        );
        assert_eq!(
            difference(&[(u32::MAX, u32::MAX)], &[(0, u32::MAX)]),
            vec![]
        );
        let cover = Cidr::cover((3, 11));
        assert_eq!(
            cover.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["0.0.0.3/32", "0.0.0.4/30", "0.0.0.8/30"]
        );
    }
}
