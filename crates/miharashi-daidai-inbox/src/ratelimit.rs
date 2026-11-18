//! 送信元 IP ごとのトークンバケット。時計(`Instant`)は呼び出し側が渡すので、試験で進められる。

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// 既定: 1 分あたり 10 件、バースト 5。
pub const DEFAULT_BURST: u32 = 5;
pub const DEFAULT_PER_MINUTE: u32 = 10;

#[derive(Debug)]
pub struct RateLimiter {
    burst: f64,
    per_sec: f64,
    buckets: HashMap<IpAddr, (f64, Instant)>,
}

impl RateLimiter {
    pub fn new(burst: u32, per_minute: u32) -> Self {
        Self {
            burst: f64::from(burst.max(1)),
            per_sec: f64::from(per_minute.max(1)) / 60.0,
            buckets: HashMap::new(),
        }
    }

    /// 1 件ぶんを使う。足りないときは、次の 1 件が使えるまでの待ち時間を返す。
    pub fn acquire(&mut self, ip: IpAddr, now: Instant) -> Result<(), Duration> {
        let burst = self.burst;
        let per_sec = self.per_sec;
        let (tokens, last) = self.buckets.entry(ip).or_insert((burst, now));
        let elapsed = now.saturating_duration_since(*last).as_secs_f64();
        *tokens = (*tokens + elapsed * per_sec).min(burst);
        if now > *last {
            *last = now;
        }
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - *tokens) / per_sec))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn allows_the_burst_then_refuses_and_recovers_with_the_clock() {
        let mut l = RateLimiter::new(5, 10);
        let t0 = Instant::now();
        let a = ip("100.64.0.2");
        for _ in 0..5 {
            assert!(l.acquire(a, t0).is_ok());
        }
        let wait = l.acquire(a, t0).unwrap_err();
        // 10 件/分 = 6 秒に 1 件
        assert!(wait > Duration::from_secs(5) && wait <= Duration::from_secs(6));
        assert!(l.acquire(a, t0 + Duration::from_secs(3)).is_err());
        assert!(l.acquire(a, t0 + Duration::from_secs(7)).is_ok());
        assert!(l.acquire(a, t0 + Duration::from_secs(7)).is_err());
        // 十分待てばバーストまで戻る(それ以上は貯まらない)
        let later = t0 + Duration::from_secs(3600);
        for _ in 0..5 {
            assert!(l.acquire(a, later).is_ok());
        }
        assert!(l.acquire(a, later).is_err());
    }

    #[test]
    fn counts_each_address_separately() {
        let mut l = RateLimiter::new(1, 10);
        let t0 = Instant::now();
        assert!(l.acquire(ip("100.64.0.2"), t0).is_ok());
        assert!(l.acquire(ip("100.64.0.2"), t0).is_err());
        assert!(l.acquire(ip("100.64.0.3"), t0).is_ok());
    }

    #[test]
    fn a_clock_that_goes_backwards_does_not_refill() {
        let mut l = RateLimiter::new(1, 10);
        let t0 = Instant::now() + Duration::from_secs(100);
        assert!(l.acquire(ip("127.0.0.1"), t0).is_ok());
        assert!(
            l.acquire(ip("127.0.0.1"), t0 - Duration::from_secs(50))
                .is_err()
        );
    }
}
