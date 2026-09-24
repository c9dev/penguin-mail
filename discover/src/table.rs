//! The built-in provider table, `providers.toml`, compiled in.

use std::sync::LazyLock;

use serde::Deserialize;

use crate::{Found, PasswordKind, ProviderInfo, Server, Source, Unreachable, Verdict, pairs};

static BUILT_IN: LazyLock<Table> = LazyLock::new(|| {
    Table::parse(include_str!("../providers.toml")).unwrap_or_else(|error| {
        // The table's own test parses this file, so this runs only on a
        // build that skipped the tests. Discovery then goes to the network.
        tracing::error!("the built-in provider table does not parse: {error}");
        Table::default()
    })
});

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Table {
    #[serde(rename = "provider")]
    pub(crate) entries: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) kind: Kind,
    #[serde(default)]
    pub(crate) domains: Vec<String>,
    #[serde(default)]
    pub(crate) mx: Vec<String>,
    pub(crate) imap: Option<Server>,
    #[serde(default)]
    pub(crate) smtp: Vec<Server>,
    pub(crate) custom_domain_imap: Option<Server>,
    #[serde(default)]
    pub(crate) custom_domain_smtp: Vec<Server>,
    pub(crate) password: Option<PasswordKind>,
    pub(crate) app_password_url: Option<String>,
    pub(crate) enable_imap_url: Option<String>,
    pub(crate) documentation_url: Option<String>,
    #[serde(default)]
    pub(crate) files_sent_mail: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Imap,
    NoImap,
    NotYet,
    Google,
    Microsoft,
}

impl Table {
    pub(crate) fn parse(text: &str) -> Result<Table, toml::de::Error> {
        toml::from_str(text)
    }

    pub(crate) fn built_in() -> &'static Table {
        &BUILT_IN
    }

    /// The entry that lists `domain` among its domains.
    pub(crate) fn by_domain(&self, domain: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|entry| entry.domains.iter().any(|d| d == domain))
    }

    /// The first IMAP entry whose display name is `name`. Zoho's data
    /// centres and GMX's two families share a name; a test holds entries
    /// that share one to the same sent-copy rule and password, so the
    /// first answers for all.
    pub(crate) fn by_name(&self, name: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|entry| entry.kind == Kind::Imap && entry.name == name)
    }

    /// The entry the first of `hosts` that any entry knows belongs to.
    /// `hosts` come best first, as MX preference orders them.
    pub(crate) fn by_mx(&self, hosts: &[String]) -> Option<&Entry> {
        hosts.iter().find_map(|host| self.by_mx_host(host))
    }

    /// An exact host wins over every pattern, and a longer suffix over a
    /// shorter one, so AOL's own exchanger beats Yahoo's `*.gm0.yahoodns.net`
    /// whatever order the entries sit in.
    fn by_mx_host(&self, host: &str) -> Option<&Entry> {
        let host = host.to_ascii_lowercase();
        let host = host.strip_suffix('.').unwrap_or(&host);
        let exact = self
            .entries
            .iter()
            .find(|entry| entry.mx.iter().any(|pattern| pattern == host));
        exact.or_else(|| {
            self.entries
                .iter()
                .flat_map(|entry| {
                    entry.mx.iter().filter_map(move |pattern| {
                        let suffix = pattern.strip_prefix('*')?;
                        host.ends_with(suffix).then_some((suffix.len(), entry))
                    })
                })
                .max_by_key(|(length, _)| *length)
                .map(|(_, entry)| entry)
        })
    }
}

/// The built-in table's facts about the IMAP provider whose display name
/// is `name`, such as `"Fastmail"`, or `None` when the table lists no IMAP
/// provider by that name. An account stores the name; reading the rest
/// here at each start lets a corrected table reach accounts added before
/// the correction.
pub fn provider_named(name: &str) -> Option<ProviderInfo> {
    Table::built_in().by_name(name).map(Entry::info)
}

impl Entry {
    /// The provider's name, password kind, links and sent-copy rule.
    pub(crate) fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: self.name.clone(),
            password: self.password.unwrap_or(PasswordKind::AccountPassword),
            app_password_url: self.app_password_url.clone(),
            enable_imap_url: self.enable_imap_url.clone(),
            documentation_url: self.documentation_url.clone(),
            files_sent_mail: self.files_sent_mail,
        }
    }

    /// What this entry says about an address. `custom_domain` is true when
    /// the entry was found by MX rather than by the address's own domain.
    pub(crate) fn found(&self, source: Source, custom_domain: bool) -> Found {
        tracing::debug!(provider = %self.id, ?source, "the provider table answered");
        let unreachable = |reason| Found {
            verdict: Verdict::Unreachable {
                provider: self.name.clone(),
                reason,
            },
            candidates: Vec::new(),
        };
        let verdict_only = |verdict| Found {
            verdict,
            candidates: Vec::new(),
        };
        match self.kind {
            Kind::NoImap => unreachable(Unreachable::NoImap),
            Kind::NotYet => unreachable(Unreachable::NotYet),
            Kind::Google => verdict_only(Verdict::Google),
            Kind::Microsoft => verdict_only(Verdict::Microsoft),
            Kind::Imap => {
                let (imap, smtp) = match (&self.custom_domain_imap, custom_domain) {
                    (Some(imap), true) => (imap, &self.custom_domain_smtp),
                    _ => match &self.imap {
                        Some(imap) => (imap, &self.smtp),
                        None => return Found::nothing(),
                    },
                };
                let provider = self.info();
                Found::servers(pairs(
                    source,
                    Some(&provider),
                    std::slice::from_ref(imap),
                    smtp,
                    false,
                ))
                .unwrap_or_else(Found::nothing)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::name::host;
    use crate::{Security, UserName};

    fn table() -> Table {
        Table::parse(include_str!("../providers.toml")).expect("providers.toml parses")
    }

    fn servers_of(entry: &Entry) -> Vec<&Server> {
        entry
            .imap
            .iter()
            .chain(&entry.smtp)
            .chain(&entry.custom_domain_imap)
            .chain(&entry.custom_domain_smtp)
            .collect()
    }

    #[test]
    fn every_id_and_domain_appears_once() {
        let table = table();
        let mut ids = HashSet::new();
        let mut domains = HashSet::new();
        for entry in &table.entries {
            assert!(ids.insert(&entry.id), "id {} twice", entry.id);
            for domain in &entry.domains {
                assert!(domains.insert(domain), "domain {domain} twice");
            }
        }
    }

    #[test]
    fn every_name_in_the_table_is_well_formed() {
        for entry in &table().entries {
            for domain in &entry.domains {
                assert_eq!(
                    host(domain).as_ref(),
                    Some(domain),
                    "{}: {domain}",
                    entry.id
                );
            }
            assert!(!entry.mx.is_empty(), "{} has no MX pattern", entry.id);
            for pattern in &entry.mx {
                let name = pattern.strip_prefix("*.").unwrap_or(pattern);
                assert_eq!(host(name).as_deref(), Some(name), "{}: {pattern}", entry.id);
            }
            for server in servers_of(entry) {
                assert_eq!(
                    host(&server.host).as_ref(),
                    Some(&server.host),
                    "{}",
                    entry.id
                );
                assert_ne!(server.port, 0, "{}", entry.id);
            }
            for url in [
                &entry.app_password_url,
                &entry.enable_imap_url,
                &entry.documentation_url,
            ]
            .into_iter()
            .flatten()
            {
                assert!(url.starts_with("https://"), "{}: {url}", entry.id);
            }
        }
    }

    #[test]
    fn an_imap_entry_has_servers_and_the_others_have_none() {
        for entry in &table().entries {
            if entry.kind == Kind::Imap {
                assert!(entry.imap.is_some(), "{}", entry.id);
                assert!(!entry.smtp.is_empty(), "{}", entry.id);
                assert!(entry.password.is_some(), "{}", entry.id);
                assert_eq!(
                    entry.custom_domain_imap.is_some(),
                    !entry.custom_domain_smtp.is_empty(),
                    "{}",
                    entry.id
                );
            } else {
                assert!(servers_of(entry).is_empty(), "{}", entry.id);
                assert!(entry.password.is_none(), "{}", entry.id);
            }
        }
    }

    #[test]
    fn imap_always_starts_with_tls() {
        for entry in &table().entries {
            for imap in entry.imap.iter().chain(&entry.custom_domain_imap) {
                assert_eq!(imap.security, Security::Tls, "{}", entry.id);
                assert_eq!(imap.port, 993, "{}", entry.id);
            }
        }
    }

    fn verdict_for(domain: &str) -> Verdict {
        table()
            .by_domain(domain)
            .unwrap_or_else(|| panic!("{domain} is in the table"))
            .found(Source::Table, false)
            .verdict
    }

    #[test]
    fn every_provider_the_app_serves_answers_by_domain() {
        for (domain, name) in [
            ("fastmail.com", "Fastmail"),
            ("icloud.com", "iCloud Mail"),
            ("yahoo.com", "Yahoo Mail"),
            ("aol.com", "AOL Mail"),
            ("zohomail.eu", "Zoho Mail"),
            ("gmx.de", "GMX"),
            ("gmx.com", "GMX"),
            ("web.de", "WEB.DE"),
            ("mail.com", "mail.com"),
            ("yandex.ru", "Yandex Mail"),
            ("mailbox.org", "mailbox.org"),
            ("posteo.de", "Posteo"),
        ] {
            let found = table()
                .by_domain(domain)
                .map(|e| e.found(Source::Table, false));
            let found = found.unwrap_or_else(|| panic!("{domain} is in the table"));
            assert_eq!(found.verdict, Verdict::Servers, "{domain}");
            let first = &found.candidates[0];
            assert_eq!(first.source, Source::Table);
            assert!(!first.confirm);
            assert_eq!(first.provider.as_ref().map(|p| p.name.as_str()), Some(name));
        }
    }

    #[test]
    fn providers_without_imap_say_so() {
        let unreachable = |provider: &str, reason| Verdict::Unreachable {
            provider: provider.into(),
            reason,
        };
        assert_eq!(
            verdict_for("tuta.com"),
            unreachable("Tuta", Unreachable::NoImap)
        );
        assert_eq!(
            verdict_for("hey.com"),
            unreachable("HEY", Unreachable::NoImap)
        );
        assert_eq!(
            verdict_for("proton.me"),
            unreachable("Proton Mail", Unreachable::NotYet)
        );
    }

    #[test]
    fn google_and_microsoft_domains_go_to_their_own_sign_in() {
        assert_eq!(verdict_for("gmail.com"), Verdict::Google);
        assert_eq!(verdict_for("outlook.com"), Verdict::Microsoft);
        assert_eq!(verdict_for("hotmail.co.uk"), Verdict::Microsoft);
    }

    #[test]
    fn fastmail_offers_465_then_587_with_an_app_password() {
        let found = table()
            .by_domain("fastmail.com")
            .unwrap()
            .found(Source::Table, false);
        let ports: Vec<(u16, Security)> = found
            .candidates
            .iter()
            .map(|c| (c.smtp.port, c.smtp.security))
            .collect();
        assert_eq!(ports, [(465, Security::Tls), (587, Security::StartTls)]);
        let provider = found.candidates[0].provider.clone().unwrap();
        assert_eq!(provider.password, PasswordKind::AppPassword);
        assert!(provider.app_password_url.is_some());
    }

    #[test]
    fn icloud_tries_the_local_part_first_for_imap() {
        let found = table()
            .by_domain("me.com")
            .unwrap()
            .found(Source::Table, false);
        assert_eq!(found.candidates[0].imap.user_name, UserName::LocalPartFirst);
        assert_eq!(found.candidates[0].smtp.user_name, UserName::Address);
    }

    fn by_mx(host: &str) -> Option<String> {
        table()
            .by_mx(&[host.to_string()])
            .map(|entry| entry.id.clone())
    }

    #[test]
    fn mx_hosts_name_their_provider() {
        for (host, id) in [
            ("in1-smtp.messagingengine.com", "fastmail"),
            ("mx01.mail.icloud.com", "icloud"),
            ("mta6.am0.yahoodns.net", "yahoo"),
            ("mx-aol.mail.gm0.yahoodns.net", "aol"),
            ("mx-apac.mail.gm0.yahoodns.net", "yahoo"),
            ("mx.zoho.eu", "zoho-eu"),
            ("mx2.zoho.com", "zoho-com"),
            ("mx.zoho.com.au", "zoho-com-au"),
            ("mx00.emig.gmx.net", "gmx-net"),
            ("mx01.gmx.net", "gmx-com"),
            ("mxext1.mailbox.org", "mailbox-org"),
            ("mail.protonmail.ch", "proton"),
            ("mail.tutanota.de", "tuta"),
            ("work-mx.app.hey.com", "hey"),
            ("smtp.google.com", "google"),
            ("alt2.aspmx.l.google.com", "google"),
            ("contoso-com.mail.protection.outlook.com", "microsoft"),
            ("contoso-com.b-v1.mx.microsoft", "microsoft"),
            ("MX01.Mail.iCloud.com.", "icloud"),
        ] {
            assert_eq!(by_mx(host).as_deref(), Some(id), "{host}");
        }
    }

    #[test]
    fn an_unknown_or_look_alike_mx_matches_nothing() {
        for host in [
            "mx.example.net",
            "messagingengine.com.evil.example",
            "zoho.com",
            "evilzoho.com",
        ] {
            assert_eq!(by_mx(host), None, "{host}");
        }
    }

    #[test]
    fn the_first_mx_host_any_entry_knows_decides() {
        let hosts = [
            "backup.example.net".to_string(),
            "mx01.mail.icloud.com".to_string(),
        ];
        assert_eq!(table().by_mx(&hosts).map(|e| e.id.as_str()), Some("icloud"));
    }

    #[test]
    fn a_custom_domain_on_zoho_uses_the_pro_hosts() {
        let table = table();
        let zoho = table.by_mx(&["mx.zoho.eu".to_string()]).unwrap();
        let found = zoho.found(Source::Mx, true);
        assert_eq!(found.candidates[0].imap.host, "imappro.zoho.eu");
        assert_eq!(found.candidates[0].smtp.host, "smtppro.zoho.eu");
        assert_eq!(found.candidates[0].source, Source::Mx);
        let own = zoho.found(Source::Table, false);
        assert_eq!(own.candidates[0].imap.host, "imap.zoho.eu");
    }

    #[test]
    fn the_built_in_table_is_the_file() {
        assert_eq!(Table::built_in().entries.len(), table().entries.len());
    }

    #[test]
    fn a_provider_is_found_again_by_its_name() {
        let fastmail = provider_named("Fastmail").expect("Fastmail is in the table");
        assert_eq!(fastmail.password, PasswordKind::AppPassword);
        assert!(fastmail.app_password_url.is_some());
        assert!(!fastmail.files_sent_mail);
        for name in ["Zoho Mail", "Yahoo Mail", "AOL Mail"] {
            let provider = provider_named(name).unwrap_or_else(|| panic!("{name}"));
            assert!(provider.files_sent_mail, "{name} files its own Sent copy");
        }
        for name in ["Tuta", "Proton Mail", "Google", "fastmail", "Example"] {
            assert_eq!(provider_named(name), None, "{name}");
        }
    }

    /// Zoho's data centres and GMX's two families share a name, and an
    /// account keeps only the name. Whichever entry answers must file sent
    /// mail the same way and take the same password. GMX's two families
    /// link to different help pages; the first family's pages answer for
    /// both.
    #[test]
    fn entries_that_share_a_name_agree_on_the_sent_copy_and_the_password() {
        let table = table();
        for entry in table.entries.iter().filter(|e| e.kind == Kind::Imap) {
            let first = table.by_name(&entry.name).expect("the entry itself").info();
            let own = entry.info();
            assert_eq!(first.files_sent_mail, own.files_sent_mail, "{}", entry.id);
            assert_eq!(first.password, own.password, "{}", entry.id);
        }
    }
}
