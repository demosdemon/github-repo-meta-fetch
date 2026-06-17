use chrono::DateTime;
use chrono::TimeZone;
use chrono::Utc;
use http::HeaderMap;

/// A GitHub API rate-limit resource category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resource {
    Core,
    GraphQL,
    Search,
}

impl Resource {
    /// Returns the lowercase string identifier for this resource as used in
    /// `X-RateLimit-Resource` headers.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Resource::Core => "core",
            Resource::GraphQL => "graphql",
            Resource::Search => "search",
        }
    }

    /// Parse a `Resource` from the string value of the `X-RateLimit-Resource`
    /// header. Returns `None` for unrecognized values.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "core" => Some(Resource::Core),
            "graphql" => Some(Resource::GraphQL),
            "search" => Some(Resource::Search),
            _ => None,
        }
    }
}

/// A snapshot of a GitHub rate-limit bucket for a single resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bucket {
    pub limit: u64,
    pub remaining: u64,
    pub used: u64,
    pub reset: DateTime<Utc>,
}

/// Parse the `X-RateLimit-*` family from a response's headers.
///
/// Returns `None` if the resource header is absent or unrecognized, or if any
/// of the required numeric/timestamp headers are missing or malformed.
#[must_use]
pub fn parse_rate_headers(headers: &HeaderMap) -> Option<(Resource, Bucket)> {
    fn h_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
        headers.get(name)?.to_str().ok()?.parse().ok()
    }

    let resource = Resource::parse(headers.get("x-ratelimit-resource")?.to_str().ok()?)?;
    let reset_secs = i64::try_from(h_u64(headers, "x-ratelimit-reset")?).ok()?;
    let bucket = Bucket {
        limit: h_u64(headers, "x-ratelimit-limit")?,
        remaining: h_u64(headers, "x-ratelimit-remaining")?,
        used: h_u64(headers, "x-ratelimit-used")?,
        reset: Utc.timestamp_opt(reset_secs, 0).single()?,
    };
    Some((resource, bucket))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                http::HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn parses_graphql_bucket() {
        let h = headers(&[
            ("x-ratelimit-resource", "graphql"),
            ("x-ratelimit-limit", "5000"),
            ("x-ratelimit-remaining", "4863"),
            ("x-ratelimit-used", "137"),
            ("x-ratelimit-reset", "1781564821"),
        ]);
        let (res, b) = parse_rate_headers(&h).unwrap();
        assert_eq!(res, Resource::GraphQL);
        assert_eq!(b.limit, 5000);
        assert_eq!(b.remaining, 4863);
        assert_eq!(b.used, 137);
    }

    #[test]
    fn returns_none_when_resource_missing() {
        let h = headers(&[("x-ratelimit-limit", "5000")]);
        assert!(parse_rate_headers(&h).is_none());
    }
}
