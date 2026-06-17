use rusqlite::Connection;

use crate::github::GithubClient;
use crate::github::rest::Conditional;
use crate::store;

/// Apply a conditional-GET result: on `Modified` replace the rows and persist
/// the new etag; on `NotModified` do nothing.
///
/// The etag read and the HTTP fetch are performed by the caller so that the
/// borrow of the etag string does not need to cross an await point inside a
/// closure (which the borrow checker rejects in the `FnOnce` + `async`
/// pattern).
fn apply_conditional<T>(
    conn: &Connection,
    resource: &str,
    result: Conditional<Vec<T>>,
    store_rows: fn(&Connection, &[T]) -> rusqlite::Result<()>,
) -> anyhow::Result<()> {
    match result {
        Conditional::NotModified => {}
        Conditional::Modified { data, etag, .. } => {
            store_rows(conn, &data)?;
            if let Some(tag) = etag {
                store::taxonomy::set_etag(conn, resource, &tag)?;
            }
        }
    }
    Ok(())
}

/// Sync labels: conditional GET; on 200 replace rows + store new etag; on 304
/// no-op.
///
/// # Errors
///
/// Returns an error if the database or HTTP request fails.
pub async fn sync_labels(
    client: &GithubClient,
    conn: &Connection,
    owner: &str,
    repo: &str,
) -> anyhow::Result<()> {
    let etag = store::taxonomy::get_etag(conn, "labels")?;
    let result = client.labels(owner, repo, etag.as_deref()).await?;
    apply_conditional(conn, "labels", result, store::taxonomy::replace_labels)
}

/// Sync milestones: conditional GET; on 200 replace rows + store new etag; on
/// 304 no-op.
///
/// # Errors
///
/// Returns an error if the database or HTTP request fails.
pub async fn sync_milestones(
    client: &GithubClient,
    conn: &Connection,
    owner: &str,
    repo: &str,
) -> anyhow::Result<()> {
    let etag = store::taxonomy::get_etag(conn, "milestones")?;
    let result = client.milestones(owner, repo, etag.as_deref()).await?;
    apply_conditional(
        conn,
        "milestones",
        result,
        store::taxonomy::replace_milestones,
    )
}

#[cfg(test)]
mod tests {
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
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
    async fn labels_sync_persists_and_caches_etag() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/labels"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("etag", "W/\"v1\"")
                    .set_body_string(
                        r#"[{"node_id":"L1","name":"bug","color":"f00","description":null}]"#,
                    ),
            )
            .mount(&server)
            .await;
        let client = client_for(&server);
        let conn = store::open_in_memory().unwrap();
        sync_labels(&client, &conn, "o", "r").await.unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM labels", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            store::taxonomy::get_etag(&conn, "labels")
                .unwrap()
                .as_deref(),
            Some("W/\"v1\"")
        );
    }

    #[tokio::test]
    async fn milestones_sync_persists() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/milestones"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    r#"[{"node_id":"M1","number":1,"title":"v1","state":"open","description":null,"due_on":null}]"#,
                ),
            )
            .mount(&server)
            .await;
        let client = client_for(&server);
        let conn = store::open_in_memory().unwrap();
        sync_milestones(&client, &conn, "o", "r").await.unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM milestones", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
