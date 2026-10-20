//! Signature choices keep existing local and server signatures until changed.

use serde::{Deserialize, Serialize};

use super::Settings;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureMode {
    Account,
    Custom,
    None,
}

impl Settings {
    /// The saved text, including an empty custom signature, else the server's text.
    pub fn signature_text(&self, account: &str, email: &str) -> &str {
        if let Some(text) = self.signatures.get(&email.to_lowercase()) {
            return text;
        }
        self.send_as
            .get(&account.to_lowercase())
            .and_then(|senders| senders.iter().find(|s| s.email.eq_ignore_ascii_case(email)))
            .map_or("", |s| s.signature.as_str())
    }

    pub fn signature_mode(&self, account: &str, email: &str) -> SignatureMode {
        self.signature_modes
            .get(&email.to_lowercase())
            .copied()
            .unwrap_or_else(|| {
                if self.signature_text(account, email).is_empty() {
                    SignatureMode::Account
                } else {
                    SignatureMode::Custom
                }
            })
    }

    /// Exact overrides win, then the matching suffix base, then the account.
    pub fn signature_for(&self, account: &str, email: &str) -> &str {
        let exact = email.to_lowercase();
        let base =
            if self.signatures.contains_key(&exact) || self.signature_modes.contains_key(&exact) {
                email
            } else {
                self.send_as
                    .get(&account.to_lowercase())
                    .and_then(|senders| crate::compose::sender_for(senders, email))
                    .map_or(email, |s| s.email.as_str())
            };
        if base.eq_ignore_ascii_case(account) {
            return self.signature_text(account, account);
        }
        match self.signature_mode(account, base) {
            SignatureMode::Account => self.signature_text(account, account),
            SignatureMode::Custom => self.signature_text(account, base),
            SignatureMode::None => "",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::{SendAsAddress, Suffixes};

    #[test]
    fn signatures_follow_exact_addresses_suffixes_and_explicit_choices() {
        let mut settings = Settings::default();
        let account = "me@example.com";
        let alias = "sales@second.example";
        settings.set_signature(account, "Common <email>");
        settings.send_as.insert(
            account.into(),
            vec![SendAsAddress {
                email: alias.into(),
                signature: "From the server".into(),
                suffixes: Suffixes {
                    plus: true,
                    dot: true,
                },
                ..Default::default()
            }],
        );
        assert_eq!(settings.signature_for(account, alias), "From the server");
        settings.set_signature(alias, "Local <email>");
        assert_eq!(
            settings.signature_for(account, "sales+Shop@second.example"),
            "Local <email>"
        );
        assert_eq!(
            settings.signature_for(account, "SALES.Shop@SECOND.EXAMPLE"),
            "Local <email>"
        );
        for (mode, expected) in [
            (SignatureMode::Account, "Common <email>"),
            (SignatureMode::Custom, "Custom <domain>"),
            (SignatureMode::None, ""),
        ] {
            settings.signature_modes.insert(alias.into(), mode);
            settings
                .signatures
                .insert(alias.into(), "Custom <domain>".into());
            let saved = toml::to_string(&settings).unwrap();
            let loaded: Settings = toml::from_str(&saved).unwrap();
            assert_eq!(loaded.signature_for(account, alias), expected);
            assert_eq!(
                loaded.signature_for(account, "sales+Shop@second.example"),
                expected
            );
        }
        settings.set_signature("sales+special@second.example", "Specific");
        assert_eq!(
            settings.signature_for(account, "sales+special@second.example"),
            "Specific"
        );
        assert_eq!(
            settings.signature_for(account, "other@third.example"),
            "Common <email>"
        );
    }
    #[test]
    fn renaming_or_removing_a_sender_keeps_signatures_used_by_another_account() {
        let mut settings = Settings::default();
        let account = "me@example.com";
        let old = "old@example.com";
        let new = "new@example.com";
        assert!(settings.save_sender(
            account,
            None,
            SendAsAddress {
                email: old.into(),
                ..Default::default()
            }
        ));
        crate::settings::Change::SenderSignature {
            email: old.into(),
            mode: SignatureMode::None,
            text: "My signature".into(),
        }
        .apply_to(&mut settings);
        assert!(settings.save_sender(
            account,
            Some(old),
            SendAsAddress {
                email: new.into(),
                ..Default::default()
            }
        ));
        assert_eq!(settings.signature_mode(account, new), SignatureMode::None);
        assert_eq!(settings.signature_text(account, new), "My signature");
        assert_eq!(settings.signature_for(old, old), "My signature");
        assert_eq!(settings.signature_modes[old], SignatureMode::None);
        settings.remove_sender(account, new);
        assert_eq!(settings.signature_modes[new], SignatureMode::None);
        assert_eq!(settings.signature_text(new, new), "My signature");
    }
}
