//! A session document for a wiremock server, and the client for it.

use mailrs_jmap::{Auth, JmapClient};
use serde_json::{Value, json};
use wiremock::MockServer;

/// A session whose every URL is on `base`, offering mail and submission,
/// with `limits` merged over small core limits.
pub fn session_on(base: &str, limits: Value) -> Value {
    let mut core = json!({
        "maxSizeUpload": 1000000, "maxConcurrentUpload": 2, "maxSizeRequest": 100000,
        "maxConcurrentRequests": 2, "maxCallsInRequest": 4, "maxObjectsInGet": 50, "maxObjectsInSet": 50
    });
    if let (Some(core), Some(extra)) = (core.as_object_mut(), limits.as_object()) {
        core.extend(extra.clone());
    }
    json!({
        "capabilities": {
            "urn:ietf:params:jmap:core": core,
            "urn:ietf:params:jmap:mail": {},
            "urn:ietf:params:jmap:submission": {}
        },
        "accounts": { "A1": { "name": "me@example.org", "isPersonal": true, "isReadOnly": false,
            "accountCapabilities": { "urn:ietf:params:jmap:mail": {}, "urn:ietf:params:jmap:submission": {} } } },
        "primaryAccounts": { "urn:ietf:params:jmap:mail": "A1", "urn:ietf:params:jmap:submission": "A1" },
        "username": "me@example.org",
        "apiUrl": format!("{base}/api/"),
        "downloadUrl": format!("{base}/download/{{accountId}}/{{blobId}}/{{name}}?type={{type}}"),
        "uploadUrl": format!("{base}/upload/{{accountId}}/"),
        "eventSourceUrl": format!("{base}/events/"),
        "state": "s1"
    })
}

#[allow(
    dead_code,
    reason = "push.rs builds its clients with short push limits instead"
)]
pub fn client(server: &MockServer) -> JmapClient {
    JmapClient::new(
        &format!("{}/jmap/session", server.uri()),
        Auth::Bearer("token-1".into()),
    )
    .unwrap()
}
