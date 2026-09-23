//! Which Google client an account signs in with.

use mailrs_domain::SignInClient;
use mailrs_gmail::OAuthClient;

use crate::config::Config;

/// The client for an account that signed in with `client`. A built-in
/// account takes `built_in`, the build's own; an own account takes the one
/// in `config.toml`. `None` when that client is missing, which the caller
/// treats as needing a new sign-in, and a new sign-in uses the built-in one.
pub fn client_for(
    client: SignInClient,
    config: &Config,
    built_in: Option<OAuthClient>,
) -> Option<OAuthClient> {
    match client {
        SignInClient::BuiltIn => built_in,
        SignInClient::Own => config
            .oauth
            .as_ref()
            .map(|own| OAuthClient::new(&own.client_id, &own.client_secret)),
    }
}

#[cfg(test)]
mod tests {
    use mailrs_domain::SignInClient;
    use mailrs_gmail::client_from;

    use super::*;
    use crate::config::{Config, OAuthConfig};

    fn built_in() -> Option<mailrs_gmail::OAuthClient> {
        client_from(Some("built.apps.googleusercontent.com"), Some("GOCSPX-built"))
    }

    fn with_own() -> Config {
        Config {
            oauth: Some(OAuthConfig {
                client_id: "own.apps.googleusercontent.com".into(),
                client_secret: "GOCSPX-own".into(),
            }),
            ..Config::default()
        }
    }

    #[test]
    fn a_built_in_account_takes_the_builds_client() {
        let client = client_for(SignInClient::BuiltIn, &with_own(), built_in()).unwrap();
        assert_eq!(client.id(), "built.apps.googleusercontent.com");
    }

    #[test]
    fn an_own_account_takes_the_client_in_the_config() {
        let client = client_for(SignInClient::Own, &with_own(), built_in()).unwrap();
        assert_eq!(client.id(), "own.apps.googleusercontent.com");
    }

    #[test]
    fn an_own_account_with_no_client_in_the_config_has_none() {
        assert!(client_for(SignInClient::Own, &Config::default(), built_in()).is_none());
    }

    #[test]
    fn a_built_in_account_in_a_build_without_one_has_none() {
        assert!(client_for(SignInClient::BuiltIn, &with_own(), None).is_none());
    }
}
