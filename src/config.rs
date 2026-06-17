use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reserve {
    Absolute(u64),
    Percent(f64), // 0.0..=1.0
}

impl Reserve {
    /// The absolute floor for a bucket whose limit is `limit`.
    #[must_use]
    pub fn floor_for(&self, limit: u64) -> u64 {
        match self {
            Reserve::Absolute(n) => *n,
            #[expect(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            Reserve::Percent(p) => (limit as f64 * p).ceil() as u64,
        }
    }
}

impl FromStr for Reserve {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if let Some(pct) = s.strip_suffix('%') {
            let v: f64 = pct
                .trim()
                .parse()
                .map_err(|_| format!("bad percent: {s:?}"))?;
            if !(0.0..=100.0).contains(&v) {
                return Err(format!("percent out of range: {s:?}"));
            }
            Ok(Reserve::Percent(v / 100.0))
        } else {
            let v: u64 = s.parse().map_err(|_| format!("bad reserve: {s:?}"))?;
            Ok(Reserve::Absolute(v))
        }
    }
}

impl Default for Reserve {
    fn default() -> Self {
        Reserve::Percent(0.10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_percent_and_absolute() {
        assert_eq!(Reserve::from_str("10%").unwrap(), Reserve::Percent(0.10));
        assert_eq!(Reserve::from_str("500").unwrap(), Reserve::Absolute(500));
    }

    #[test]
    fn percent_floor_rounds_up() {
        assert_eq!(Reserve::Percent(0.10).floor_for(5000), 500);
        assert_eq!(Reserve::Percent(0.10).floor_for(30), 3);
    }

    #[test]
    fn absolute_floor_ignores_limit() {
        assert_eq!(Reserve::Absolute(500).floor_for(30), 500);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Reserve::from_str("abc").is_err());
        assert!(Reserve::from_str("200%").is_err());
    }

    #[test]
    fn default_is_ten_percent() {
        assert_eq!(Reserve::default(), Reserve::Percent(0.10));
    }
}
