//! Each provider's services built in one place, from what the account's
//! own server offers and the servers found beside it. The connector and
//! the fakes build through the same functions, so a test runs the setup
//! an account really gets. Google and Microsoft serve everything over one
//! client and need nothing here: [`AccountServices::google`] and
//! [`AccountServices::microsoft`] are the whole of their setup.
//!
//! Nothing here connects: each client signs in when sync first asks.

use std::sync::Arc;

use mailrs_dav::{DavClient, Kind as DavKind, Login as DavLogin};
use mailrs_domain::AccountId;
use mailrs_sieve::client::{Login as SieveLogin, ManageSieveClient};
use mailrs_store::services::{FoundService, ServiceKind};

use super::{
    AccountServices, AnyAutoReply, AnyCalendar, AnyContacts, AnyDav, AnyIdentities, AnyJmap,
    AnyMail, AnyRules, AnySieve, CalDav, CardDav, Jmap, JmapSettings, JmapSieve, LocalRules,
    RulesBeside, SERVER_REPLY_SCRIPT, SieveRules,
};

/// A calendar, contacts or rules server found for the account and
/// confirmed, with what it signs in with: the password, or with `bearer`
/// the token a browser sign-in shares.
pub(crate) struct FoundServer {
    pub service: FoundService,
    pub secret: String,
    pub bearer: Option<Arc<dyn mailrs_dav::TokenSource>>,
}

/// What an account's services are built from beside its own server.
pub(crate) struct Beside {
    pub account_id: AccountId,
    /// The account's address, which a calendar server answers invitations as.
    pub address: String,
    pub provider_name: String,
    /// The servers that serve what the account's own server lacks.
    pub found: Vec<FoundServer>,
    /// The rules kept on this computer, which run when no server runs
    /// rules. `None` for a fake that keeps no store.
    pub local: Option<LocalRules>,
    /// Whether rules are kept on this computer now. Beside a script on the
    /// server they keep running, as after a switch from IMAP that could
    /// not move them.
    pub local_kept: bool,
}

impl Beside {
    /// Nothing beside the server, for a fake with no store.
    #[cfg(any(test, feature = "fake"))]
    pub(crate) fn nothing(address: &str, provider_name: &str) -> Beside {
        Beside {
            account_id: 0,
            address: address.to_string(),
            provider_name: provider_name.to_string(),
            found: Vec::new(),
            local: None,
            local_kept: false,
        }
    }
}

impl AccountServices {
    /// A JMAP account's services. The session's own contacts and calendar
    /// serve where it offers them, and a found server where it does not.
    /// On a session that runs Sieve the automatic reply goes in the rules
    /// script: Stalwart keeps JMAP's own reply as a script of its own, and
    /// only one script runs, so the two would turn each other off. With
    /// no Sieve on the session, JMAP's own reply wins over one a
    /// ManageSieve server brings with its rules.
    pub(crate) fn jmap_from(
        client: impl Into<AnyJmap>,
        settings: JmapSettings,
        beside: Beside,
    ) -> AccountServices {
        let offers = |capability: &str| settings.capabilities.iter().any(|c| c == capability);
        let (contacts, calendars, sieve, vacation) = (
            offers(mailrs_jmap::CONTACTS),
            offers(mailrs_jmap::CALENDARS),
            offers(mailrs_jmap::SIEVE),
            offers(mailrs_jmap::VACATION),
        );
        // The adapter and the Sieve rules share one client: clones share
        // the session, the pool and the permits.
        let api = Arc::new(client.into());
        let adapter = Jmap::new(Arc::clone(&api), settings);
        let mut services = AccountServices {
            mail: AnyMail::Jmap(adapter.clone()),
            calendar: calendars.then(|| AnyCalendar::Jmap(adapter.clone())),
            contacts: contacts.then(|| AnyContacts::Jmap(adapter.clone())),
            rules: None,
            auto_reply: None,
            identities: AnyIdentities::Jmap(adapter.clone()),
        };
        if sieve {
            let rules = SieveRules::new(
                Arc::new(JmapSieve::new(api)),
                services.mail.clone(),
                beside.address.clone(),
                beside.provider_name.clone(),
            )
            .beside_server_reply(vacation.then_some(SERVER_REPLY_SCRIPT));
            services = services.with_auto_reply(AnyAutoReply::JmapSieve(rules.clone()));
            services = match (&beside.local, beside.local_kept) {
                (Some(local), true) => {
                    services.with_rules(AnyRules::Beside(RulesBeside::new(rules, local.clone())))
                }
                _ => services.with_rules(AnyRules::JmapSieve(rules)),
            };
        }
        let mut services = services.with_found(&beside);
        if vacation && !sieve {
            services = services.with_auto_reply(AnyAutoReply::Jmap(adapter));
        }
        services.or_local_rules(beside.local)
    }

    /// `self` with each found server behind the service it offers, then
    /// the rules kept on this computer where no server runs rules.
    pub(crate) fn with_beside(self, beside: Beside) -> AccountServices {
        self.with_found(&beside).or_local_rules(beside.local)
    }

    fn with_found(self, beside: &Beside) -> AccountServices {
        beside
            .found
            .iter()
            .fold(self, |services, found| services.attach(found, beside))
    }

    fn or_local_rules(self, local: Option<LocalRules>) -> AccountServices {
        match (&self.rules, local) {
            (None, Some(local)) => self.with_rules(AnyRules::Local(local)),
            _ => self,
        }
    }

    /// `self` with the server `found` names put behind the service it
    /// offers. A server that cannot be set up is logged and left out, so
    /// the account keeps its mail. A Sieve row is passed over for an
    /// account whose folders live on this computer, as a POP3 account's
    /// do: its mail never reaches server rules.
    fn attach(self, found: &FoundServer, beside: &Beside) -> AccountServices {
        let (service, secret, id) = (&found.service, found.secret.as_str(), beside.account_id);
        let with_bearer = |client: DavClient| match &found.bearer {
            Some(source) => client.with_bearer(Arc::clone(source)),
            None => client,
        };
        match service.kind {
            ServiceKind::CalDav => {
                match DavClient::new(
                    &service.url,
                    DavKind::Calendar,
                    DavLogin::new(&service.user_name, secret),
                )
                .map(with_bearer)
                {
                    Ok(client) => {
                        let calendar = CalDav::new(
                            Arc::new(AnyDav::from(client)),
                            self.mail.clone(),
                            vec![beside.address.clone()],
                        );
                        self.with_calendar(AnyCalendar::Dav(calendar))
                    }
                    Err(err) => {
                        tracing::warn!(account = id, %err, "could not set up the calendar server");
                        self
                    }
                }
            }
            ServiceKind::CardDav => {
                match DavClient::new(
                    &service.url,
                    DavKind::AddressBook,
                    DavLogin::new(&service.user_name, secret),
                )
                .map(with_bearer)
                {
                    Ok(client) => self.with_contacts(AnyContacts::Dav(CardDav::new(Arc::new(
                        AnyDav::from(client),
                    )))),
                    Err(err) => {
                        tracing::warn!(account = id, %err, "could not set up the contacts server");
                        self
                    }
                }
            }
            ServiceKind::Sieve if self.capabilities().local_mailboxes => self,
            ServiceKind::Sieve => {
                let (host, port) = match service.url.rsplit_once(':') {
                    Some((host, port)) => (host, port.parse().unwrap_or(mailrs_sieve::PORT)),
                    None => (service.url.as_str(), mailrs_sieve::PORT),
                };
                let client =
                    ManageSieveClient::new(host, port, SieveLogin::new(&service.user_name, secret));
                let rules = SieveRules::new(
                    Arc::new(AnySieve::from(client)),
                    self.mail.clone(),
                    beside.address.clone(),
                    beside.provider_name.clone(),
                );
                self.with_rules(AnyRules::Sieve(rules.clone()))
                    .with_auto_reply(AnyAutoReply::Sieve(rules))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mailrs_jmap::{CALENDARS, CONTACTS, SIEVE, VACATION};
    use mailrs_store::Db;
    use mailrs_store::services::{FoundService, ServiceKind};

    use super::{Beside, FoundServer};
    use crate::fake::FakeJmap;
    use crate::{
        AccountServices, AnyAutoReply, AnyCalendar, AnyContacts, AnyRules, JmapSettings, LocalRules,
    };

    fn settings(offers: &[&str]) -> JmapSettings {
        JmapSettings {
            address: "me@example.com".into(),
            provider_name: "Stalwart".into(),
            window_days: crate::DEFAULT_WINDOW_DAYS,
            labels: false,
            capabilities: offers.iter().map(|c| c.to_string()).collect(),
            auto_reply_needs_sign_in: false,
        }
    }

    fn found(kind: ServiceKind, url: &str) -> FoundServer {
        let service = FoundService {
            kind,
            url: url.into(),
            user_name: "me".into(),
            confirmed: true,
            source: "table".into(),
        };
        FoundServer {
            service,
            secret: "app password".into(),
            bearer: None,
        }
    }

    /// A JMAP account whose session offers `offers`, with `found` beside
    /// it and the rules kept in a store of their own, which hold some when
    /// `kept`.
    fn jmap(offers: &[&str], found: Vec<FoundServer>, kept: bool) -> AccountServices {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let beside = Beside {
            account_id: 1,
            address: "me@example.com".into(),
            provider_name: "Stalwart".into(),
            found,
            local: Some(LocalRules::new(db, 1)),
            local_kept: kept,
        };
        AccountServices::jmap_from(Arc::new(FakeJmap::new()), settings(offers), beside)
    }

    #[test]
    fn a_session_with_sieve_keeps_the_reply_in_the_rules_script() {
        let services = jmap(&[SIEVE, VACATION], Vec::new(), false);
        assert!(matches!(services.rules, Some(AnyRules::JmapSieve(_))));
        assert!(
            matches!(services.auto_reply, Some(AnyAutoReply::JmapSieve(_))),
            "not JMAP's own reply"
        );
    }

    #[test]
    fn a_session_without_sieve_answers_with_its_own_reply_over_a_sieve_server() {
        let services = jmap(
            &[VACATION],
            vec![found(ServiceKind::Sieve, "sieve.example.com:4190")],
            false,
        );
        assert!(
            matches!(services.rules, Some(AnyRules::Sieve(_))),
            "the rules run on the found server"
        );
        assert!(matches!(services.auto_reply, Some(AnyAutoReply::Jmap(_))));
    }

    #[test]
    fn rules_kept_here_run_beside_the_script() {
        let services = jmap(&[SIEVE], Vec::new(), true);
        assert!(matches!(services.rules, Some(AnyRules::Beside(_))));
        let services = jmap(&[], Vec::new(), true);
        assert!(
            matches!(services.rules, Some(AnyRules::Local(_))),
            "no server runs rules"
        );
        assert!(services.auto_reply.is_none());
    }

    #[test]
    fn found_servers_serve_what_the_session_lacks() {
        let own = jmap(&[CALENDARS, CONTACTS], Vec::new(), false);
        assert!(matches!(own.calendar, Some(AnyCalendar::Jmap(_))));
        assert!(matches!(own.contacts, Some(AnyContacts::Jmap(_))));
        let servers = vec![
            found(ServiceKind::CalDav, "https://dav.example.com/calendars/"),
            found(ServiceKind::CardDav, "https://dav.example.com/contacts/"),
        ];
        let found = jmap(&[], servers, false);
        assert!(matches!(found.calendar, Some(AnyCalendar::Dav(_))));
        assert!(matches!(found.contacts, Some(AnyContacts::Dav(_))));
    }

    /// The fake goes through the same setup, so a fake session that runs
    /// Sieve gets the script's reply, as a real one does.
    #[test]
    fn the_fake_is_set_up_as_a_real_account() {
        let fake = FakeJmap::new()
            .with_sieve(&["fileinto", "vacation"])
            .with_capability(VACATION);
        let services = AccountServices::fake_jmap(Arc::new(fake));
        assert!(matches!(services.rules, Some(AnyRules::JmapSieve(_))));
        assert!(matches!(
            services.auto_reply,
            Some(AnyAutoReply::JmapSieve(_))
        ));
    }
}
