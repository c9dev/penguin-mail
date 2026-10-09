//! The assistant: which model to use, what it may do, and how its tool
//! calls reach the window. The GTK pane lives in `ui::assistant`.

mod host;
pub mod language;
mod markup;
mod prompt;
pub mod run;
pub mod sources;
pub mod turn;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use mailrs_ai::ProviderConfig;
use mailrs_gmail::{KeyringTokenStore, TokenStore};

pub use host::{Host, ToolRequest};
pub use markup::to_pango;
pub use prompt::SYSTEM_PROMPT;

use crate::settings::{AiProvider, AiSettings, Feature};
use mailrs_domain::translate::gettext;

/// The keyring service that holds the assistant's API keys.
const KEY_SERVICE: &str = "mailrs-ai";

/// Keyring entry names, one per service that takes a key.
pub const LOCAL_KEY: &str = "local";
pub const ANTHROPIC_KEY: &str = "anthropic";
pub const BRAVE_KEY: &str = "brave-search";

fn keys() -> KeyringTokenStore {
    KeyringTokenStore::with_service(KEY_SERVICE)
}

/// Keys read from the keyring, so the GTK thread never waits on it.
static CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// Reads every key from the keyring on a background thread. Call once at
/// startup; until it finishes, no key is known.
pub fn preload_keys() {
    std::thread::spawn(|| {
        let store = keys();
        let found: HashMap<String, String> = [LOCAL_KEY, ANTHROPIC_KEY, BRAVE_KEY]
            .into_iter()
            .filter_map(|name| {
                let key = store.load(name).ok().flatten()?;
                (!key.is_empty()).then(|| (name.to_string(), key))
            })
            .collect();
        let mut cache = CACHE.lock().expect("the key cache lock is never poisoned");
        // Keys saved while loading win over what the keyring had.
        let mut merged = found;
        merged.extend(cache.take().unwrap_or_default());
        *cache = Some(merged);
    });
}

/// A stored API key, or `None` when there is none or it hasn't loaded yet.
pub fn load_key(name: &str) -> Option<String> {
    CACHE
        .lock()
        .expect("the key cache lock is never poisoned")
        .as_ref()
        .and_then(|keys| keys.get(name).cloned())
}

/// A stored key, read from the keyring itself when the startup preload did
/// not cover it, as for an MCP server's token. It can wait on the keyring,
/// so it belongs off the GTK thread.
pub fn read_key(name: &str) -> Option<String> {
    if let Some(key) = load_key(name) {
        return Some(key);
    }
    let key = keys().load(name).ok().flatten().filter(|k| !k.is_empty())?;
    CACHE
        .lock()
        .expect("the key cache lock is never poisoned")
        .get_or_insert_with(HashMap::new)
        .insert(name.to_string(), key.clone());
    Some(key)
}

/// Stores an API key, or removes it when empty. The keyring write happens
/// in the background.
pub fn save_key(name: &str, key: &str) {
    let (name, key) = (name.to_string(), key.trim().to_string());
    {
        let mut cache = CACHE.lock().expect("the key cache lock is never poisoned");
        let keys = cache.get_or_insert_with(HashMap::new);
        if key.is_empty() {
            keys.remove(&name);
        } else {
            keys.insert(name.clone(), key.clone());
        }
    }
    std::thread::spawn(move || {
        let store = keys();
        let result = if key.is_empty() {
            store.delete(&name)
        } else {
            store.save(&name, &key)
        };
        if let Err(err) = result {
            tracing::warn!(error = %err, "could not store the assistant's API key");
        }
    });
}

/// The model a feature runs on, with keys from the keyring. This is the one
/// way to a `ProviderConfig`, so every feature names itself and gets the
/// model chosen for it on the AI page.
pub fn model_for(ai: &AiSettings, feature: Feature) -> Result<ProviderConfig, String> {
    let (connection, model) = ai.resolved(feature);
    let model = model.trim().to_string();
    match connection {
        AiProvider::Off => Err(match feature {
            Feature::Assistant => gettext("The assistant is off. Choose a model in Preferences."),
            Feature::Translation => {
                gettext("Translation has no model. Choose one in Preferences, on the AI page.")
            }
            Feature::Unsubscribe => {
                gettext("Unsubscribing has no model. Choose one in Preferences, on the AI page.")
            }
        }),
        AiProvider::Local => {
            if model.is_empty() {
                return Err(gettext(
                    "Choose a model for the local server in Preferences.",
                ));
            }
            Ok(ProviderConfig::OpenAiCompatible {
                base_url: ai.base_url.trim().to_string(),
                api_key: load_key(LOCAL_KEY),
                model,
            })
        }
        AiProvider::Anthropic => {
            let api_key = load_key(ANTHROPIC_KEY)
                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
                .ok_or_else(|| gettext("Add an Anthropic API key in Preferences."))?;
            Ok(ProviderConfig::Anthropic { api_key, model })
        }
        AiProvider::ClaudeCode => {
            let command = if ai.claude_command.trim().is_empty() {
                find_claude().ok_or_else(|| {
                    gettext(
                        "Claude Code is not installed. Install it and sign in, then try \
                         again.",
                    )
                })?
            } else {
                PathBuf::from(ai.claude_command.trim())
            };
            Ok(ProviderConfig::ClaudeCode {
                command,
                model: Some(model).filter(|m| !m.is_empty()),
            })
        }
    }
}

/// Whether the assistant has a model to answer with, as far as the
/// settings say: a connection other than Off, and for a local server a
/// model named. The keyring is not read, so asking costs nothing; a
/// missing key shows in the pane when the question goes.
pub fn offers_summary(ai: &AiSettings) -> bool {
    match ai.resolved(Feature::Assistant) {
        (AiProvider::Off, _) => false,
        (AiProvider::Local, model) => !model.trim().is_empty(),
        _ => true,
    }
}

/// Whether a header bar's assistant toggle should show, and whether it
/// should show pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssistantToggle {
    /// Shown under the same condition Summarize offers itself
    /// ([`offers_summary`]): hidden while the assistant is off in
    /// Preferences.
    pub visible: bool,
    /// Pressed while the assistant's panel is open.
    pub pressed: bool,
}

/// The reading pane's and the calendar's assistant toggle, from the
/// assistant's settings and whether its panel is open now.
pub fn assistant_toggle(ai: &AiSettings, panel_open: bool) -> AssistantToggle {
    AssistantToggle {
        visible: offers_summary(ai),
        pressed: panel_open,
    }
}

/// The `claude` command, from `PATH` or its usual install places.
pub fn find_claude() -> Option<PathBuf> {
    let on_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("claude"))
            .find(|p| p.is_file())
    });
    on_path.or_else(|| {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        [".local/bin/claude", ".claude/local/claude"]
            .iter()
            .map(|p| home.join(p))
            .find(|p| p.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Use;

    fn local(model: &str) -> AiSettings {
        AiSettings {
            provider: AiProvider::Local,
            base_url: "http://127.0.0.1:1234/v1".into(),
            local_model: model.into(),
            ..AiSettings::default()
        }
    }

    fn model_of(config: ProviderConfig) -> String {
        match config {
            ProviderConfig::OpenAiCompatible { model, .. } => model,
            other => panic!("expected the local server, got {other:?}"),
        }
    }

    #[test]
    fn a_feature_following_the_assistant_gets_the_assistants_model() {
        let ai = local("qwen3");
        assert_eq!(ai.use_for(Feature::Translation), Use::SameAsAssistant);
        assert_eq!(
            model_for(&ai, Feature::Translation),
            model_for(&ai, Feature::Assistant)
        );
        assert_eq!(
            model_of(model_for(&ai, Feature::Translation).unwrap()),
            "qwen3"
        );
    }

    #[test]
    fn a_feature_with_a_model_of_its_own_gets_that_one() {
        let mut ai = local("qwen3");
        ai.set_use(
            Feature::Translation,
            Use::Model {
                connection: AiProvider::Local,
                model: " gemma-3 ".into(),
            },
        );
        assert_eq!(
            model_of(model_for(&ai, Feature::Translation).unwrap()),
            "gemma-3"
        );
        assert_eq!(
            model_of(model_for(&ai, Feature::Assistant).unwrap()),
            "qwen3"
        );
    }

    #[test]
    fn a_feature_can_be_off_while_the_assistant_runs() {
        let mut ai = local("qwen3");
        ai.set_use(
            Feature::Translation,
            Use::Model {
                connection: AiProvider::Off,
                model: String::new(),
            },
        );
        let problem = model_for(&ai, Feature::Translation).expect_err("translation is off");
        assert!(problem.contains("Translation"), "{problem}");
        assert!(model_for(&ai, Feature::Assistant).is_ok());
    }

    #[test]
    fn with_the_assistant_off_a_follower_says_it_has_no_model() {
        let ai = AiSettings::default();
        for feature in Feature::ALL {
            let problem = model_for(&ai, feature).expect_err("nothing is chosen");
            assert!(problem.contains("Preferences"), "{feature:?}: {problem}");
        }
    }

    #[test]
    fn a_local_server_needs_a_model_name() {
        let problem = model_for(&local("  "), Feature::Assistant).expect_err("no model");
        assert!(problem.contains("local server"), "{problem}");
    }

    #[test]
    fn summarize_waits_for_an_assistant_with_a_model() {
        let mut ai = crate::settings::AiSettings::default();
        assert!(!offers_summary(&ai), "the assistant starts off");
        ai.provider = crate::settings::AiProvider::Local;
        assert!(!offers_summary(&ai), "a local server with no model named");
        ai.local_model = "qwen3".into();
        assert!(offers_summary(&ai));
        ai.provider = crate::settings::AiProvider::Anthropic;
        assert!(offers_summary(&ai));
    }

    #[test]
    fn the_assistant_toggle_hides_while_the_assistant_is_off() {
        let ai = crate::settings::AiSettings::default();
        assert_eq!(
            assistant_toggle(&ai, false),
            AssistantToggle {
                visible: false,
                pressed: false
            }
        );
    }

    #[test]
    fn the_assistant_toggle_shows_and_follows_the_panel_once_the_assistant_has_a_model() {
        let ai = crate::settings::AiSettings {
            provider: crate::settings::AiProvider::Anthropic,
            ..Default::default()
        };
        assert_eq!(
            assistant_toggle(&ai, false),
            AssistantToggle {
                visible: true,
                pressed: false
            }
        );
        assert_eq!(
            assistant_toggle(&ai, true),
            AssistantToggle {
                visible: true,
                pressed: true
            }
        );
    }
}
