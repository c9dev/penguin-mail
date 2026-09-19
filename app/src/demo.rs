//! Sample mail for `mailrs --demo`: three accounts and a few weeks of
//! conversations, written straight into a throwaway store. Every address
//! uses a reserved `.example` domain.

use mailrs_domain::{AccountState, Address, Attachment, EpochMillis, Label, LabelKind, MessageBody, MessageMeta};
use mailrs_store::{Result, accounts, bodies, labels, messages};
use rusqlite::Connection;

pub const ACCOUNTS: [&str; 3] = ["dana.reyes@example.com", "dana@fernwood.example", "d.reyes@uni.example"];
pub const DISPLAY_NAME: &str = "Dana Reyes";

struct Sample {
    account: usize,
    thread: &'static str,
    id: &'static str,
    from: (&'static str, &'static str),
    to: &'static [(&'static str, &'static str)],
    subject: &'static str,
    minutes_ago: i64,
    labels: &'static [&'static str],
    text: &'static str,
    html: Option<&'static str>,
    attachments: &'static [(&'static str, &'static str, i64)],
}

const ME: (&str, &str) = ("", "");

const HOUR: i64 = 60;
const DAY: i64 = 24 * HOUR;

fn samples() -> Vec<Sample> {
    vec![
        Sample {
            account: 1, thread: "t-roadmap", id: "roadmap-1",
            from: ("Priya Raman", "priya@fernwood.example"), to: &[ME, ("Jonas Weber", "jonas@fernwood.example")],
            subject: "Q4 roadmap review", minutes_ago: 26 * HOUR, labels: &["INBOX"],
            text: "Hi both,\n\nI've put the Q4 roadmap draft in the shared folder. The big open question is whether the offline editor ships in October or slips to November.\n\nCould you each leave comments by Thursday? I'd like to walk the leadership team through it on Friday.\n\nThanks,\nPriya",
            html: None, attachments: &[("q4-roadmap.pdf", "application/pdf", 1_842_000)],
        },
        Sample {
            account: 1, thread: "t-roadmap", id: "roadmap-2",
            from: ("Jonas Weber", "jonas@fernwood.example"), to: &[("Priya Raman", "priya@fernwood.example"), ME],
            subject: "Re: Q4 roadmap review", minutes_ago: 20 * HOUR, labels: &["INBOX"],
            text: "Left my comments. Short version: October is possible if we cut sync conflict resolution down to last-write-wins for the first release.\n\n> Could you each leave comments by Thursday?\n\nJonas",
            html: None, attachments: &[],
        },
        Sample {
            account: 1, thread: "t-roadmap", id: "roadmap-3",
            from: ("Priya Raman", "priya@fernwood.example"), to: &[ME, ("Jonas Weber", "jonas@fernwood.example")],
            subject: "Re: Q4 roadmap review", minutes_ago: 38, labels: &["INBOX", "UNREAD", "IMPORTANT"],
            text: "Dana, can you sanity-check Jonas's estimate before Friday? If last-write-wins is acceptable to support, I'm happy to commit to October.\n\nOn Tue, Jonas Weber wrote:\n> October is possible if we cut sync conflict resolution down to\n> last-write-wins for the first release.\n\n-- \nPriya Raman\nHead of Product, Fernwood",
            html: None, attachments: &[],
        },
        Sample {
            account: 0, thread: "t-hike", id: "hike-1",
            from: ("Mara Okafor", "mara.okafor@example.org"), to: &[ME],
            subject: "Saturday hike?", minutes_ago: 3 * HOUR, labels: &["INBOX"],
            text: "Weather looks perfect for Saturday. I was thinking the ridge loop from the north trailhead, about 14 km. Leave at 8, back by 3?\n\nTheo might join if he can get the car.",
            html: None, attachments: &[],
        },
        Sample {
            account: 0, thread: "t-hike", id: "hike-2",
            from: (DISPLAY_NAME, "dana.reyes@example.com"), to: &[("Mara Okafor", "mara.okafor@example.org")],
            subject: "Re: Saturday hike?", minutes_ago: 2 * HOUR + 40, labels: &["SENT"],
            text: "Yes! Count me in. I'll bring lunch for three just in case.\n\n> Leave at 8, back by 3?\n\nPerfect.",
            html: None, attachments: &[],
        },
        Sample {
            account: 0, thread: "t-hike", id: "hike-3",
            from: ("Mara Okafor", "mara.okafor@example.org"), to: &[ME],
            subject: "Re: Saturday hike?", minutes_ago: 12, labels: &["INBOX", "UNREAD"],
            text: "Theo's in. Meet at mine at 7:45 and we'll drive up together. Bring layers, it'll be cold at the top.",
            html: None, attachments: &[],
        },
        Sample {
            account: 2, thread: "t-thesis", id: "thesis-1",
            from: ("Prof. Kemi Adeyemi", "k.adeyemi@uni.example"), to: &[ME],
            subject: "Thesis chapter 3 feedback", minutes_ago: 95, labels: &["INBOX", "UNREAD"],
            text: "Dear Dana,\n\nI've read chapter 3. The methodology section is much stronger than the last draft. Two things before you move on:\n\n1. The sampling rationale in 3.2 needs a sentence on why you excluded the pilot cohort.\n2. Figure 3.4 is doing a lot of work; consider splitting it into two panels.\n\nHappy to talk it through on Wednesday at 2pm if that suits.\n\nBest,\nKemi",
            html: None, attachments: &[],
        },
        Sample {
            account: 0, thread: "t-bank", id: "bank-1",
            from: ("Juniper Bank", "statements@juniper.example"), to: &[ME],
            subject: "Your September statement is ready", minutes_ago: 5 * HOUR, labels: &["INBOX"],
            text: "Your September statement is ready to view.",
            html: Some(r#"<table width="100%" cellpadding="0" cellspacing="0" style="font-family:Helvetica,Arial,sans-serif;background:#f4f1ec"><tr><td align="center" style="padding:28px 12px"><table width="560" cellpadding="0" cellspacing="0" style="background:#ffffff;border-radius:14px"><tr><td style="padding:26px 32px 8px;font-size:13px;letter-spacing:.12em;color:#2f6b4f;font-weight:bold">JUNIPER BANK</td></tr><tr><td style="padding:4px 32px 0;font-size:24px;font-weight:bold;color:#1d1d1f">Your September statement is ready</td></tr><tr><td style="padding:14px 32px;font-size:15px;line-height:1.55;color:#444">Hi Dana, your statement for the account ending 4821 is now available in online banking.</td></tr><tr><td style="padding:6px 32px 20px"><table width="100%" style="font-size:14px;color:#1d1d1f;border-top:1px solid #eee"><tr><td style="padding:10px 0">Opening balance</td><td align="right">$3,412.08</td></tr><tr><td style="padding:10px 0;border-top:1px solid #eee">Money in</td><td align="right" style="border-top:1px solid #eee;color:#2f6b4f">+$4,950.00</td></tr><tr><td style="padding:10px 0;border-top:1px solid #eee">Money out</td><td align="right" style="border-top:1px solid #eee">−$3,877.41</td></tr><tr><td style="padding:10px 0;border-top:1px solid #eee;font-weight:bold">Closing balance</td><td align="right" style="border-top:1px solid #eee;font-weight:bold">$4,484.67</td></tr></table></td></tr><tr><td style="padding:0 32px 30px"><a href="https://juniper.example/statements" style="display:inline-block;background:#2f6b4f;color:#fff;text-decoration:none;padding:12px 22px;border-radius:999px;font-weight:bold;font-size:14px">View statement</a></td></tr></table><p style="font-size:12px;color:#8a8a8a;margin:18px 0 0">Juniper Bank will never ask for your password by email.</p></td></tr></table>"#),
            attachments: &[],
        },
        Sample {
            account: 0, thread: "t-lake", id: "lake-1",
            from: ("Theo Lindqvist", "theo@example.net"), to: &[ME, ("Mara Okafor", "mara.okafor@example.org")],
            subject: "Photos from the lake", minutes_ago: DAY + 3 * HOUR, labels: &["INBOX", "STARRED"],
            text: "Finally got these off the camera. The one of the dock at sunrise might be the best photo I've taken all year.\n\nFull album: https://photos.example.net/lake-2026",
            html: None, attachments: &[("dock-sunrise.jpg", "image/jpeg", 2_480_000), ("ridge.jpg", "image/jpeg", 3_120_000)],
        },
        Sample {
            account: 1, thread: "t-crit", id: "crit-1",
            from: ("Jonas Weber", "jonas@fernwood.example"), to: &[ME],
            subject: "Design crit notes", minutes_ago: DAY + 6 * HOUR, labels: &["INBOX"],
            text: "Notes from today's crit:\n\n- Onboarding: people missed the skip link. Make it a real button.\n- Settings: group the sync options under one heading.\n- Empty states: everyone loved the illustrations. Keep them.\n\nI'll turn these into tickets tomorrow.",
            html: None, attachments: &[],
        },
        Sample {
            account: 0, thread: "t-parcel", id: "parcel-1",
            from: ("Packet Post", "tracking@packetpost.example"), to: &[ME],
            subject: "Your parcel is out for delivery", minutes_ago: 2 * DAY + 2 * HOUR, labels: &["INBOX"],
            text: "Your parcel is out for delivery today between 10:00 and 14:00.",
            html: Some(r#"<div style="font-family:Arial,sans-serif;max-width:520px;margin:0 auto;padding:24px;color:#222"><div style="font-size:20px;font-weight:bold;color:#d9480f">Packet Post</div><h2 style="margin:18px 0 6px;font-size:22px">Arriving today</h2><p style="font-size:15px;color:#444;margin:0 0 18px">Your parcel from Linden Books is out for delivery between <b>10:00 and 14:00</b>.</p><div style="background:#fff4e6;border-radius:10px;padding:14px 16px;font-size:14px">Tracking number <b>PP 4417 2290 118</b></div><p style="font-size:12px;color:#888;margin-top:22px">You're receiving this because you placed an order with a Packet Post partner.</p></div>"#),
            attachments: &[],
        },
        Sample {
            account: 1, thread: "t-invite", id: "invite-1",
            from: ("Ines Duarte", "ines@fernwood.example"), to: &[ME],
            subject: "Contract draft v3", minutes_ago: 3 * DAY + 4 * HOUR, labels: &["INBOX", "STARRED", "Label_clients"],
            text: "Hi Dana,\n\nAttached is v3 with the changes from legal. The only substantive edit is the payment schedule in section 4, now net 30 instead of net 45.\n\nIf you're happy, I'll send it to Maple & Finch for signature on Monday.\n\nInês",
            html: None, attachments: &[("fernwood-maple-finch-v3.docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document", 86_400)],
        },
        Sample {
            account: 0, thread: "t-recipe", id: "recipe-1",
            from: ("Lucia Reyes", "lucia.reyes@example.com"), to: &[ME],
            subject: "The recipe you asked for", minutes_ago: 4 * DAY + 5 * HOUR, labels: &["INBOX"],
            text: "Here it is, exactly how your grandmother wrote it down:\n\nArroz con pollo\n- 1 whole chicken, in pieces\n- 2 cups rice\n- 1 onion, 1 pepper, 3 cloves garlic\n- a pinch of saffron (don't skip it)\n\nBrown the chicken first. Be patient with it.\n\nCall me on Sunday!\nMamá",
            html: None, attachments: &[],
        },
        Sample {
            account: 2, thread: "t-library", id: "library-1",
            from: ("University Library", "library@uni.example"), to: &[ME],
            subject: "Two books are due next week", minutes_ago: 6 * DAY + 2 * HOUR, labels: &["INBOX"],
            text: "Hello Dana,\n\nThese items are due on 24 September:\n\n- Research Design in Practice\n- Visualizing Data, 2nd ed.\n\nRenew online at https://library.uni.example/account\n\nUniversity Library",
            html: None, attachments: &[],
        },
        Sample {
            account: 1, thread: "t-offsite", id: "offsite-1",
            from: ("Priya Raman", "priya@fernwood.example"), to: &[ME],
            subject: "Offsite logistics", minutes_ago: 12 * DAY, labels: &["Label_travel"],
            text: "Train tickets are booked for everyone. Hotel confirmation to follow.",
            html: None, attachments: &[],
        },
        Sample {
            account: 0, thread: "t-concert", id: "concert-1",
            from: ("Hollow Pines Hall", "tickets@hollowpines.example"), to: &[ME],
            subject: "Your tickets for The Night Ferries", minutes_ago: 19 * DAY, labels: &["INBOX"],
            text: "Doors open at 19:30. Show this email at the entrance.",
            html: None, attachments: &[("tickets.pdf", "application/pdf", 214_000)],
        },
        Sample {
            account: 1, thread: "t-draft", id: "draft-1",
            from: (DISPLAY_NAME, "dana@fernwood.example"), to: &[("Priya Raman", "priya@fernwood.example")],
            subject: "Estimate check", minutes_ago: 30, labels: &["DRAFT"],
            text: "Priya,\n\nI went through Jonas's numbers. October holds **if** we",
            html: None, attachments: &[],
        },
    ]
}

/// Fills an empty store with the demo accounts and mail.
pub fn seed(conn: &Connection, now: EpochMillis) -> Result<()> {
    let mut account_ids = Vec::new();
    for email in ACCOUNTS {
        let id = accounts::insert_account(conn, email, now)?;
        accounts::start_generation(conn, id, 1)?;
        accounts::set_backfill(conn, id, None, true)?;
        accounts::set_state(conn, id, AccountState::Ok)?;
        let mut account_labels: Vec<Label> = ["INBOX", "SENT", "DRAFT", "STARRED", "UNREAD", "IMPORTANT"]
            .into_iter()
            .map(|l| Label { account_id: id, id: l.into(), name: l.into(), kind: LabelKind::System })
            .collect();
        if email == ACCOUNTS[1] {
            for (label, name) in [("Label_clients", "Clients"), ("Label_travel", "Travel")] {
                account_labels.push(Label { account_id: id, id: label.into(), name: name.into(), kind: LabelKind::User });
            }
        }
        labels::replace_labels(conn, id, &account_labels)?;
        account_ids.push(id);
    }
    for sample in samples() {
        let account_id = account_ids[sample.account];
        let me = Address { name: Some(DISPLAY_NAME.into()), email: ACCOUNTS[sample.account].into() };
        let address = |(name, email): (&str, &str)| {
            if email.is_empty() {
                me.clone()
            } else {
                Address { name: (!name.is_empty()).then(|| name.to_string()), email: email.into() }
            }
        };
        let meta = MessageMeta {
            account_id,
            id: sample.id.into(),
            thread_id: sample.thread.into(),
            rfc822_msgid: Some(format!("<{}@demo.example>", sample.id)),
            from: Some(address(sample.from)),
            to: sample.to.iter().map(|&a| address(a)).collect(),
            cc: vec![],
            subject: sample.subject.into(),
            date: now - sample.minutes_ago * 60_000,
            snippet: sample.text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(140).collect(),
            size: sample.text.len() as i64,
            has_attachments: !sample.attachments.is_empty(),
            label_ids: sample.labels.iter().map(|l| l.to_string()).collect(),
        };
        messages::upsert_message(conn, &meta, 2)?;
        messages::refresh_thread(conn, account_id, sample.thread)?;
        let body = MessageBody {
            text: Some(sample.text.into()),
            html: sample.html.map(str::to_string),
            attachments: sample
                .attachments
                .iter()
                .enumerate()
                .map(|(i, &(filename, mime_type, size))| Attachment {
                    part_id: (i + 1).to_string(),
                    filename: filename.into(),
                    mime_type: mime_type.into(),
                    size,
                    attachment_id: Some(format!("{}-att-{i}", sample.id)),
                    content_id: None,
                })
                .collect(),
        };
        bodies::put_body(conn, account_id, sample.id, &body, now)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use mailrs_store::threads::{self, ThreadFilter};
    use mailrs_store::{bodies, open_in_memory};

    use super::*;

    #[test]
    fn the_demo_store_has_a_lively_unified_inbox() {
        let conn = open_in_memory().unwrap();
        seed(&conn, 1_758_000_000_000).unwrap();
        let inbox = ThreadFilter::unified("INBOX");
        let threads = threads::list_threads(&conn, &inbox, 0, 100).unwrap();
        assert!(threads.len() >= 10, "{}", threads.len());
        assert!(threads::unread_threads(&conn, &inbox).unwrap() >= 3);
        assert_eq!(threads[0].id, "t-hike");
        let accounts_seen: std::collections::HashSet<_> = threads.iter().map(|t| t.account_id).collect();
        assert_eq!(accounts_seen.len(), 3);
        assert_eq!(threads::list_threads(&conn, &ThreadFilter::unified("DRAFT"), 0, 10).unwrap().len(), 1);
        for sample in samples() {
            let account = accounts::account_by_email(&conn, ACCOUNTS[sample.account]).unwrap().unwrap();
            assert!(bodies::get_body(&conn, account.id, sample.id, 0).unwrap().is_some(), "{}", sample.id);
        }
    }
}
