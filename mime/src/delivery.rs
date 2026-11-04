//! Transport recipients are hints, not permission to send as an address.

use mail_parser::HeaderForm;

/// Explicit delivery headers take precedence over Received hops. Each kind
/// keeps wire order, so the receiving server's newest hop comes first.
pub fn addresses(headers: &[(String, String)]) -> Vec<String> {
    let mut found = Vec::new();
    for name in ["Delivered-To", "X-Original-To", "Received"] {
        for (_, value) in headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
        {
            let address = if name == "Received" {
                let parsed = HeaderForm::Received.parse(value.as_bytes());
                parsed
                    .value()
                    .as_received()
                    .and_then(|hop| hop.for_())
                    .map(str::to_string)
            } else {
                let parsed = HeaderForm::Addresses.parse(value.as_bytes());
                parsed.value().as_address().and_then(|list| {
                    let mut mailboxes = list.mailboxes();
                    let address = mailboxes.next()?.address()?;
                    mailboxes.next().is_none().then(|| address.to_string())
                })
            };
            if let Some(address) = address
                && address.contains('@')
                && !address
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '<' | '>'))
                && !found
                    .iter()
                    .any(|a: &String| a.eq_ignore_ascii_case(&address))
            {
                found.push(address);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folded_received_fields_keep_the_newest_delivery_first() {
        let body = crate::read(
            b"From: list@example.net\r\nTo: list@example.net\r\n\
Received: from sender.example.net ([192.0.2.5]) by mail.example.com\r\n\
\tvia tcp with ESMTPS id abc (TLS1.2) for <ann+news@example.com>;\r\n\
\t3 Nov 2026 14:20:28 +0000\r\n\
Received: from older.example.net by sender.example.net for old@example.net;\r\n\
\t3 Nov 2026 14:19:28 +0000\r\n\r\nNews",
        );
        assert_eq!(
            body.provenance.delivery_addresses(),
            ["ann+news@example.com", "old@example.net"]
        );
    }

    #[test]
    fn explicit_headers_precede_hops_and_duplicates_ignore_case() {
        let headers = [
            ("Received", "from sender.example by mx.example for <other@example.com>; Tue, 3 Nov 2026 14:20:28 +0000"),
            ("X-Original-To", "alias@example.com"),
            ("delivered-to", "Ann@example.com"),
            ("Delivered-To", "ann@EXAMPLE.COM"),
        ].map(|(k, v)| (k.into(), v.into()));
        assert_eq!(
            addresses(&headers),
            ["Ann@example.com", "alias@example.com", "other@example.com"]
        );
    }

    #[test]
    fn comments_and_unrelated_headers_do_not_supply_a_recipient() {
        for (key, value) in [
            (
                "Received",
                "from sender.example (for <decoy@example.com>) by mx.example; Tue, 3 Nov 2026 14:20:28 +0000",
            ),
            ("Received", "from before.example by mx.example"),
            ("Received", "garbage"),
            ("Delivered-To", "not an address"),
            ("Delivered-To", "a@example.com, b@example.com"),
            ("Return-Path", "<bounce@example.com>"),
        ] {
            assert!(
                addresses(&[(key.into(), value.into())]).is_empty(),
                "{key}: {value}"
            );
        }
        assert_eq!(
            crate::read(b"From: a@example.com\r\n\r\nHello")
                .provenance
                .delivered_to,
            Some(vec![])
        );
    }
}
