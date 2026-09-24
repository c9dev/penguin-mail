//! Mozilla's autoconfig format, `config-v1.1.xml`, as a domain, the ISPDB
//! and a mail host's own domain serve it.

use roxmltree::{Document, Node, ParsingOptions};

use crate::name::{host, is_within};
use crate::{Candidate, PasswordKind, ProviderInfo, Security, Server, Source, UserName, pairs};

/// A config file is a few kilobytes. A bigger document is not one, and
/// parsing it would only cost memory.
const MOST_NODES: u32 = 10_000;

/// What one autoconfig file offers, with every server the app cannot use
/// left out.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Config {
    pub(crate) name: Option<String>,
    pub(crate) imap: Vec<Server>,
    pub(crate) smtp: Vec<Server>,
    pub(crate) enable_imap_url: Option<String>,
    pub(crate) documentation_url: Option<String>,
}

/// Reads `xml` for an address at `domain`. `None` when it is not an
/// autoconfig file or offers no IMAP or no SMTP server the app can use.
pub(crate) fn parse(xml: &str, domain: &str) -> Option<Config> {
    let options = ParsingOptions {
        nodes_limit: MOST_NODES,
        ..ParsingOptions::default()
    };
    let document = Document::parse_with_options(xml, options).ok()?;
    let root = document.root_element();
    if !root.has_tag_name("clientConfig") {
        return None;
    }
    let provider = root.children().find(|n| n.has_tag_name("emailProvider"))?;
    let servers = |tag: &str, kind: &str| -> Vec<Server> {
        provider
            .children()
            .filter(|n| n.has_tag_name(tag) && n.attribute("type") == Some(kind))
            .filter_map(|n| server(n, domain))
            .collect()
    };
    let config = Config {
        // The short name reads better in "Fastmail needs an app password";
        // mailbox.org's long one carries a slogan.
        name: text_of(provider, "displayShortName").or_else(|| text_of(provider, "displayName")),
        imap: servers("incomingServer", "imap"),
        smtp: servers("outgoingServer", "smtp"),
        enable_imap_url: provider
            .children()
            .filter(|n| n.has_tag_name("enable"))
            .find_map(|n| link(n.attribute("visiturl"))),
        documentation_url: ["documentation", "instruction"]
            .into_iter()
            .find_map(|tag| {
                provider
                    .children()
                    .filter(|n| n.has_tag_name(tag))
                    .find_map(|n| link(n.attribute("url")))
            }),
    };
    (!config.imap.is_empty() && !config.smtp.is_empty()).then_some(config)
}

impl Config {
    /// Every IMAP server with every SMTP server, in the file's order, which
    /// is the provider's preference. A file found through the MX hosts
    /// rests on unsigned DNS, so a candidate from one that names a server
    /// outside `domain` must be confirmed, as an SRV target outside it is.
    pub(crate) fn candidates(&self, source: Source, domain: &str) -> Vec<Candidate> {
        let provider = ProviderInfo {
            name: self.name.clone().unwrap_or_else(|| domain.to_string()),
            password: PasswordKind::AccountPassword,
            app_password_url: None,
            enable_imap_url: self.enable_imap_url.clone(),
            documentation_url: self.documentation_url.clone(),
            files_sent_mail: false,
        };
        let mut candidates = pairs(source, Some(&provider), &self.imap, &self.smtp, false);
        if source == Source::MxAutoconfig {
            for candidate in &mut candidates {
                candidate.confirm = !is_within(&candidate.imap.host, domain)
                    || !is_within(&candidate.smtp.host, domain);
            }
        }
        candidates
    }
}

/// One server entry, or `None` when the app cannot use it: a plain-text
/// socket, a host that is no host name, or only ways of signing in the app
/// does not have.
fn server(node: Node, domain: &str) -> Option<Server> {
    let hostname = text_of(node, "hostname")?.replace("%EMAILDOMAIN%", domain);
    let port: u16 = text_of(node, "port")?.parse().ok().filter(|p| *p != 0)?;
    let security = match text_of(node, "socketType")?.as_str() {
        "SSL" => Security::Tls,
        "STARTTLS" => Security::StartTls,
        _ => return None,
    };
    let user_name = match text_of(node, "username").as_deref() {
        Some("%EMAILLOCALPART%") => UserName::LocalPartFirst,
        _ => UserName::Address,
    };
    signs_in_with_password(node).then_some(())?;
    Some(Server {
        host: host(&hostname)?,
        port,
        security,
        user_name,
    })
}

/// Whether the app can sign in to this server with a password. The app
/// holds no OAuth client for any provider, so `OAuth2` never counts; a
/// server that names only `OAuth2`, as Fastmail's own file does, still
/// takes an app password. A server that names only methods such as
/// Kerberos or NTLM does not.
fn signs_in_with_password(node: Node) -> bool {
    let methods: Vec<String> = node
        .children()
        .filter(|n| n.has_tag_name("authentication"))
        .filter_map(|n| n.text().map(|t| t.trim().to_string()))
        .filter(|method| method != "OAuth2")
        .collect();
    methods.is_empty()
        || methods
            .iter()
            .any(|m| m == "password-cleartext" || m == "password-encrypted")
}

fn text_of(node: Node, tag: &str) -> Option<String> {
    node.children()
        .find(|n| n.has_tag_name(tag))
        .and_then(|n| n.text())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// A link to show the person, kept only when it is HTTPS.
fn link(url: Option<&str>) -> Option<String> {
    url.map(str::trim)
        .filter(|url| url.starts_with("https://"))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(host: &str, port: u16, security: Security) -> Server {
        Server {
            host: host.into(),
            port,
            security,
            user_name: UserName::Address,
        }
    }

    #[test]
    fn fastmail_offers_only_oauth_and_gets_a_password_instead() {
        let config = parse(
            include_str!("../tests/fixtures/fastmail.com.xml"),
            "fastmail.com",
        )
        .expect("a config");
        assert_eq!(config.name.as_deref(), Some("Fastmail"));
        assert_eq!(
            config.imap,
            [server("imap.fastmail.com", 993, Security::Tls)]
        );
        assert_eq!(
            config.smtp,
            [server("smtp.fastmail.com", 465, Security::Tls)]
        );
        assert_eq!(
            config.documentation_url.as_deref(),
            Some("https://www.fastmail.help/hc/en-us/articles/1500000278342")
        );
    }

    #[test]
    fn mailbox_org_keeps_tls_and_starttls_and_drops_its_plain_http_link() {
        let config = parse(
            include_str!("../tests/fixtures/mailbox.org.xml"),
            "mailbox.org",
        )
        .expect("a config");
        assert_eq!(
            config.imap,
            [
                server("imap.mailbox.org", 993, Security::Tls),
                server("imap.mailbox.org", 143, Security::StartTls),
            ]
        );
        assert_eq!(
            config.smtp,
            [
                server("smtp.mailbox.org", 465, Security::Tls),
                server("smtp.mailbox.org", 587, Security::StartTls),
            ]
        );
        assert_eq!(config.documentation_url, None);
    }

    #[test]
    fn posteo_leaves_pop3_out() {
        let config =
            parse(include_str!("../tests/fixtures/posteo.de.xml"), "posteo.de").expect("a config");
        assert_eq!(config.imap, [server("posteo.de", 993, Security::Tls)]);
        assert_eq!(config.smtp, [server("posteo.de", 465, Security::Tls)]);
    }

    #[test]
    fn yandex_keeps_its_password_method_and_the_link_that_turns_imap_on() {
        let config = parse(
            include_str!("../tests/fixtures/yandex.com.xml"),
            "yandex.com",
        )
        .expect("a config");
        assert_eq!(config.imap, [server("imap.yandex.com", 993, Security::Tls)]);
        assert_eq!(
            config.enable_imap_url.as_deref(),
            Some("https://mail.yandex.ru/neo2/#setup/client")
        );
    }

    #[test]
    fn gmx_carries_its_enable_link_and_both_submission_ports() {
        let config =
            parse(include_str!("../tests/fixtures/gmx.net.xml"), "gmx.net").expect("a config");
        assert_eq!(
            config.enable_imap_url.as_deref(),
            Some("https://hilfe.gmx.net/pop-imap/einschalten.html")
        );
        assert_eq!(config.smtp.len(), 2);
    }

    #[test]
    fn a_hoster_file_fills_in_the_domain_and_the_local_part() {
        let config =
            parse(include_str!("../tests/fixtures/hoster.xml"), "example.org").expect("a config");
        assert_eq!(config.imap[0].host, "mail.example.org");
        assert_eq!(config.imap[0].user_name, UserName::LocalPartFirst);
    }

    #[test]
    fn plain_text_servers_are_unusable() {
        assert_eq!(
            parse(include_str!("../tests/fixtures/plain.xml"), "example.org"),
            None
        );
    }

    #[test]
    fn a_server_on_an_ip_address_is_unusable() {
        assert_eq!(
            parse(include_str!("../tests/fixtures/proton.me.xml"), "proton.me"),
            None
        );
    }

    #[test]
    fn a_home_page_is_not_a_config() {
        assert_eq!(
            parse(
                include_str!("../tests/fixtures/home-page.html"),
                "example.org"
            ),
            None
        );
        assert_eq!(parse("", "example.org"), None);
        assert_eq!(parse("<clientConfig/>", "example.org"), None);
    }

    #[test]
    fn a_document_type_declaration_is_refused() {
        let xml = r#"<?xml version="1.0"?>
<!DOCTYPE clientConfig [<!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;&a;&a;&a;">]>
<clientConfig version="1.1"><emailProvider id="x"><displayName>&b;</displayName></emailProvider></clientConfig>"#;
        assert_eq!(parse(xml, "example.org"), None);
    }

    #[test]
    fn kerberos_alone_is_unusable_and_oauth_beside_a_password_is_dropped() {
        let xml = |methods: &str| {
            format!(
                r#"<clientConfig version="1.1"><emailProvider id="x">
<incomingServer type="imap"><hostname>imap.example.org</hostname><port>993</port>
<socketType>SSL</socketType>{methods}</incomingServer>
<outgoingServer type="smtp"><hostname>smtp.example.org</hostname><port>465</port>
<socketType>SSL</socketType></outgoingServer></emailProvider></clientConfig>"#
            )
        };
        assert_eq!(
            parse(
                &xml("<authentication>GSSAPI</authentication>"),
                "example.org"
            ),
            None
        );
        let both = xml(
            "<authentication>OAuth2</authentication><authentication>password-cleartext</authentication>",
        );
        assert!(parse(&both, "example.org").is_some());
    }

    #[test]
    fn candidates_pair_every_imap_server_with_every_smtp_server() {
        let config = parse(
            include_str!("../tests/fixtures/mailbox.org.xml"),
            "mailbox.org",
        )
        .expect("a config");
        let candidates = config.candidates(Source::Autoconfig, "mailbox.org");
        let ports: Vec<(u16, u16)> = candidates
            .iter()
            .map(|c| (c.imap.port, c.smtp.port))
            .collect();
        assert_eq!(ports, [(993, 465), (993, 587), (143, 465), (143, 587)]);
        let provider = candidates[0].provider.clone().expect("a provider");
        assert_eq!(provider.name, "mailbox.org");
        assert_eq!(provider.password, PasswordKind::AccountPassword);
        assert!(!provider.files_sent_mail);
        assert!(
            candidates
                .iter()
                .all(|c| c.source == Source::Autoconfig && !c.confirm)
        );
    }
}
