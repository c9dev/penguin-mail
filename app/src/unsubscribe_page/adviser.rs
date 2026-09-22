//! The model that answers when the rules cannot read a page.
//!
//! It is the second [`Adviser`], beside the fake the run is tested
//! through, and it is deliberately the smaller half of the decision. The
//! model sees the page as data, never as markup, and it gets no tools,
//! so the only thing it can do is name ids that are already on the page.
//! [`crate::unsubscribe_page::valid`] then holds its answer to the same
//! rules a plan the rules wrote themselves has to pass, which is why
//! nothing here checks the plan: an invented button is caught one step
//! later, in [`crate::unsubscribe_page::prepare`].
//!
//! Anything but the JSON the prompt asked for reads as no answer. Prose,
//! a half-written object and `{"unsure": true}` all end the same way,
//! with the page opening in the person's own browser.

use std::sync::Arc;
use std::time::Duration;

use mailrs_ai::{AgentEvent, Conversation, NoTools, ProviderConfig};

use super::{Adviser, Answer, PageForm, Plan};
use crate::assistant;
use crate::settings::{AiSettings, Feature};

/// What the model is told, once per page. It is not translated: it goes
/// to a model, not to a person.
const INSTRUCTION: &str = "You pick what to press on a newsletter's unsubscribe page. \
     Answer only JSON: {\"form\":n,\"fill\":[[field,\"<address>\"]],\"tick\":[ids],\"press\":id} \
     or {\"unsure\":true}. Fill only the address given. Never choose between topics.";

/// The Unsubscribing feature's model, asked one page at a time.
pub struct ModelAdviser {
    config: ProviderConfig,
    /// Where the asking runs. A page is read from the GTK loop, and some
    /// providers need the tokio runtime around them: Claude Code, for one,
    /// opens the socket its tools are served on.
    runtime: tokio::runtime::Handle,
}

impl ModelAdviser {
    pub fn new(config: ProviderConfig, runtime: tokio::runtime::Handle) -> ModelAdviser {
        ModelAdviser { config, runtime }
    }
}

/// The adviser the Unsubscribing feature is set to, or nothing when it
/// has no model. Whoever runs a page passes the answer straight to
/// [`crate::unsubscribe_page::prepare`], so a feature turned off in
/// Preferences means an unreadable page goes to the browser without a
/// model ever being asked.
pub fn model_adviser(ai: &AiSettings, runtime: tokio::runtime::Handle) -> Option<ModelAdviser> {
    match assistant::model_for(ai, Feature::Unsubscribe) {
        Ok(config) => Some(ModelAdviser::new(config, runtime)),
        Err(why) => {
            tracing::debug!(reason = %why, "no model reads unsubscribe pages");
            None
        }
    }
}

impl Adviser for ModelAdviser {
    fn advise(&self, form: &PageForm, address: &str) -> Answer<'_, Option<Plan>> {
        let question = question(form, address);
        let asking = self.runtime.spawn(ask(self.config.clone(), question));
        // A task that died answers nothing, which sends the page to the
        // browser rather than leaving the dialog reading it for ever.
        Box::pin(async move { asking.await.ok().flatten() })
    }
}

/// The page and the address, as one message. The page has been through
/// the extraction script, so it carries labels and ids and none of the
/// markup the sender wrote.
fn question(form: &PageForm, address: &str) -> String {
    let page = serde_json::to_string(form).unwrap_or_default();
    format!("The newsletter was sent to {address}.\n\nThe page:\n{page}")
}

/// How long the model has to answer before the page goes to the browser.
/// The person is looking at a spinner the whole time.
const PATIENCE: Duration = Duration::from_secs(60);

async fn ask(config: ProviderConfig, question: String) -> Option<Plan> {
    let mut chat = Conversation::new(config, INSTRUCTION.to_string());
    // Nobody watches this go by, and the agent loop carries on past a
    // channel with no reader.
    let (events, watching) = async_channel::unbounded::<AgentEvent>();
    drop(watching);
    let asking = chat.send(question, Arc::new(NoTools), events);
    match tokio::time::timeout(PATIENCE, asking).await {
        Ok(Ok(reply)) => read_plan(&reply),
        Ok(Err(err)) => {
            tracing::info!(error = %err, "the model could not be asked about an unsubscribe page");
            None
        }
        Err(_) => {
            tracing::info!("the model took too long over an unsubscribe page");
            None
        }
    }
}

/// The model's answer, read strictly. A plan has to name a form and a
/// button; `fill` and `tick` may be left out, because a page often wants
/// neither. Everything else is no answer.
fn read_plan(reply: &str) -> Option<Plan> {
    let said: serde_json::Value = serde_json::from_str(reply.trim()).ok()?;
    let object = said.as_object()?;
    if object.contains_key("unsure")
        || !object.contains_key("form")
        || !object.contains_key("press")
    {
        return None;
    }
    serde_json::from_value(said).ok()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::unsubscribe_page::fake::FakeBrowser;
    use crate::unsubscribe_page::{Step, prepare};

    const ME: &str = "david@example.com";
    const URL: &str = "https://preferences.forum.example/p/12";
    const TOPICS: &str = include_str!("fixtures/topics.json");

    /// A server that speaks the chat completions API and says one thing,
    /// which stands in for whichever model the owner chose.
    async fn model_saying(reply: &str) -> MockServer {
        let server = MockServer::start().await;
        let chunk: Value = json!({"choices": [
            {"index": 0, "delta": {"role": "assistant", "content": reply}, "finish_reason": null}
        ]});
        let body = format!("data: {chunk}\n\ndata: [DONE]\n\n");
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .mount(&server)
            .await;
        server
    }

    fn adviser(server: &MockServer) -> ModelAdviser {
        ModelAdviser::new(ProviderConfig::OpenAiCompatible {
            base_url: format!("{}/v1", server.uri()),
            api_key: None,
            model: "qwen3".into(),
        }, tokio::runtime::Handle::current())
    }

    fn topics() -> PageForm {
        serde_json::from_str(TOPICS).expect("the fixture is a PageForm")
    }

    /// A page is read from the GTK loop, which is no tokio runtime. Asked
    /// from there, the model used to panic for want of one and leave the
    /// dialog reading the page for ever.
    #[test]
    fn the_model_is_asked_from_outside_the_runtime() {
        let runtime = tokio::runtime::Runtime::new().expect("a runtime");
        let server = runtime.block_on(model_saying(
            r#"{"form":0,"fill":[],"tick":[],"press":4}"#,
        ));
        let adviser = ModelAdviser::new(
            ProviderConfig::OpenAiCompatible {
                base_url: format!("{}/v1", server.uri()),
                api_key: None,
                model: "qwen3".into(),
            },
            runtime.handle().clone(),
        );
        let plan = futures::executor::block_on(adviser.advise(&topics(), ME));
        assert_eq!(plan.map(|p| p.press), Some(4));
    }

    #[tokio::test]
    async fn a_plan_in_json_comes_back_as_a_plan() {
        let server = model_saying(r#"{"form":0,"fill":[],"tick":[1,2,3],"press":4}"#).await;
        let plan = adviser(&server)
            .advise(&topics(), ME)
            .await
            .expect("the model answered a plan");
        assert_eq!(
            plan,
            Plan {
                form: 0,
                fill: Vec::new(),
                tick: vec![1, 2, 3],
                press: 4,
            }
        );
    }

    #[tokio::test]
    async fn a_plan_the_page_does_not_hold_is_the_runs_to_refuse() {
        let server = model_saying(r#"{"form":0,"fill":[],"tick":[],"press":42}"#).await;
        let adviser = adviser(&server);
        // The adviser hands the plan on as the model wrote it. Nothing
        // here reads the page; `prepare` runs `valid` over the answer
        // and sends this one to the browser.
        let plan = adviser.advise(&topics(), ME).await.expect("a plan");
        assert_eq!(plan.press, 42);
        let browser = FakeBrowser::holding(URL, topics());
        let prepared = prepare(&browser, Some(&adviser), URL, ME).await;
        assert!(matches!(prepared.step, Step::Browser(_)));
        assert!(browser.submissions().is_empty());
    }

    #[tokio::test]
    async fn prose_is_no_answer() {
        let server = model_saying("I think you should press the Save preferences button.").await;
        assert_eq!(adviser(&server).advise(&topics(), ME).await, None);
    }

    #[tokio::test]
    async fn a_model_that_says_it_is_unsure_is_no_answer() {
        let server = model_saying(r#"{"unsure": true}"#).await;
        assert_eq!(adviser(&server).advise(&topics(), ME).await, None);
    }

    #[tokio::test]
    async fn a_model_with_nothing_to_answer_leaves_the_page_to_the_person() {
        let server = model_saying(r#"{"unsure": true}"#).await;
        let browser = FakeBrowser::holding(URL, topics());
        let prepared = prepare(&browser, Some(&adviser(&server)), URL, ME).await;
        assert!(matches!(prepared.step, Step::Browser(_)));
    }

    #[tokio::test]
    async fn the_feature_with_no_model_asks_nobody() {
        let runtime = tokio::runtime::Handle::current();
        assert!(model_adviser(&AiSettings::default(), runtime).is_none());
    }
}
