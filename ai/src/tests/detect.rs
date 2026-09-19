use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::ProviderConfig;
use crate::detect::{Probe, probe_servers};

#[tokio::test]
async fn finds_servers_that_answer_and_skips_the_rest() {
    let lm_studio = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "qwen3-8b"}, {"id": "gemma-3"}],
        })))
        .mount(&lm_studio)
        .await;
    let locked = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&locked)
        .await;
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let closed_port = closed.local_addr().unwrap().port();
    drop(closed);

    let lm_port = lm_studio.address().port();
    let probes = vec![
        Probe {
            name: "LM Studio",
            base_url: format!("{}/v1", lm_studio.uri()),
        },
        Probe {
            name: "Ollama",
            base_url: format!("http://127.0.0.1:{closed_port}/v1"),
        },
        Probe {
            name: "Unsloth Studio",
            base_url: format!("{}/v1", locked.uri()),
        },
    ];
    let found = probe_servers(&probes).await;

    assert_eq!(found.len(), 2);
    assert_eq!(found[0].label, format!("LM Studio on port {lm_port}"));
    assert_eq!(found[0].models, vec!["qwen3-8b", "gemma-3"]);
    assert_eq!(
        found[0].config,
        ProviderConfig::OpenAiCompatible {
            base_url: format!("{}/v1", lm_studio.uri()),
            api_key: None,
            model: "qwen3-8b".into(),
        }
    );
    assert_eq!(
        found[1].label,
        format!(
            "Unsloth Studio on port {} (needs an API key)",
            locked.address().port()
        )
    );
    assert!(found[1].models.is_empty());
}
