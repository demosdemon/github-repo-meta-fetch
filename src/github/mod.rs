pub mod auth;
pub mod gql;
pub mod rest;

use bytes::Bytes;
use http::HeaderMap;
use http_body_util::BodyExt as _;

/// A thin wrapper around [`octocrab::Octocrab`] that exposes a GraphQL method
/// preserving the raw HTTP response headers (e.g. `X-RateLimit-*`), which the
/// built-in `Octocrab::graphql` discards.
pub struct GithubClient {
    pub octo: octocrab::Octocrab,
}

/// A decoded GraphQL response plus the rate-limit headers from the HTTP
/// response.
pub struct GqlResult<T> {
    pub data: T,
    pub headers: HeaderMap,
}

impl GithubClient {
    /// Wrap an existing [`octocrab::Octocrab`] instance.
    #[must_use]
    pub fn new(octo: octocrab::Octocrab) -> Self {
        Self { octo }
    }

    /// POST a GraphQL body (any `Serialize`, e.g. `graphql_client`'s
    /// `QueryBody`) and decode `graphql_client::Response<T>`, returning
    /// data + response headers.
    ///
    /// Returns `Err` if the GraphQL `errors` array is present/non-empty or if
    /// `data` is missing.
    pub async fn graphql<B, T>(&self, body: &B) -> anyhow::Result<GqlResult<T>>
    where
        B: serde::Serialize + ?Sized,
        T: serde::de::DeserializeOwned,
    {
        let uri: http::Uri = "/graphql"
            .parse()
            .map_err(|e| anyhow::anyhow!("uri parse: {e}"))?;
        // `_post` is octocrab's semi-internal request API; it preserves the raw
        // response so we keep the `X-RateLimit-*` headers that `graphql()` drops.
        let resp = self.octo._post(uri, Some(body)).await?;
        let (parts, body) = resp.into_parts();
        let bytes: Bytes = body
            .collect()
            .await
            .map_err(|e| anyhow::anyhow!("body read failed: {e}"))?
            .to_bytes();
        let decoded: graphql_client::Response<T> = serde_json::from_slice(&bytes)?;
        if let Some(errors) = decoded.errors.filter(|e| !e.is_empty()) {
            anyhow::bail!("graphql errors: {errors:?}");
        }
        let data = decoded
            .data
            .ok_or_else(|| anyhow::anyhow!("graphql response had no data"))?;
        Ok(GqlResult {
            data,
            headers: parts.headers,
        })
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::GithubClient;
    use super::GqlResult;

    #[derive(Deserialize)]
    struct Viewer {
        viewer: ViewerInner,
    }

    #[derive(Deserialize)]
    struct ViewerInner {
        login: String,
    }

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
    async fn graphql_decodes_data_and_captures_headers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-ratelimit-resource", "graphql")
                    .insert_header("x-ratelimit-limit", "5000")
                    .insert_header("x-ratelimit-remaining", "4999")
                    .insert_header("x-ratelimit-used", "1")
                    .insert_header("x-ratelimit-reset", "1781564821")
                    .set_body_string(r#"{"data":{"viewer":{"login":"octocat"}}}"#),
            )
            .mount(&server)
            .await;
        let client = client_for(&server);
        let body = serde_json::json!({"query":"{viewer{login}}"});
        let res: GqlResult<Viewer> = client.graphql(&body).await.unwrap();
        assert_eq!(res.data.viewer.login, "octocat");
        let (r, b) = crate::ratelimit::budget::parse_rate_headers(&res.headers).unwrap();
        assert_eq!(r, crate::ratelimit::budget::Resource::GraphQL);
        assert_eq!(b.remaining, 4999);
    }

    #[tokio::test]
    async fn graphql_surfaces_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"errors":[{"message":"boom"}]}"#),
            )
            .mount(&server)
            .await;
        let client = client_for(&server);
        let body = serde_json::json!({"query":"{x}"});
        let res: anyhow::Result<GqlResult<Viewer>> = client.graphql(&body).await;
        assert!(res.is_err());
    }
}
