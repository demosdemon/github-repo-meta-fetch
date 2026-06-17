use bytes::Bytes;
use http::HeaderMap;
use http_body_util::BodyExt as _;

use crate::github::GithubClient;
use crate::model::Label;
use crate::model::Milestone;

/// Result of a conditional GET: `NotModified`, or fresh data + new etag.
pub enum Conditional<T> {
    /// The server returned 304 Not Modified; cached data is still valid.
    NotModified,
    /// The server returned fresh data with an optional `ETag` and response
    /// headers.
    Modified {
        /// Decoded response body.
        data: T,
        /// `ETag` value from the response, if present.
        etag: Option<String>,
        /// Full response headers.
        headers: HeaderMap,
    },
}

impl GithubClient {
    async fn conditional_get_json(
        &self,
        route: &str,
        etag: Option<&str>,
    ) -> anyhow::Result<Conditional<serde_json::Value>> {
        let uri: http::Uri = route
            .parse()
            .map_err(|e| anyhow::anyhow!("uri parse failed: {e}"))?;
        let mut headers = HeaderMap::new();
        if let Some(tag) = etag {
            headers.insert(
                http::header::IF_NONE_MATCH,
                http::HeaderValue::from_str(tag)?,
            );
        }
        // `_get_with_headers` is octocrab's semi-internal request API; it keeps the
        // raw response headers (`ETag`, `X-RateLimit-*`) needed below.
        let resp = self.octo._get_with_headers(uri, Some(headers)).await?;
        let (parts, body) = resp.into_parts();
        if parts.status == http::StatusCode::NOT_MODIFIED {
            return Ok(Conditional::NotModified);
        }
        let new_etag = parts
            .headers
            .get(http::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let bytes: Bytes = body
            .collect()
            .await
            .map_err(|e| anyhow::anyhow!("body read failed: {e}"))?
            .to_bytes();
        let data: serde_json::Value = serde_json::from_slice(&bytes)?;
        Ok(Conditional::Modified {
            data,
            etag: new_etag,
            headers: parts.headers,
        })
    }

    /// GET all labels (single page, up to 100). Conditional on `etag`.
    pub async fn labels(
        &self,
        owner: &str,
        repo: &str,
        etag: Option<&str>,
    ) -> anyhow::Result<Conditional<Vec<Label>>> {
        let route = format!("/repos/{owner}/{repo}/labels?per_page=100");
        match self.conditional_get_json(&route, etag).await? {
            Conditional::NotModified => Ok(Conditional::NotModified),
            Conditional::Modified {
                data,
                etag,
                headers,
            } => {
                let labels = data
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|v| Label {
                        node_id: v["node_id"].as_str().unwrap_or_default().to_string(),
                        name: v["name"].as_str().unwrap_or_default().to_string(),
                        color: v["color"].as_str().unwrap_or_default().to_string(),
                        description: v["description"].as_str().map(String::from),
                    })
                    .collect();
                Ok(Conditional::Modified {
                    data: labels,
                    etag,
                    headers,
                })
            }
        }
    }

    /// GET all milestones (state=all, up to 100). Conditional on `etag`.
    pub async fn milestones(
        &self,
        owner: &str,
        repo: &str,
        etag: Option<&str>,
    ) -> anyhow::Result<Conditional<Vec<Milestone>>> {
        let route = format!("/repos/{owner}/{repo}/milestones?per_page=100&state=all");
        match self.conditional_get_json(&route, etag).await? {
            Conditional::NotModified => Ok(Conditional::NotModified),
            Conditional::Modified {
                data,
                etag,
                headers,
            } => {
                let milestones = data
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|v| Milestone {
                        node_id: v["node_id"].as_str().unwrap_or_default().to_string(),
                        number: v["number"].as_i64().unwrap_or_default(),
                        title: v["title"].as_str().unwrap_or_default().to_string(),
                        state: v["state"].as_str().unwrap_or_default().to_string(),
                        description: v["description"].as_str().map(String::from),
                        due_on: v["due_on"]
                            .as_str()
                            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                            .map(|d| d.with_timezone(&chrono::Utc)),
                    })
                    .collect();
                Ok(Conditional::Modified {
                    data: milestones,
                    etag,
                    headers,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header_exists;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;

    fn client_for(server: &MockServer) -> GithubClient {
        let octo = octocrab::Octocrab::builder()
            .base_uri(server.uri())
            .unwrap()
            .personal_token("t".to_string())
            .build()
            .unwrap();
        GithubClient::new(octo)
    }

    #[tokio::test]
    async fn labels_fetch_then_304() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/labels"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("etag", "W/\"v1\"")
                    .set_body_string(
                        r#"[{"node_id":"L1","name":"bug","color":"f00","description":"d"}]"#,
                    ),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        let client = client_for(&server);
        let first = client.labels("o", "r", None).await.unwrap();
        let etag = match first {
            Conditional::Modified { data, etag, .. } => {
                assert_eq!(data[0].name, "bug");
                etag.unwrap()
            }
            Conditional::NotModified => panic!("expected modified"),
        };

        Mock::given(method("GET"))
            .and(path("/repos/o/r/labels"))
            .and(header_exists("if-none-match"))
            .respond_with(ResponseTemplate::new(304))
            .mount(&server)
            .await;
        let second = client.labels("o", "r", Some(&etag)).await.unwrap();
        assert!(matches!(second, Conditional::NotModified));
    }

    #[tokio::test]
    async fn milestones_parse() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/milestones"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    r#"[{"node_id":"M1","number":1,"title":"v1","state":"open","description":null,"due_on":"2026-12-31T00:00:00Z"}]"#,
                ),
            )
            .mount(&server)
            .await;
        let client = client_for(&server);
        match client.milestones("o", "r", None).await.unwrap() {
            Conditional::Modified { data, .. } => {
                assert_eq!(data[0].title, "v1");
                assert!(data[0].due_on.is_some());
            }
            Conditional::NotModified => panic!("expected modified"),
        }
    }
}
