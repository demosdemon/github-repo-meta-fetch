use graphql_client::GraphQLQuery;

// GitHub custom scalars used by our queries. Only DateTime is actually
// referenced; add more `type X = String;` aliases ONLY if codegen errors
// demand them.
type DateTime = chrono::DateTime<chrono::Utc>;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "github-schema.json",
    query_path = "src/github/queries/issues.graphql",
    response_derives = "Debug,Clone"
)]
pub struct IssuesPage;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "github-schema.json",
    query_path = "src/github/queries/comments.graphql",
    response_derives = "Debug,Clone"
)]
pub struct CommentsPage;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "github-schema.json",
    query_path = "src/github/queries/timeline.graphql",
    response_derives = "Debug,Clone"
)]
pub struct TimelinePage;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "github-schema.json",
    query_path = "src/github/queries/prs.graphql",
    response_derives = "Debug,Clone"
)]
pub struct PrsPage;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "github-schema.json",
    query_path = "src/github/queries/reviews.graphql",
    response_derives = "Debug,Clone"
)]
pub struct ReviewsPage;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "github-schema.json",
    query_path = "src/github/queries/review_threads.graphql",
    response_derives = "Debug,Clone"
)]
pub struct ReviewThreadsPage;

#[cfg(test)]
mod tests {
    use graphql_client::GraphQLQuery as _;

    use super::IssuesPage;
    use super::PrsPage;
    use super::issues_page;
    use super::prs_page;

    #[test]
    fn prs_query_builds_body() {
        let body = PrsPage::build_query(prs_page::Variables {
            owner: "o".into(),
            repo: "r".into(),
            cursor: None,
        });
        let json = serde_json::to_value(&body).unwrap();
        assert!(json["query"].as_str().unwrap().contains("pullRequests("));
        assert_eq!(json["variables"]["owner"], "o");
    }

    #[test]
    fn issues_query_builds_body() {
        let body = IssuesPage::build_query(issues_page::Variables {
            owner: "o".into(),
            repo: "r".into(),
            cursor: None,
        });
        let json = serde_json::to_value(&body).unwrap();
        assert!(json["query"].as_str().unwrap().contains("issues("));
        assert_eq!(json["variables"]["owner"], "o");
    }

    #[test]
    fn issues_response_decodes_from_fixture() {
        let fixture = serde_json::json!({
          "data": {"repository": {"issues": {
            "pageInfo": {"hasNextPage": false, "endCursor": null},
            "nodes": [{
              "id": "I_1", "number": 42, "title": "Bug", "body": "x",
              "state": "OPEN", "stateReason": null,
              "createdAt": "2026-01-05T00:00:00Z", "updatedAt": "2026-06-10T00:00:00Z",
              "closedAt": null, "author": {"__typename": "User", "login": "octocat"}, "milestone": null,
              "labels": {"nodes": [{"name": "bug"}]},
              "assignees": {"nodes": [{"login": "octocat"}]},
              "comments": {"totalCount": 0, "pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": []},
              "timelineItems": {"pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": []}
            }]
          }}}
        });
        let resp: graphql_client::Response<issues_page::ResponseData> =
            serde_json::from_value(fixture).unwrap();
        let data = resp.data.unwrap();
        let issues = data.repository.unwrap().issues;
        assert_eq!(issues.nodes.unwrap()[0].as_ref().unwrap().number, 42);
    }
}
