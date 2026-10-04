#![allow(dead_code)]

use std::sync::Arc;

use mailrs_graph::{Graph, MicrosoftClient, Session};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub const CLIENT: &str = "00000000-0000-0000-0000-000000000000";

pub async fn token_endpoint(server: &MockServer, refresh: &str) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access-1",
            "expires_in": 3600,
            "refresh_token": refresh,
            "scope": "https://graph.microsoft.com/Mail.ReadWrite offline_access openid",
        })))
        .mount(server)
        .await;
}

pub fn graph(server: &MockServer) -> Graph {
    let client = MicrosoftClient::new(CLIENT).with_endpoints(
        format!("{}/authorize", server.uri()),
        format!("{}/token", server.uri()),
    );
    let session = Arc::new(Session::new(client, "refresh-0"));
    Graph::with_base(session, &format!("{}/v1.0/", server.uri())).unwrap()
}
