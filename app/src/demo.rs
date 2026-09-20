//! Sample mail for `penguin-mail --demo`: three accounts and a few weeks of
//! conversations. Every address uses a reserved `.example` domain.
//!
//! Each account gets a `FakeGmail` holding this mail, and the same mail goes
//! straight into a throwaway store so the demo opens on a full inbox instead
//! of syncing one. From there the demo runs the same code as a real account.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::{
    AccountId, AccountState, Address, Attachment, EpochMillis, Label, LabelKind, MessageBody,
    MessageMeta, system_label,
};
use mailrs_gmail::{LabelColor, RemoteLabel, SendAs};
use mailrs_store::{Result, accounts, address_book, bodies, invitations, labels, messages};
use mailrs_sync::fake::FakeGmail;
use rusqlite::Connection;

/// The history cursor the demo starts on, in both the store and the fake, so
/// the first sync finds nothing to replay.
const HISTORY_ID: u64 = 1;

/// The id of the draft behind the sample draft message, as Gmail would hold it.
const DRAFT_ID: &str = "demo-draft";

/// The event behind the sample invitation, as Google would write it.
const INVITE_UID: &str = "7f3k2q9demo1invite@google.com";

/// The event the sample update moves. The demo remembers an older version
/// of it, so opening the update says what changed.
const MOVED_UID: &str = "2b8h5x0demo2moved@google.com";

pub const ACCOUNTS: [&str; 3] = [
    "dana.reyes@example.com",
    "dana@fernwood.example",
    "d.reyes@uni.example",
];
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
            account: 1,
            thread: "t-roadmap",
            id: "roadmap-1",
            from: ("Priya Raman", "priya@fernwood.example"),
            to: &[ME, ("Jonas Weber", "jonas@fernwood.example")],
            subject: "Q4 roadmap review",
            minutes_ago: 26 * HOUR,
            labels: &["INBOX"],
            text: "Hi both,\n\nI've put the Q4 roadmap draft in the shared folder. The big open question is whether the offline editor ships in October or slips to November.\n\nCould you each leave comments by Thursday? I'd like to walk the leadership team through it on Friday.\n\nThanks,\nPriya",
            html: None,
            attachments: &[("q4-roadmap.pdf", "application/pdf", 1_842_000)],
        },
        Sample {
            account: 1,
            thread: "t-roadmap",
            id: "roadmap-2",
            from: ("Jonas Weber", "jonas@fernwood.example"),
            to: &[("Priya Raman", "priya@fernwood.example"), ME],
            subject: "Re: Q4 roadmap review",
            minutes_ago: 20 * HOUR,
            labels: &["INBOX"],
            text: "Left my comments. Short version: October is possible if we cut sync conflict resolution down to last-write-wins for the first release.\n\n> Could you each leave comments by Thursday?\n\nJonas",
            html: None,
            attachments: &[],
        },
        Sample {
            account: 1,
            thread: "t-roadmap",
            id: "roadmap-3",
            from: ("Priya Raman", "priya@fernwood.example"),
            to: &[ME, ("Jonas Weber", "jonas@fernwood.example")],
            subject: "Re: Q4 roadmap review",
            minutes_ago: 38,
            labels: &["INBOX", "UNREAD", "IMPORTANT"],
            text: "Dana, can you sanity-check Jonas's estimate before Friday? If last-write-wins is acceptable to support, I'm happy to commit to October.\n\nOn Tue, Jonas Weber wrote:\n> October is possible if we cut sync conflict resolution down to\n> last-write-wins for the first release.\n\n-- \nPriya Raman\nHead of Product, Fernwood",
            html: None,
            attachments: &[],
        },
        Sample {
            account: 0,
            thread: "t-hike",
            id: "hike-1",
            from: ("Mara Okafor", "mara.okafor@example.org"),
            to: &[ME],
            subject: "Saturday hike?",
            minutes_ago: 3 * HOUR,
            labels: &["INBOX"],
            text: "Weather looks perfect for Saturday. I was thinking the ridge loop from the north trailhead, about 14 km. Leave at 8, back by 3?\n\nTheo might join if he can get the car.",
            html: None,
            attachments: &[],
        },
        Sample {
            account: 0,
            thread: "t-hike",
            id: "hike-2",
            from: (DISPLAY_NAME, "dana.reyes@example.com"),
            to: &[("Mara Okafor", "mara.okafor@example.org")],
            subject: "Re: Saturday hike?",
            minutes_ago: 2 * HOUR + 40,
            labels: &["SENT"],
            text: "Yes! Count me in. I'll bring lunch for three just in case.\n\n> Leave at 8, back by 3?\n\nPerfect.",
            html: None,
            attachments: &[],
        },
        Sample {
            account: 0,
            thread: "t-hike",
            id: "hike-3",
            from: ("Mara Okafor", "mara.okafor@example.org"),
            to: &[ME],
            subject: "Re: Saturday hike?",
            minutes_ago: 12,
            labels: &["INBOX", "UNREAD"],
            text: "Theo's in. Meet at mine at 7:45 and we'll drive up together. Bring layers, it'll be cold at the top.",
            html: None,
            attachments: &[],
        },
        Sample {
            account: 2,
            thread: "t-thesis",
            id: "thesis-1",
            from: ("Prof. Kemi Adeyemi", "k.adeyemi@uni.example"),
            to: &[ME],
            subject: "Thesis chapter 3 feedback",
            minutes_ago: 95,
            labels: &["INBOX", "UNREAD"],
            text: "Dear Dana,\n\nI've read chapter 3. The methodology section is much stronger than the last draft. Two things before you move on:\n\n1. The sampling rationale in 3.2 needs a sentence on why you excluded the pilot cohort.\n2. Figure 3.4 is doing a lot of work; consider splitting it into two panels.\n\nHappy to talk it through on Wednesday at 2pm if that suits.\n\nBest,\nKemi",
            html: None,
            attachments: &[],
        },
        Sample {
            account: 0,
            thread: "t-bank",
            id: "bank-1",
            from: ("Juniper Bank", "statements@juniper.example"),
            to: &[ME],
            subject: "Your September statement is ready",
            minutes_ago: 5 * HOUR,
            labels: &["INBOX", "CATEGORY_UPDATES"],
            text: "Your September statement is ready to view.",
            html: Some(
                r#"<table width="100%" cellpadding="0" cellspacing="0" style="font-family:Helvetica,Arial,sans-serif;background:#f4f1ec"><tr><td align="center" style="padding:28px 12px"><table width="560" cellpadding="0" cellspacing="0" style="background:#ffffff;border-radius:14px"><tr><td style="padding:26px 32px 8px;font-size:13px;letter-spacing:.12em;color:#2f6b4f;font-weight:bold">JUNIPER BANK</td></tr><tr><td style="padding:4px 32px 0;font-size:24px;font-weight:bold;color:#1d1d1f">Your September statement is ready</td></tr><tr><td style="padding:14px 32px;font-size:15px;line-height:1.55;color:#444">Hi Dana, your statement for the account ending 4821 is now available in online banking.</td></tr><tr><td style="padding:6px 32px 20px"><table width="100%" style="font-size:14px;color:#1d1d1f;border-top:1px solid #eee"><tr><td style="padding:10px 0">Opening balance</td><td align="right">$3,412.08</td></tr><tr><td style="padding:10px 0;border-top:1px solid #eee">Money in</td><td align="right" style="border-top:1px solid #eee;color:#2f6b4f">+$4,950.00</td></tr><tr><td style="padding:10px 0;border-top:1px solid #eee">Money out</td><td align="right" style="border-top:1px solid #eee">−$3,877.41</td></tr><tr><td style="padding:10px 0;border-top:1px solid #eee;font-weight:bold">Closing balance</td><td align="right" style="border-top:1px solid #eee;font-weight:bold">$4,484.67</td></tr></table></td></tr><tr><td style="padding:0 32px 30px"><a href="https://juniper.example/statements" style="display:inline-block;background:#2f6b4f;color:#fff;text-decoration:none;padding:12px 22px;border-radius:999px;font-weight:bold;font-size:14px">View statement</a></td></tr></table><p style="font-size:12px;color:#8a8a8a;margin:18px 0 0">Juniper Bank will never ask for your password by email.</p></td></tr></table>"#,
            ),
            attachments: &[],
        },
        Sample {
            account: 0,
            thread: "t-lake",
            id: "lake-1",
            from: ("Theo Lindqvist", "theo@example.net"),
            to: &[ME, ("Mara Okafor", "mara.okafor@example.org")],
            subject: "Photos from the lake",
            minutes_ago: DAY + 3 * HOUR,
            labels: &["INBOX", "STARRED"],
            text: "Finally got these off the camera. The one of the dock at sunrise might be the best photo I've taken all year.\n\nFull album: https://photos.example.net/lake-2026",
            html: None,
            attachments: &[
                ("dock-sunrise.jpg", "image/jpeg", 2_480_000),
                ("ridge.jpg", "image/jpeg", 3_120_000),
            ],
        },
        Sample {
            account: 1,
            thread: "t-crit",
            id: "crit-1",
            from: ("Jonas Weber", "jonas@fernwood.example"),
            to: &[ME],
            subject: "Design crit notes",
            minutes_ago: DAY + 6 * HOUR,
            labels: &["INBOX"],
            text: "Notes from today's crit:\n\n- Onboarding: people missed the skip link. Make it a real button.\n- Settings: group the sync options under one heading.\n- Empty states: everyone loved the illustrations. Keep them.\n\nI'll turn these into tickets tomorrow.",
            html: None,
            attachments: &[],
        },
        Sample {
            account: 0,
            thread: "t-parcel",
            id: "parcel-1",
            from: ("Packet Post", "tracking@packetpost.example"),
            to: &[ME],
            subject: "Your parcel is out for delivery",
            minutes_ago: 2 * DAY + 2 * HOUR,
            labels: &["INBOX", "CATEGORY_UPDATES"],
            text: "Your parcel is out for delivery today between 10:00 and 14:00.",
            html: Some(
                r#"<div style="font-family:Arial,sans-serif;max-width:520px;margin:0 auto;padding:24px;color:#222"><div style="font-size:20px;font-weight:bold;color:#d9480f">Packet Post</div><h2 style="margin:18px 0 6px;font-size:22px">Arriving today</h2><p style="font-size:15px;color:#444;margin:0 0 18px">Your parcel from Linden Books is out for delivery between <b>10:00 and 14:00</b>.</p><div style="background:#fff4e6;border-radius:10px;padding:14px 16px;font-size:14px">Tracking number <b>PP 4417 2290 118</b></div><p style="font-size:12px;color:#888;margin-top:22px">You're receiving this because you placed an order with a Packet Post partner.</p></div>"#,
            ),
            attachments: &[],
        },
        Sample {
            account: 1,
            thread: "t-invite",
            id: "invite-1",
            from: ("Ines Duarte", "ines@fernwood.example"),
            to: &[ME],
            subject: "Contract draft v3",
            minutes_ago: 3 * DAY + 4 * HOUR,
            labels: &["INBOX", "STARRED", "Label_clients", "Label_clients_mf"],
            text: "Hi Dana,\n\nAttached is v3 with the changes from legal. The only substantive edit is the payment schedule in section 4, now net 30 instead of net 45.\n\nIf you're happy, I'll send it to Maple & Finch for signature on Monday.\n\nInês",
            html: None,
            attachments: &[(
                "fernwood-maple-finch-v3.docx",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                86_400,
            )],
        },
        Sample {
            account: 0,
            thread: "t-recipe",
            id: "recipe-1",
            from: ("Lucia Reyes", "lucia.reyes@example.com"),
            to: &[ME],
            subject: "The recipe you asked for",
            minutes_ago: 4 * DAY + 5 * HOUR,
            labels: &["INBOX"],
            text: "Here it is, exactly how your grandmother wrote it down:\n\nArroz con pollo\n- 1 whole chicken, in pieces\n- 2 cups rice\n- 1 onion, 1 pepper, 3 cloves garlic\n- a pinch of saffron (don't skip it)\n\nBrown the chicken first. Be patient with it.\n\nCall me on Sunday!\nMamá",
            html: None,
            attachments: &[],
        },
        Sample {
            account: 2,
            thread: "t-library",
            id: "library-1",
            from: ("University Library", "library@uni.example"),
            to: &[ME],
            subject: "Two books are due next week",
            minutes_ago: 6 * DAY + 2 * HOUR,
            labels: &["INBOX", "CATEGORY_UPDATES"],
            text: "Hello Dana,\n\nThese items are due on 24 September:\n\n- Research Design in Practice\n- Visualizing Data, 2nd ed.\n\nRenew online at https://library.uni.example/account\n\nUniversity Library",
            html: None,
            attachments: &[],
        },
        Sample {
            account: 1,
            thread: "t-offsite",
            id: "offsite-1",
            from: ("Priya Raman", "priya@fernwood.example"),
            to: &[ME],
            subject: "Offsite logistics",
            minutes_ago: 12 * DAY,
            labels: &["Label_travel"],
            text: "Train tickets are booked for everyone. Hotel confirmation to follow.",
            html: None,
            attachments: &[],
        },
        Sample {
            account: 0,
            thread: "t-concert",
            id: "concert-1",
            from: ("Hollow Pines Hall", "tickets@hollowpines.example"),
            to: &[ME],
            subject: "Your tickets for The Night Ferries",
            minutes_ago: 19 * DAY,
            labels: &["INBOX", "CATEGORY_UPDATES"],
            text: "Doors open at 19:30. Show this email at the entrance.",
            html: None,
            attachments: &[("tickets.pdf", "application/pdf", 214_000)],
        },
        Sample {
            account: 1,
            thread: "t-draft",
            id: "draft-1",
            from: (DISPLAY_NAME, "dana@fernwood.example"),
            to: &[("Priya Raman", "priya@fernwood.example")],
            subject: "Estimate check",
            minutes_ago: 30,
            labels: &["DRAFT"],
            text: "Priya,\n\nI went through Jonas's numbers. October holds **if** we",
            html: None,
            attachments: &[],
        },
        Sample {
            account: 1,
            thread: "t-invoice",
            id: "invoice-1",
            from: (DISPLAY_NAME, "dana@fernwood.example"),
            to: &[("Owen Mercer", "accounts@mapleandfinch.example")],
            subject: "Invoice 2291 for August",
            minutes_ago: 5 * DAY + 3 * HOUR,
            labels: &["SENT"],
            text: "Hi Owen,\n\nAttached is invoice 2291 for the August work, due on the 30th. Could you confirm it reached the right person?\n\nThanks,\nDana",
            html: None,
            attachments: &[("invoice-2291.pdf", "application/pdf", 96_000)],
        },
        Sample {
            account: 0,
            thread: "t-lease",
            id: "lease-1",
            from: (DISPLAY_NAME, "dana.reyes@example.com"),
            to: &[("Harbor Lane Lettings", "office@harborlane.example")],
            subject: "Question about the lease renewal",
            minutes_ago: 8 * DAY + 5 * HOUR,
            labels: &["SENT"],
            text: "Hello,\n\nMy lease ends on 31 October. Can I renew for another twelve months at the current rent? Happy to sign whenever the paperwork is ready.\n\nBest,\nDana Reyes",
            html: None,
            attachments: &[],
        },
        Sample {
            account: 0,
            thread: "t-news",
            id: "news-1",
            from: ("Trail Notes", "hello@trailnotes.example"),
            to: &[ME],
            subject: "Five autumn loops under 15 km",
            minutes_ago: 7 * HOUR,
            labels: &["INBOX", "CATEGORY_PROMOTIONS"],
            text: "This week: five autumn loops under 15 km, a gear list for cold mornings, and where the larches turn first.",
            html: None,
            attachments: &[],
        },
        Sample {
            account: 0,
            thread: "t-sale",
            id: "sale-1",
            from: ("Linden Books", "offers@lindenbooks.example"),
            to: &[ME],
            subject: "20% off travel guides this weekend",
            minutes_ago: DAY + 9 * HOUR,
            labels: &["INBOX", "UNREAD", "CATEGORY_PROMOTIONS"],
            text: "Planning a trip? Every travel guide is 20% off until Sunday night. Use code WANDER at checkout.",
            html: None,
            attachments: &[],
        },
        Sample {
            account: 0,
            thread: "t-pinecone",
            id: "pinecone-1",
            from: ("Pinecone", "notify@pinecone.example"),
            to: &[ME],
            subject: "Mara Okafor mentioned you in a comment",
            minutes_ago: 9 * HOUR,
            labels: &["INBOX", "UNREAD", "CATEGORY_SOCIAL"],
            text: "Mara Okafor mentioned you: \"@dana this is the ridge we're doing on Saturday!\" Reply on Pinecone.",
            html: None,
            attachments: &[],
        },
        Sample {
            account: 2,
            thread: "t-seminar",
            id: "seminar-1",
            from: ("Grad Seminar List", "grad-seminar@uni.example"),
            to: &[("", "grad-seminar@uni.example")],
            subject: "[grad-seminar] Room change for Thursday",
            minutes_ago: DAY + 2 * HOUR,
            labels: &["INBOX", "CATEGORY_FORUMS"],
            text: "Thursday's seminar moves to room B214. Same time, 4pm. Coffee provided.",
            html: None,
            attachments: &[],
        },
        Sample {
            account: 1,
            thread: "t-design-review",
            id: "design-review-1",
            from: ("Priya Raman", "priya@fernwood.example"),
            to: &[ME, ("Jonas Weber", "jonas@fernwood.example")],
            subject: "Invitation: Offline editor design review",
            minutes_ago: 4 * HOUR,
            labels: &["INBOX", "UNREAD"],
            text: "Walking through the offline editor design before we commit to a date. Agenda in the deck; bring questions about conflict resolution.\n\nPriya",
            html: None,
            attachments: &[("invite.ics", "text/calendar", 1_284)],
        },
        Sample {
            account: 1,
            thread: "t-planning",
            id: "planning-1",
            from: ("Jonas Weber", "jonas@fernwood.example"),
            to: &[ME],
            subject: "Updated invitation: Sprint planning",
            minutes_ago: 2 * HOUR,
            labels: &["INBOX", "UNREAD"],
            text: "Moved this so the whole team can make it. Same room.\n\nJonas",
            html: None,
            attachments: &[("invite.ics", "text/calendar", 892)],
        },
        Sample {
            account: 0,
            thread: "t-prize",
            id: "prize-1",
            from: ("Rewards Desk", "winner@prize-center.example"),
            to: &[ME],
            subject: "You have been selected!!!",
            minutes_ago: 5 * HOUR,
            labels: &["SPAM", "UNREAD", "CATEGORY_PROMOTIONS"],
            text: "Claim your gift card today. Offer ends at midnight.",
            html: None,
            attachments: &[],
        },
        Sample {
            account: 0,
            thread: "t-webinar",
            id: "webinar-1",
            from: ("Growth Weekly", "news@growth.example"),
            to: &[ME],
            subject: "Last chance: webinar seats",
            minutes_ago: 2 * DAY,
            labels: &["TRASH", "CATEGORY_PROMOTIONS"],
            text: "Seats for Thursday's webinar are almost gone.",
            html: None,
            attachments: &[],
        },
    ]
}

/// Gmail for demo mode: one in-memory mailbox per sample account. The demo
/// keeps them for as long as the app runs, so rules, hidden addresses, and
/// automatic replies made in the demo survive a sync restart.
pub struct DemoGmail(HashMap<AccountId, Arc<FakeGmail>>);

impl DemoGmail {
    pub fn account(&self, account_id: AccountId) -> Option<Arc<FakeGmail>> {
        self.0.get(&account_id).cloned()
    }
}

/// Fills an empty store with the demo accounts and mail, and builds the
/// Gmail behind them from the same samples.
pub fn seed(conn: &Connection, now: EpochMillis) -> Result<DemoGmail> {
    let mut account_ids = Vec::new();
    let mut gmail = HashMap::new();
    for email in ACCOUNTS {
        let id = accounts::insert_account(conn, email, now)?;
        accounts::start_generation(conn, id, HISTORY_ID)?;
        accounts::set_backfill(conn, id, None, true)?;
        accounts::set_state(conn, id, AccountState::Ok)?;
        let account_labels = account_labels(id, email);
        labels::replace_labels(conn, id, &account_labels)?;
        gmail.insert(id, Arc::new(gmail_for(email, &account_labels)));
        account_ids.push(id);
    }
    for sample in samples() {
        let account_id = account_ids[sample.account];
        let fake = &gmail[&account_id];
        let meta = sample.meta(account_id, now);
        let body = sample.body(now);
        messages::upsert_message(conn, &meta, 2)?;
        messages::refresh_thread(conn, account_id, sample.thread)?;
        bodies::put_body(conn, account_id, sample.id, &body, now)?;
        fake.with(|state| {
            for attachment in &body.attachments {
                let id = attachment.attachment_id.clone().unwrap_or_default();
                state
                    .attachments
                    .insert((meta.id.clone(), id.clone()), stand_in(&id));
            }
            if meta.has_label(system_label::DRAFT) {
                state
                    .drafts
                    .insert(DRAFT_ID.into(), sample.text.as_bytes().to_vec());
                state
                    .draft_messages
                    .insert(DRAFT_ID.into(), meta.id.clone());
            }
            if sample.id == "design-review-1" {
                // Google puts an invitation on the guest's calendar as it
                // arrives, so the demo has an event to answer.
                state.calendar.insert(INVITE_UID.into(), None);
            }
            if sample.id == "planning-1" {
                state.calendar.insert(MOVED_UID.into(), None);
            }
            state.bodies.insert(meta.id.clone(), body.clone());
            state.messages.insert(meta.id.clone(), meta.clone());
        });
    }
    remember_the_older_invitation(conn, account_ids[1], now)?;
    Ok(DemoGmail(gmail))
}

/// The contacts the demo accounts have written down, with a photo each
/// where a real address book would have one. Demo photos are drawn here
/// rather than shipped, so nothing in the repository is a picture of a
/// person who does not exist.
struct SampleContact {
    /// Whose address book holds them: an index into [`ACCOUNTS`].
    account: usize,
    resource: &'static str,
    name: &'static str,
    email: &'static str,
    organization: &'static str,
    phone: Option<&'static str>,
    /// The colour their drawn portrait uses. `None` leaves them with
    /// initials, as a contact with no photo has.
    photo: Option<(u8, u8, u8)>,
}

const CONTACTS: [SampleContact; 4] = [
    SampleContact {
        account: 0,
        resource: "people/c1",
        name: "Mara Okafor",
        email: "mara.okafor@example.org",
        organization: "Ridgeline Trails",
        phone: Some("+1 555 0100"),
        photo: Some((0x2d, 0x6a, 0x4f)),
    },
    SampleContact {
        account: 1,
        resource: "people/c2",
        name: "Priya Raman",
        email: "priya@fernwood.example",
        organization: "Fernwood",
        phone: Some("+1 555 0142"),
        photo: Some((0x7b, 0x2c, 0x6b)),
    },
    SampleContact {
        account: 1,
        resource: "people/c3",
        name: "Jonas Weber",
        email: "jonas@fernwood.example",
        organization: "Fernwood",
        phone: None,
        photo: None,
    },
    SampleContact {
        account: 2,
        resource: "people/c4",
        name: "Sam Iyer",
        email: "s.iyer@uni.example",
        organization: "Department of Geology",
        phone: None,
        photo: Some((0x1b, 0x4d, 0x7a)),
    },
];

/// Writes the demo address books, with a photo on disk for the contacts
/// that have one. A photo that cannot be written leaves that contact with
/// initials, which is what a real one with no photo shows.
pub fn seed_contacts(conn: &Connection, photo_dir: &std::path::Path) -> Result<()> {
    let ids = accounts::list_accounts(conn)?;
    let _ = std::fs::create_dir_all(photo_dir);
    for contact in CONTACTS {
        let Some(account_id) = ids.get(contact.account).map(|a| a.id) else {
            continue;
        };
        let photo_file = contact.photo.and_then(|color| {
            let file = format!("{account_id}-{}.png", contact.resource.replace('/', "-"));
            match std::fs::write(photo_dir.join(&file), portrait(color)) {
                Ok(()) => Some(file),
                Err(err) => {
                    tracing::warn!(error = %err, "could not write a demo contact photo");
                    None
                }
            }
        });
        address_book::save(
            conn,
            &[address_book::Contact {
                account_id,
                resource: contact.resource.into(),
                name: Some(contact.name.into()),
                emails: vec![contact.email.into()],
                organization: Some(contact.organization.into()),
                phone: contact.phone.map(str::to_string),
                photo_url: contact
                    .photo
                    .map(|_| format!("https://photos.example/{}", contact.resource)),
                photo_file,
            }],
        )?;
    }
    Ok(())
}

/// A stand-in portrait: a head and shoulders in one colour on a lighter
/// wash of it, as a PNG.
fn portrait(color: (u8, u8, u8)) -> Vec<u8> {
    const SIZE: usize = 128;
    let (r, g, b) = color;
    let wash = |c: u8| (u16::from(c) / 3 + 175).min(255) as u8;
    let mut pixels = vec![0u8; SIZE * SIZE * 3];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (dx, dy) = (x as f32 - 64.0, y as f32 - 48.0);
            let head = (dx / 27.0).powi(2) + (dy / 31.0).powi(2) <= 1.0;
            let shoulders = (dx / 58.0).powi(2) + ((y as f32 - 150.0) / 62.0).powi(2) <= 1.0;
            let ink = head || shoulders;
            let at = (y * SIZE + x) * 3;
            pixels[at] = if ink { r } else { wash(r) };
            pixels[at + 1] = if ink { g } else { wash(g) };
            pixels[at + 2] = if ink { b } else { wash(b) };
        }
    }
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_mut_slice(
        pixels,
        gtk::gdk_pixbuf::Colorspace::Rgb,
        false,
        8,
        SIZE as i32,
        SIZE as i32,
        (SIZE * 3) as i32,
    );
    pixbuf
        .save_to_bufferv("png", &[])
        .map(|bytes| bytes.to_vec())
        .unwrap_or_default()
}

/// Puts the version of the sprint planning meeting that came before the
/// update in the inbox into the store, as if the demo had opened it last
/// week. The card then says the meeting moved, and from when.
fn remember_the_older_invitation(
    conn: &Connection,
    account_id: AccountId,
    now: EpochMillis,
) -> Result<()> {
    let older = invitations::Saved {
        uid: MOVED_UID.into(),
        sequence: 0,
        starts_at: Some(planning_was(now).timestamp_millis()),
        all_day: false,
        summary: "Sprint planning".into(),
        cancelled: false,
        answer: None,
        message_id: "planning-0".into(),
        news: None,
        moved_from: None,
    };
    invitations::remember(conn, account_id, &older, now)
}

/// The labels one demo account has. Only the work account has user labels.
fn account_labels(account_id: AccountId, email: &str) -> Vec<Label> {
    let mut all: Vec<Label> = [
        system_label::INBOX,
        system_label::SENT,
        system_label::DRAFT,
        system_label::STARRED,
        system_label::UNREAD,
        system_label::IMPORTANT,
    ]
    .into_iter()
    .map(|l| Label {
        account_id,
        id: l.into(),
        name: l.into(),
        kind: LabelKind::System,
        color: None,
    })
    .collect();
    if email == ACCOUNTS[1] {
        for (label, name, color) in [
            ("Label_clients", "Clients", Some("#4a86e8")),
            ("Label_clients_mf", "Clients/Maple & Finch", None),
            ("Label_travel", "Travel", Some("#16a766")),
        ] {
            all.push(Label {
                account_id,
                id: label.into(),
                name: name.into(),
                kind: LabelKind::User,
                color: color.map(str::to_string),
            });
        }
    }
    all
}

/// An empty in-memory Gmail for one demo account, with its identity, its
/// labels, and a history cursor the store already holds.
fn gmail_for(email: &str, account_labels: &[Label]) -> FakeGmail {
    let fake = FakeGmail::new();
    fake.with(|state| {
        state.email = email.into();
        state.display_name = Some(DISPLAY_NAME.into());
        state.signature = Some(format!("{DISPLAY_NAME}\nSent from Penguin Mail"));
        // The work account sends as two addresses, as a Gmail account with
        // verified aliases does, so the demo shows the From row doing its job.
        if email == ACCOUNTS[1] {
            state.send_as = vec![
                SendAs {
                    send_as_email: "hello@fernwood.example".into(),
                    display_name: "Fernwood Studio".into(),
                    is_default: false,
                    is_primary: false,
                    signature: "<p>Fernwood Studio<br>hello@fernwood.example</p>".into(),
                    verification_status: Some("accepted".into()),
                },
                // Still waiting on its owner to confirm it, so it is left out.
                SendAs {
                    send_as_email: "press@fernwood.example".into(),
                    display_name: "Fernwood Press".into(),
                    is_default: false,
                    is_primary: false,
                    signature: String::new(),
                    verification_status: Some("pending".into()),
                },
            ];
        }
        state.history_id = HISTORY_ID;
        // The demo lists a mailbox in one page, as a Gmail search does.
        state.page_size = 1000;
        state.labels = account_labels
            .iter()
            .map(|l| RemoteLabel {
                id: l.id.clone(),
                name: l.name.clone(),
                kind: Some(match l.kind {
                    LabelKind::System => "system".into(),
                    LabelKind::User => "user".to_string(),
                }),
                color: l.color.as_ref().map(|c| LabelColor {
                    background_color: c.clone(),
                    text_color: "#ffffff".into(),
                }),
            })
            .collect();
    });
    fake
}

/// What the demo hands back for an attachment, since the samples name files
/// that do not exist.
fn stand_in(attachment_id: &str) -> Vec<u8> {
    format!("This is {attachment_id}, a stand-in file from Penguin Mail demo mode.\n").into_bytes()
}

impl Sample {
    fn meta(&self, account_id: AccountId, now: EpochMillis) -> MessageMeta {
        let me = Address {
            name: Some(DISPLAY_NAME.into()),
            email: ACCOUNTS[self.account].into(),
        };
        let address = |(name, email): (&str, &str)| {
            if email.is_empty() {
                me.clone()
            } else {
                Address {
                    name: (!name.is_empty()).then(|| name.to_string()),
                    email: email.into(),
                }
            }
        };
        MessageMeta {
            account_id,
            id: self.id.into(),
            thread_id: self.thread.into(),
            rfc822_msgid: Some(format!("<{}@demo.example>", self.id)),
            from: Some(address(self.from)),
            to: self.to.iter().map(|&a| address(a)).collect(),
            cc: vec![],
            subject: self.subject.into(),
            date: now - self.minutes_ago * 60_000,
            snippet: self
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(140)
                .collect(),
            size: self.text.len() as i64,
            has_attachments: !self.attachments.is_empty(),
            label_ids: self.labels.iter().map(|l| l.to_string()).collect(),
        }
    }

    fn body(&self, now: EpochMillis) -> MessageBody {
        MessageBody {
            text: Some(self.text.into()),
            html: self.html.map(str::to_string),
            attachments: self
                .attachments
                .iter()
                .enumerate()
                .map(|(i, &(filename, mime_type, size))| Attachment {
                    part_id: (i + 1).to_string(),
                    filename: filename.into(),
                    mime_type: mime_type.into(),
                    size,
                    attachment_id: Some(format!("{}-att-{i}", self.id)),
                    content_id: None,
                })
                .collect(),
            list_unsubscribe: (self.id == "news-1").then(|| {
                "<mailto:leave@trailnotes.example?subject=unsubscribe>, <https://trailnotes.example/u/dana>"
                    .to_string()
            }),
            one_click_unsubscribe: false,
            calendar: match self.id {
                "design-review-1" => Some(invitation_ics(now)),
                "planning-1" => Some(moved_ics(now)),
                _ => None,
            },
        }
    }
}

/// The sample invitation, written around `now` so the meeting is always a
/// few days out and the card shows a real date.
fn invitation_ics(now: EpochMillis) -> String {
    let stamp = |at: chrono::DateTime<chrono::Utc>| at.format("%Y%m%dT%H%M%SZ").to_string();
    let sent = chrono::DateTime::from_timestamp_millis(now).unwrap_or_default();
    let start = next_tuesday(sent.with_timezone(&chrono::Local));
    let end = start + chrono::Duration::minutes(45);
    let until = start + chrono::Duration::weeks(8);
    [
        "BEGIN:VCALENDAR".to_string(),
        "PRODID:-//Google Inc//Google Calendar 70.9054//EN".to_string(),
        "VERSION:2.0".to_string(),
        "METHOD:REQUEST".to_string(),
        "BEGIN:VEVENT".to_string(),
        format!("UID:{INVITE_UID}"),
        "SEQUENCE:0".to_string(),
        "STATUS:CONFIRMED".to_string(),
        "SUMMARY:Offline editor design review".to_string(),
        "LOCATION:Meeting Room 2\\, Fernwood HQ".to_string(),
        "DESCRIPTION:Agenda in the deck. Bring questions about conflict resolution.".to_string(),
        format!("DTSTAMP:{}", stamp(sent)),
        format!("DTSTART:{}", stamp(start.with_timezone(&chrono::Utc))),
        format!("DTEND:{}", stamp(end.with_timezone(&chrono::Utc))),
        format!(
            "RRULE:FREQ=WEEKLY;BYDAY=TU;UNTIL={}",
            stamp(until.with_timezone(&chrono::Utc))
        ),
        "ORGANIZER;CN=Priya Raman:mailto:priya@fernwood.example".to_string(),
        format!(
            "ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE;CN=Dana Reyes:mailto:{}",
            ACCOUNTS[1]
        ),
        "ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED;CN=Priya Raman:mailto:priya@fernwood.example".to_string(),
        "ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=TENTATIVE;CN=Jonas Weber:mailto:jonas@fernwood.example".to_string(),
        "ATTENDEE;ROLE=OPT-PARTICIPANT;PARTSTAT=DECLINED;CN=Mara Okafor:mailto:mara.okafor@example.org".to_string(),
        "END:VEVENT".to_string(),
        "END:VCALENDAR".to_string(),
        String::new(),
    ]
    .join("\r\n")
}

/// The update that moves the sprint planning meeting.
fn moved_ics(now: EpochMillis) -> String {
    let stamp = |at: chrono::DateTime<chrono::Utc>| at.format("%Y%m%dT%H%M%SZ").to_string();
    let sent = chrono::DateTime::from_timestamp_millis(now).unwrap_or_default();
    let start = planning_is(now);
    let end = start + chrono::Duration::minutes(60);
    [
        "BEGIN:VCALENDAR".to_string(),
        "PRODID:-//Google Inc//Google Calendar 70.9054//EN".to_string(),
        "VERSION:2.0".to_string(),
        "METHOD:REQUEST".to_string(),
        "BEGIN:VEVENT".to_string(),
        format!("UID:{MOVED_UID}"),
        "SEQUENCE:1".to_string(),
        "STATUS:CONFIRMED".to_string(),
        "SUMMARY:Sprint planning".to_string(),
        "LOCATION:Meeting Room 1\\, Fernwood HQ".to_string(),
        format!("DTSTAMP:{}", stamp(sent)),
        format!("DTSTART:{}", stamp(start.with_timezone(&chrono::Utc))),
        format!("DTEND:{}", stamp(end.with_timezone(&chrono::Utc))),
        "ORGANIZER;CN=Jonas Weber:mailto:jonas@fernwood.example".to_string(),
        format!(
            "ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE;CN=Dana Reyes:mailto:{}",
            ACCOUNTS[1]
        ),
        "ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED;CN=Jonas Weber:mailto:jonas@fernwood.example".to_string(),
        "ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED;CN=Priya Raman:mailto:priya@fernwood.example".to_string(),
        "END:VEVENT".to_string(),
        "END:VCALENDAR".to_string(),
        String::new(),
    ]
    .join("\r\n")
}

/// Where the sprint planning meeting sat before the update: Wednesday at
/// 15:00 local.
fn planning_was(now: EpochMillis) -> chrono::DateTime<chrono::Local> {
    weekday_at(now, chrono::Weekday::Wed, 15)
}

/// Where it sits now: Thursday at 11:00 local.
fn planning_is(now: EpochMillis) -> chrono::DateTime<chrono::Local> {
    weekday_at(now, chrono::Weekday::Thu, 11)
}

/// The next `weekday` after `now`, at `hour` local.
fn weekday_at(
    now: EpochMillis,
    weekday: chrono::Weekday,
    hour: u32,
) -> chrono::DateTime<chrono::Local> {
    let from = chrono::DateTime::from_timestamp_millis(now)
        .unwrap_or_default()
        .with_timezone(&chrono::Local);
    next_weekday(from, weekday, hour)
}

/// The next Tuesday after `from`, at 14:00 local.
fn next_tuesday(from: chrono::DateTime<chrono::Local>) -> chrono::DateTime<chrono::Local> {
    next_weekday(from, chrono::Weekday::Tue, 14)
}

/// The next `weekday` after `from`, at `hour` local.
fn next_weekday(
    from: chrono::DateTime<chrono::Local>,
    weekday: chrono::Weekday,
    hour: u32,
) -> chrono::DateTime<chrono::Local> {
    use chrono::{Datelike, TimeZone};
    let days = (weekday.num_days_from_monday() + 7 - from.weekday().num_days_from_monday()) % 7;
    let day = from.date_naive() + chrono::Days::new(if days == 0 { 7 } else { u64::from(days) });
    day.and_hms_opt(hour, 0, 0)
        .and_then(|at| chrono::Local.from_local_datetime(&at).earliest())
        .unwrap_or(from)
}

#[cfg(test)]
mod tests {
    use mailrs_domain::Folder;
    use mailrs_store::threads::{self, ThreadFilter};
    use mailrs_store::{bodies, open_in_memory};
    use mailrs_sync::GmailApi;

    use super::*;

    #[test]
    fn the_demo_contacts_have_photos_and_rank_first() {
        let conn = open_in_memory().unwrap();
        seed(&conn, 1_700_000_000_000).unwrap();
        let dir = tempfile::tempdir().unwrap();
        seed_contacts(&conn, dir.path()).unwrap();

        let mara = mailrs_store::address_book::find(&conn, "mara.okafor@example.org")
            .unwrap()
            .expect("Mara is in the demo address book");
        assert_eq!(mara.organization.as_deref(), Some("Ridgeline Trails"));
        let photo = dir.path().join(mara.photo_file.expect("Mara has a photo"));
        // A PNG, so the avatar can read it.
        assert_eq!(&std::fs::read(&photo).unwrap()[1..4], b"PNG");

        let suggestions = mailrs_store::contacts::suggestions(&conn).unwrap();
        let known: Vec<&str> = suggestions
            .iter()
            .take_while(|s| s.known)
            .map(|s| s.email.as_str())
            .collect();
        assert_eq!(known.len(), 4, "every demo contact comes before the rest");
        assert!(known.contains(&"jonas@fernwood.example"));
    }

    #[test]
    fn two_sent_messages_wait_for_a_reply() {
        let conn = open_in_memory().unwrap();
        let now = 1_758_000_000_000;
        seed(&conn, now).unwrap();
        let waiting: Vec<String> = mailrs_store::follow_ups::waiting(&conn, now)
            .unwrap()
            .into_iter()
            .map(|f| f.thread_id)
            .collect();
        assert_eq!(waiting, ["t-invoice", "t-lease"]);
    }

    #[test]
    fn no_two_samples_in_one_account_share_a_message_id() {
        let mut seen = std::collections::HashSet::new();
        for sample in samples() {
            assert!(
                seen.insert((sample.account, sample.id)),
                "{} is used twice in account {}",
                sample.id,
                sample.account
            );
        }
    }

    #[test]
    fn the_invitations_land_in_the_demo_mailbox() {
        let conn = open_in_memory().unwrap();
        let now = 1_758_000_000_000;
        seed(&conn, now).unwrap();
        let work = accounts::account_by_email(&conn, ACCOUNTS[1])
            .unwrap()
            .unwrap()
            .id;
        for id in ["design-review-1", "planning-1"] {
            let body = bodies::get_body(&conn, work, id, now).unwrap().unwrap();
            let ics = body.calendar.expect("the message carries an invitation");
            let invitation =
                mailrs_domain::invitation::read(&ics).expect("the part holds an event");
            assert!(invitation.when.is_some(), "{id}");
            assert!(!invitation.guests.is_empty(), "{id}");
        }
        // The update in the inbox moves a meeting the demo already knows.
        let held = mailrs_store::invitations::saved(&conn, work, MOVED_UID)
            .unwrap()
            .expect("the older version is remembered");
        assert_eq!(held.sequence, 0);
        assert_eq!(held.starts_at, Some(planning_was(now).timestamp_millis()));
    }

    #[test]
    fn every_inbox_category_has_demo_mail() {
        let conn = open_in_memory().unwrap();
        seed(&conn, 1_758_000_000_000).unwrap();
        for labels in [
            &["CATEGORY_UPDATES"][..],
            &["CATEGORY_PROMOTIONS"],
            &["CATEGORY_SOCIAL", "CATEGORY_FORUMS"],
        ] {
            let filter = ThreadFilter::unified("INBOX").with_labels(labels, &[]);
            assert!(
                threads::count_threads(&conn, &filter).unwrap() >= 2,
                "{labels:?}"
            );
        }
    }

    #[test]
    fn the_demo_store_has_a_lively_unified_inbox() {
        let conn = open_in_memory().unwrap();
        seed(&conn, 1_758_000_000_000).unwrap();
        let inbox = ThreadFilter::unified("INBOX");
        let threads = threads::list_threads(&conn, &inbox, 0, 100).unwrap();
        assert!(threads.len() >= 10, "{}", threads.len());
        assert!(threads::unread_threads(&conn, &inbox).unwrap() >= 3);
        assert_eq!(threads[0].id, "t-hike");
        let accounts_seen: std::collections::HashSet<_> =
            threads.iter().map(|t| t.account_id).collect();
        assert_eq!(accounts_seen.len(), 3);
        assert_eq!(
            threads::list_threads(&conn, &ThreadFilter::unified("DRAFT"), 0, 10)
                .unwrap()
                .len(),
            1
        );
        for sample in samples() {
            let account = accounts::account_by_email(&conn, ACCOUNTS[sample.account])
                .unwrap()
                .unwrap();
            assert!(
                bodies::get_body(&conn, account.id, sample.id, 0)
                    .unwrap()
                    .is_some(),
                "{}",
                sample.id
            );
        }
    }

    /// Ids a search brings back from one account's demo Gmail.
    async fn found(gmail: &DemoGmail, account: AccountId, query: &str) -> Vec<String> {
        gmail
            .account(account)
            .expect("the account has a mailbox")
            .list_messages(query, None)
            .await
            .expect("the search runs")
            .messages
            .into_iter()
            .map(|m| m.id)
            .collect()
    }

    #[tokio::test]
    async fn the_folders_come_from_the_demo_gmail() {
        let conn = open_in_memory().unwrap();
        let gmail = seed(&conn, 1_758_000_000_000).unwrap();
        let account = accounts::account_by_email(&conn, ACCOUNTS[0])
            .unwrap()
            .unwrap()
            .id;
        assert_eq!(
            found(&gmail, account, Folder::Junk.query()).await,
            ["prize-1"]
        );
        assert_eq!(
            found(&gmail, account, Folder::Trash.query()).await,
            ["webinar-1"]
        );
        let all = found(&gmail, account, Folder::AllMail.query()).await;
        assert!(!all.contains(&"prize-1".to_string()));
        assert!(!all.contains(&"webinar-1".to_string()));
        assert!(all.contains(&"hike-1".to_string()));
        // What the search bar sends: plain words across sender and subject.
        assert_eq!(found(&gmail, account, "sunrise").await, ["lake-1"]);
    }

    #[tokio::test]
    async fn attachments_and_the_sample_draft_come_from_the_demo_gmail() {
        let conn = open_in_memory().unwrap();
        let gmail = seed(&conn, 1_758_000_000_000).unwrap();
        let work = accounts::account_by_email(&conn, ACCOUNTS[1])
            .unwrap()
            .unwrap()
            .id;
        let api = gmail.account(work).expect("the account has a mailbox");
        assert_eq!(
            api.draft_for_message("draft-1").await.unwrap().as_deref(),
            Some(DRAFT_ID)
        );
        let file = api
            .attachment("roadmap-1", "roadmap-1-att-0")
            .await
            .unwrap();
        assert!(String::from_utf8(file).unwrap().contains("stand-in file"));
        assert_eq!(api.signature().await.unwrap().as_deref().map(str::len), {
            let expect = format!("{DISPLAY_NAME}\nSent from Penguin Mail");
            Some(expect.len())
        });
    }
}
