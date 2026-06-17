use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryType {
    IssuesPage,
    CommentsPage,
    TimelinePage,
    PrsPage,
}

impl QueryType {
    /// Conservative per-type ceiling used as the first-call estimate and as a
    /// floor on the running estimate so a cheap early page can't lull the
    /// budget check.
    #[must_use]
    pub fn ceiling(self) -> u64 {
        match self {
            QueryType::IssuesPage => 30,
            QueryType::PrsPage => 40,
            QueryType::CommentsPage | QueryType::TimelinePage => 10,
        }
    }
}

#[must_use]
pub struct CostEstimator {
    ewma: HashMap<QueryType, f64>,
    alpha: f64,
    ceiling_override: Option<u64>,
}

impl CostEstimator {
    pub fn new(ceiling_override: Option<u64>) -> Self {
        Self {
            ewma: HashMap::new(),
            alpha: 0.3,
            ceiling_override,
        }
    }

    fn floor_for(&self, q: QueryType) -> u64 {
        self.ceiling_override.unwrap_or_else(|| q.ceiling())
    }

    /// Estimated cost of the next call of this type: max(observed EWMA,
    /// configured floor).
    ///
    /// # Cast rationale
    /// `ewma` values are small non-negative API point counts; `ceil()` output
    /// fits in u64.
    #[must_use]
    pub fn estimate(&self, q: QueryType) -> u64 {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let observed = self.ewma.get(&q).copied().unwrap_or(0.0).ceil() as u64;
        observed.max(self.floor_for(q))
    }

    /// Update the running estimate from an observed used-delta.
    ///
    /// # Cast rationale
    /// `used_delta` is a small non-negative API point count; cast to f64 is
    /// lossless for realistic values (well below 2^53).
    pub fn observe(&mut self, q: QueryType, used_delta: u64) {
        #[expect(clippy::cast_precision_loss)]
        let used_delta = used_delta as f64;
        let prev = self.ewma.get(&q).copied().unwrap_or(used_delta);
        let next = self.alpha.mul_add(used_delta, (1.0 - self.alpha) * prev);
        self.ewma.insert(q, next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_call_uses_ceiling() {
        let est = CostEstimator::new(None);
        assert_eq!(est.estimate(QueryType::IssuesPage), 30);
    }

    #[test]
    fn estimate_never_below_ceiling_even_after_cheap_pages() {
        let mut est = CostEstimator::new(None);
        est.observe(QueryType::IssuesPage, 1);
        assert_eq!(est.estimate(QueryType::IssuesPage), 30);
    }

    #[test]
    fn estimate_rises_with_expensive_observations() {
        let mut est = CostEstimator::new(None);
        for _ in 0..10 {
            est.observe(QueryType::IssuesPage, 100);
        }
        assert!(est.estimate(QueryType::IssuesPage) > 30);
    }

    #[test]
    fn ceiling_override_applies() {
        let est = CostEstimator::new(Some(200));
        assert_eq!(est.estimate(QueryType::CommentsPage), 200);
    }

    #[test]
    fn prs_page_ceiling_is_conservative() {
        let est = CostEstimator::new(None);
        assert_eq!(est.estimate(QueryType::PrsPage), 40);
    }
}
