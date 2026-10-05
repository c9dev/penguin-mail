//! DavClient against wiremock serving the shapes iCloud, Fastmail,
//! Radicale and Nextcloud answer with.

use mailrs_dav::{DavApi, DavClient, DavError, Kind, Login, Precondition};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn multistatus(body: String) -> ResponseTemplate {
    ResponseTemplate::new(207).insert_header("Content-Type", "application/xml; charset=utf-8").set_body_string(body)
}

fn client(server: &MockServer) -> DavClient {
    DavClient::over(url::Url::parse(&server.uri()).unwrap(), Login::new("me", "pw")).unwrap()
}

#[tokio::test]
async fn a_principal_on_one_host_finds_its_home_on_another_of_the_same_domain() {
    let server = MockServer::start().await;
    Mock::given(method("PROPFIND")).and(path("/"))
        .respond_with(multistatus(fixture("icloud-principal.xml"))).mount(&server).await;
    Mock::given(method("PROPFIND")).and(path("/1234567890/principal/"))
        .respond_with(multistatus(fixture("icloud-homes.xml"))).mount(&server).await;
    let homes = client(&server).homes().await.unwrap();
    assert_eq!(homes.principal, "/1234567890/principal/");
    assert_eq!(homes.calendar.as_deref(), Some("https://p12-caldav.icloud.com:443/1234567890/calendars/"));
}

#[tokio::test]
async fn collections_leave_out_the_home_and_lists_without_events() {
    let server = MockServer::start().await;
    Mock::given(method("PROPFIND")).and(path("/dav/calendars/user/me@fastmail.com/")).and(header("Depth", "1"))
        .respond_with(multistatus(fixture("fastmail-collections.xml"))).mount(&server).await;
    let found = client(&server).collections("/dav/calendars/user/me@fastmail.com/", Kind::Calendar).await.unwrap();
    let names: Vec<&str> = found.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Personal", "Shared with me"]);
    assert_eq!(found[0].color.as_deref(), Some("#3a87ad"));
    assert!(found[0].can_write && !found[1].can_write);
    assert!(found[0].sync);
}

#[tokio::test]
async fn a_sync_report_splits_changes_removals_and_a_truncation() {
    let server = MockServer::start().await;
    Mock::given(method("REPORT")).and(path("/me/work/"))
        .respond_with(multistatus(fixture("radicale-sync.xml"))).mount(&server).await;
    let synced = client(&server).sync("/me/work/", "").await.unwrap();
    assert_eq!(synced.changed.len(), 1);
    assert_eq!(synced.removed, [format!("{}/me/work/gone.ics", server.uri())]);
    assert!(synced.more);
    assert_eq!(synced.token, "http://radicale.org/ns/sync/abc");
}

#[tokio::test]
async fn a_refused_token_is_invalid_sync_token() {
    let server = MockServer::start().await;
    Mock::given(method("REPORT")).respond_with(
        ResponseTemplate::new(403).set_body_string(r#"<D:error xmlns:D="DAV:"><D:valid-sync-token/></D:error>"#),
    ).mount(&server).await;
    let refused = client(&server).sync("/me/work/", "old").await.unwrap_err();
    assert!(matches!(refused, DavError::InvalidSyncToken), "{refused:?}");
}

#[tokio::test]
async fn a_put_sends_if_match_and_answers_the_new_etag() {
    let server = MockServer::start().await;
    Mock::given(method("PUT")).and(path("/me/work/a.ics")).and(header("If-Match", "\"1\""))
        .respond_with(ResponseTemplate::new(204).insert_header("ETag", "\"2\"")).mount(&server).await;
    let etag = client(&server).put("/me/work/a.ics", "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n", Kind::Calendar, Precondition::Match("\"1\"".into())).await.unwrap();
    assert_eq!(etag.as_deref(), Some("\"2\""));
}

#[tokio::test]
async fn a_stale_etag_is_changed_and_a_bad_password_unauthorized() {
    let server = MockServer::start().await;
    Mock::given(method("PUT")).respond_with(ResponseTemplate::new(412)).mount(&server).await;
    Mock::given(method("DELETE")).respond_with(ResponseTemplate::new(401)).mount(&server).await;
    let c = client(&server);
    assert!(matches!(c.put("/a.ics", "x", Kind::Calendar, Precondition::NoneMatch).await, Err(DavError::Changed)));
    assert!(matches!(c.delete("/a.ics", None).await, Err(DavError::Unauthorized)));
}

#[tokio::test]
async fn a_multiget_skips_what_went() {
    let server = MockServer::start().await;
    Mock::given(method("REPORT")).and(path("/cal/work/"))
        .respond_with(multistatus(fixture("nextcloud-multiget.xml"))).mount(&server).await;
    let hrefs = vec!["/cal/work/a.ics".to_string(), "/cal/work/b.ics".into(), "/cal/work/gone.ics".into()];
    let fetched = client(&server).fetch("/cal/work/", Kind::Calendar, &hrefs).await.unwrap();
    assert_eq!(fetched.len(), 2);
    assert!(fetched[0].body.contains("BEGIN:VEVENT"));
}

#[tokio::test]
async fn an_answer_past_the_limit_is_refused_without_reading_it_whole() {
    let server = MockServer::start().await;
    let huge = "x".repeat(mailrs_dav::MOST_BYTES + 1);
    Mock::given(method("PROPFIND")).respond_with(ResponseTemplate::new(207).set_body_string(huge)).mount(&server).await;
    assert!(matches!(client(&server).state("/cal/").await, Err(DavError::TooLarge(_))));
}

#[test]
fn a_plain_http_context_is_refused() {
    assert!(DavClient::new("http://dav.example.org/", Login::new("me", "pw")).is_err());
}

#[test]
fn the_password_follows_a_redirect_only_within_the_domain() {
    use mailrs_dav::client::same_site;
    assert!(same_site("caldav.icloud.com", "p12-caldav.icloud.com"));
    assert!(!same_site("dav.example.org", "dav.example.net"));
    assert!(!same_site("example.co.uk", "other.co.uk"));
}
#[tokio::test]
async fn a_range_query_names_both_ends() {
    let server = MockServer::start().await;
    Mock::given(method("REPORT")).and(path("/cal/work/"))
        .and(body_string_contains(r#"<C:time-range start="20261101T000000Z" end="20261201T000000Z"/>"#))
        .respond_with(multistatus(fixture("radicale-sync.xml"))).mount(&server).await;
    let from = 1_793_491_200_000; // 2026-11-01T00:00:00Z
    let to = from + 30 * 86_400_000;
    let found = client(&server).members("/cal/work/", Kind::Calendar, Some((from, to))).await.unwrap();
    assert_eq!(found.len(), 1);
}

#[tokio::test]
async fn the_dav_header_says_whether_the_server_schedules() {
    let server = MockServer::start().await;
    Mock::given(method("OPTIONS")).and(path("/"))
        .respond_with(ResponseTemplate::new(200).insert_header("DAV", "1, 3, calendar-access, calendar-auto-schedule"))
        .mount(&server).await;
    assert!(client(&server).auto_schedule().await.unwrap());
    let plain = MockServer::start().await;
    Mock::given(method("OPTIONS"))
        .respond_with(ResponseTemplate::new(200).insert_header("DAV", "1, calendar-access")).mount(&plain).await;
    assert!(!client(&plain).auto_schedule().await.unwrap());
}

#[tokio::test]
async fn a_resource_past_the_limit_is_not_put() {
    let server = MockServer::start().await;
    let big = "x".repeat(mailrs_dav::MOST_RESOURCE_BYTES + 1);
    let refused = client(&server).put("/a.ics", &big, Kind::Calendar, Precondition::NoneMatch).await;
    assert!(matches!(refused, Err(DavError::TooLarge(_))), "{refused:?}");
}

#[tokio::test]
async fn a_multiget_asks_for_fifty_at_a_time() {
    let server = MockServer::start().await;
    Mock::given(method("REPORT")).and(path("/cal/work/"))
        .respond_with(multistatus("<d:multistatus xmlns:d=\"DAV:\"/>".into())).expect(2).mount(&server).await;
    let hrefs: Vec<String> = (0..51).map(|n| format!("/cal/work/{n}.ics")).collect();
    assert!(client(&server).fetch("/cal/work/", Kind::Calendar, &hrefs).await.unwrap().is_empty());
}

fn is_send<T: Send>(_: T) {}

#[test]
fn every_future_a_trait_method_returns_is_send() {
    fn check<D: DavApi>(d: &D) {
        is_send(d.homes());
        is_send(d.collections("/", Kind::Calendar));
        is_send(d.state("/"));
        is_send(d.sync("/", ""));
        is_send(d.members("/", Kind::Calendar, None));
        is_send(d.fetch("/", Kind::Calendar, &[]));
        is_send(d.get("/a"));
        is_send(d.put("/a", "", Kind::Calendar, Precondition::NoneMatch));
        is_send(d.delete("/a", None));
        is_send(d.find_uid("/", "u"));
        is_send(d.auto_schedule());
    }
    let _ = check::<DavClient>;
    let _ = check::<mailrs_dav::fake::FakeDav>;
}

fn redirect_to(target: String) -> ResponseTemplate {
    ResponseTemplate::new(307).insert_header("Location", target.as_str())
}

#[tokio::test]
async fn a_redirect_to_another_site_is_followed_without_the_password() {
    let origin = MockServer::start().await;
    let elsewhere = MockServer::start().await;
    let port = elsewhere.address().port();
    // The same machine under another host name, so another site.
    Mock::given(method("PROPFIND")).respond_with(redirect_to(format!("http://localhost:{port}/cal/"))).mount(&origin).await;
    Mock::given(method("PROPFIND")).respond_with(multistatus(fixture("radicale-sync.xml"))).mount(&elsewhere).await;
    client(&origin).state("/cal/").await.unwrap();
    let seen = elsewhere.received_requests().await.unwrap();
    assert_eq!(seen.len(), 1, "the client follows the redirect");
    assert!(!seen[0].headers.contains_key("authorization"), "the password left the site");
}

#[tokio::test]
async fn a_redirect_within_the_site_keeps_the_password() {
    let origin = MockServer::start().await;
    let same_site = MockServer::start().await;
    let port = same_site.address().port();
    Mock::given(method("PROPFIND")).respond_with(redirect_to(format!("http://127.0.0.1:{port}/cal/"))).mount(&origin).await;
    Mock::given(method("PROPFIND")).respond_with(multistatus(fixture("radicale-sync.xml"))).mount(&same_site).await;
    client(&origin).state("/cal/").await.unwrap();
    let seen = same_site.received_requests().await.unwrap();
    assert!(seen[0].headers.contains_key("authorization"));
}

fn one_event_answer(href: &str) -> String {
    format!(
        r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:"><d:response><d:href>{href}</d:href>
<d:propstat><d:prop><d:getetag>"1"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:sync-token>t1</d:sync-token></d:multistatus>"#
    )
}

#[tokio::test]
async fn an_encoded_href_in_an_answer_comes_back_decoded() {
    let server = MockServer::start().await;
    Mock::given(method("REPORT")).and(path("/me@example.test/work/"))
        .respond_with(multistatus(one_event_answer("/me%40example.test/work/e1.ics"))).mount(&server).await;
    let synced = client(&server).sync("/me@example.test/work/", "").await.unwrap();
    assert_eq!(synced.changed[0].href, format!("{}/me@example.test/work/e1.ics", server.uri()));
}

#[tokio::test]
async fn a_request_for_an_href_with_a_space_or_an_at_sign_is_encoded_on_the_wire() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/me@example.test/my%20work/a%20b.ics"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"1\"").set_body_string("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n"))
        .expect(2)
        .mount(&server)
        .await;
    let dav = client(&server);
    // The canonical form, and the server's own spelling of it, reach one URL.
    let fetched = dav.get("/me@example.test/my work/a b.ics").await.unwrap();
    dav.get("/me%40example.test/my%20work/a%20b.ics").await.unwrap();
    assert_eq!(fetched.href, format!("{}/me@example.test/my work/a b.ics", server.uri()));
}
