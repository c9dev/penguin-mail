//! The automatic reply dialog's save, through `AccountSettings`, against
//! mailboxes that keep what they are sent and mailboxes that do not.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_graph::{AutomaticReplies, DateTimeZone};
use serde_json::json;

use super::{Outlook, outlook};
use crate::fake::ReplyWrites;
use crate::settings::{AccountSettings, AutomaticReply, Permitted};
use crate::tests::Connected;

fn settings(h: &Outlook) -> AccountSettings<Connected> {
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    AccountSettings::new(Arc::new(Connected(connected)), h.db.clone())
}

fn utc(date_time: &str) -> DateTimeZone {
    DateTimeZone { date_time: date_time.into(), time_zone: "UTC".into() }
}

/// What a new Outlook.com mailbox answered on 2026-10-05: off, for nobody
/// outside, and a one-day schedule from the next hour that nobody set.
async fn fresh_mailbox(writes: ReplyWrites) -> Outlook {
    let h = outlook().await;
    h.fake.with(|s| {
        s.reply_writes = writes;
        s.replies = AutomaticReplies {
            status: "disabled".into(),
            external_audience: "none".into(),
            scheduled_start_date_time: Some(utc("2026-10-05T11:00:00.0000000")),
            scheduled_end_date_time: Some(utc("2026-10-06T11:00:00.0000000")),
            internal_reply_message: String::new(),
            external_reply_message: String::new(),
        };
    });
    h
}

/// The dialog's values from the live test: on, no dates, not limited to
/// contacts, over the reply it read when it opened.
async fn turned_on(h: &Outlook) -> AutomaticReply {
    let read = settings(h).automatic_reply(h.account_id).await.unwrap().done().unwrap();
    AutomaticReply {
        enabled: true,
        body: "PM test: away until Friday.".into(),
        contacts_only: false,
        first_day: None,
        last_day: None,
        ..read
    }
}

#[tokio::test]
async fn the_dialogs_reply_goes_out_as_microsoft_documents_it() {
    let h = fresh_mailbox(ReplyWrites::Kept).await;
    let wanted = turned_on(&h).await;

    let saved = settings(&h).set_automatic_reply(h.account_id, &wanted).await.unwrap();

    assert_eq!(saved, Permitted::Done(()));
    let sent = h.fake.with(|s| s.replies_sent.clone());
    assert_eq!(sent.len(), 1, "a mailbox that keeps the reply gets one write");
    assert_eq!(
        json!({"automaticRepliesSetting": sent[0]}),
        json!({"automaticRepliesSetting": {
            "status": "alwaysEnabled",
            "externalAudience": "all",
            "internalReplyMessage": "<p>PM test: away until Friday.</p>",
            "externalReplyMessage": "<p>PM test: away until Friday.</p>",
        }}),
        "the dialog says the reply goes outside the organization too"
    );
}

#[tokio::test]
async fn an_outlook_com_mailbox_keeps_a_reply_with_no_dates() {
    let h = fresh_mailbox(ReplyWrites::Personal).await;
    let wanted = turned_on(&h).await;

    let saved = settings(&h).set_automatic_reply(h.account_id, &wanted).await.unwrap();

    assert_eq!(saved, Permitted::Done(()));
    let held = h.fake.with(|s| s.replies.clone());
    assert_eq!(held.status, "scheduled");
    assert!(held.internal_reply_message.contains("PM test: away until Friday."));
    let read = settings(&h).automatic_reply(h.account_id).await.unwrap().done().unwrap();
    assert_eq!(
        (read.enabled, read.first_day, read.last_day, read.body.as_str()),
        (true, None, None, "PM test: away until Friday."),
        "the dialog opens on, with no dates"
    );
}

#[tokio::test]
async fn an_outlook_com_reply_turns_off_again() {
    let h = fresh_mailbox(ReplyWrites::Personal).await;
    let on = turned_on(&h).await;
    settings(&h).set_automatic_reply(h.account_id, &on).await.unwrap();

    let off = AutomaticReply { enabled: false, ..on };
    let saved = settings(&h).set_automatic_reply(h.account_id, &off).await.unwrap();

    assert_eq!(saved, Permitted::Done(()));
    assert_eq!(h.fake.with(|s| s.replies.status.clone()), "disabled");
}

#[tokio::test]
async fn a_reply_the_mailbox_does_not_keep_is_not_reported_saved() {
    let h = fresh_mailbox(ReplyWrites::Ignored).await;
    let wanted = turned_on(&h).await;

    let saved = settings(&h).set_automatic_reply(h.account_id, &wanted).await;

    assert!(saved.is_err(), "{saved:?}");
}
