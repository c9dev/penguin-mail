//! Sample mail for `penguin-mail --demo`: three accounts and a few weeks of
//! conversations. Every address uses a reserved `.example` domain.
//!
//! Each account gets a `FakeGmail` holding this mail, and sync's own first
//! sync against it fills a throwaway store, so the demo opens on a full
//! inbox that holds what a real account's store would. From there the demo
//! runs the same code as a real account.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::{
    AccountId, Address, Attachment, EpochMillis, MessageBody, MessageMeta, Provenance,
};
use mailrs_gmail::{LabelColor, RemoteLabel, SendAs};
use mailrs_store::{Db, Result, accounts, address_book, invitations};
use mailrs_sync::fake::{FakeGmail, fill_store};
use mailrs_sync::{AccountServices, AccountSync, SyncError};
use rusqlite::Connection;

mod pages;

/// The id of the draft behind the sample draft message, as Gmail would hold it.
const DRAFT_ID: &str = "demo-draft";

/// The event behind the sample invitation, as Google would write it.
const INVITE_UID: &str = "7f3k2q9demo1invite@google.com";

/// The event the sample update moves. The demo remembers an older version
/// of it, so opening the update says what changed.
const MOVED_UID: &str = "2b8h5x0demo2moved@google.com";

pub const DISPLAY_NAME: &str = "Dana Reyes";

/// One demo account and what its Gmail holds beside the mail.
struct SampleAccount {
    email: &'static str,
    /// User labels, each an id, a name, and the colour Gmail shows it in.
    labels: &'static [(&'static str, &'static str, Option<&'static str>)],
    /// Addresses the account sends as beyond its own, as a Gmail account
    /// with verified aliases does.
    aliases: &'static [Alias],
}

struct Alias {
    email: &'static str,
    name: &'static str,
    signature: &'static str,
    /// Whether the owner confirmed the address. One still waiting is left
    /// out of the From row.
    confirmed: bool,
}

const ACCOUNTS: [SampleAccount; 3] = [
    SampleAccount {
        email: "dana.reyes@example.com",
        labels: &[],
        aliases: &[],
    },
    SampleAccount {
        email: "dana@fernwood.example",
        labels: &[
            ("Label_clients", "Clients", Some("#4a86e8")),
            ("Label_clients_mf", "Clients/Maple & Finch", None),
            ("Label_travel", "Travel", Some("#16a766")),
        ],
        // Two addresses, so the demo shows the From row doing its job.
        aliases: &[
            Alias {
                email: "hello@fernwood.example",
                name: "Fernwood Studio",
                signature: "<p>Fernwood Studio<br>hello@fernwood.example</p>",
                confirmed: true,
            },
            Alias {
                email: "press@fernwood.example",
                name: "Fernwood Press",
                signature: "",
                confirmed: false,
            },
        ],
    },
    SampleAccount {
        email: "d.reyes@uni.example",
        labels: &[],
        aliases: &[],
    },
];

/// The system labels every demo account lists.
const SYSTEM_LABELS: [&str; 8] = [
    "INBOX",
    "SENT",
    "DRAFT",
    "TRASH",
    "SPAM",
    "STARRED",
    "UNREAD",
    "IMPORTANT",
];

struct Sample {
    /// Whose mailbox holds it: an index into [`ACCOUNTS`].
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
    /// The `List-Unsubscribe` header a mailing list puts on its mail.
    /// `{pages}` in it stands for the demo's own page server, which
    /// picks its port when the demo opens.
    unsubscribe: Option<&'static str>,
    /// Whether the sender promised RFC 8058 one-click, which is the
    /// `List-Unsubscribe-Post` header beside the one above.
    one_click: bool,
    /// Whether the message reached this computer unencrypted, which the
    /// details panel warns about.
    in_the_clear: bool,
    /// The calendar event the message carries.
    invitation: Option<Invite>,
    /// The id Gmail keeps the draft under, for a message that is a draft.
    draft: Option<&'static str>,
}

/// A calendar invitation inside a sample.
struct Invite {
    uid: &'static str,
    /// The iCalendar part, written around the time the demo starts.
    ics: fn(EpochMillis) -> String,
    /// The version of the meeting this one updates, which the demo has
    /// already seen.
    replaces: Option<Older>,
    /// The series the meeting belongs to, as the calendar holds it: its
    /// rule and when each occurrence starts.
    series: Option<fn(EpochMillis) -> Series>,
}

/// A series' rule, and when each of its occurrences starts.
type Series = (String, Vec<EpochMillis>);

/// An earlier version of a meeting, remembered as if the demo had opened
/// its invitation last week. The card then says the meeting moved, and
/// from when.
struct Older {
    message_id: &'static str,
    starts: fn(EpochMillis) -> chrono::DateTime<chrono::Local>,
}

/// What a sample is unless it says otherwise.
const PLAIN: Sample = Sample {
    account: 0,
    thread: "",
    id: "",
    from: ME,
    to: &[],
    subject: "",
    minutes_ago: 0,
    labels: &[],
    text: "",
    html: None,
    attachments: &[],
    unsubscribe: None,
    one_click: false,
    in_the_clear: false,
    invitation: None,
    draft: None,
};

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
            attachments: &[("q4-roadmap.pdf", "application/pdf", 1_842_000)],
            ..PLAIN
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
            ..PLAIN
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
            ..PLAIN
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
            ..PLAIN
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
            ..PLAIN
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
            ..PLAIN
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
            ..PLAIN
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
                r#"<table width="100%" cellpadding="0" cellspacing="0" style="font-family:Helvetica,Arial,sans-serif;background:#f4f1ec"><tr><td align="center" style="padding:28px 12px"><table width="560" cellpadding="0" cellspacing="0" style="background:#ffffff;border-radius:14px"><tr><td style="padding:26px 32px 8px;font-size:13px;letter-spacing:.12em;color:#2f6b4f;font-weight:bold">JUNIPER BANK <img src="https://juniper.example/logo.png" width="18" height="18" alt=""></td></tr><tr><td style="padding:4px 32px 0;font-size:24px;font-weight:bold;color:#1d1d1f">Your September statement is ready</td></tr><tr><td style="padding:14px 32px;font-size:15px;line-height:1.55;color:#444">Hi Dana, your statement for the account ending 4821 is now available in online banking.</td></tr><tr><td style="padding:6px 32px 20px"><table width="100%" style="font-size:14px;color:#1d1d1f;border-top:1px solid #eee"><tr><td style="padding:10px 0">Opening balance</td><td align="right">$3,412.08</td></tr><tr><td style="padding:10px 0;border-top:1px solid #eee">Money in</td><td align="right" style="border-top:1px solid #eee;color:#2f6b4f">+$4,950.00</td></tr><tr><td style="padding:10px 0;border-top:1px solid #eee">Money out</td><td align="right" style="border-top:1px solid #eee">−$3,877.41</td></tr><tr><td style="padding:10px 0;border-top:1px solid #eee;font-weight:bold">Closing balance</td><td align="right" style="border-top:1px solid #eee;font-weight:bold">$4,484.67</td></tr></table></td></tr><tr><td style="padding:0 32px 30px"><a href="https://juniper.example/statements" style="display:inline-block;background:#2f6b4f;color:#fff;text-decoration:none;padding:12px 22px;border-radius:999px;font-weight:bold;font-size:14px">View statement</a></td></tr></table><p style="font-size:12px;color:#8a8a8a;margin:18px 0 0">Juniper Bank will never ask for your password by email.</p></td></tr></table>"#,
            ),
            ..PLAIN
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
            attachments: &[
                ("dock-sunrise.jpg", "image/jpeg", 2_480_000),
                ("ridge.jpg", "image/jpeg", 3_120_000),
            ],
            ..PLAIN
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
            ..PLAIN
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
            ..PLAIN
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
            attachments: &[(
                "fernwood-maple-finch-v3.docx",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                86_400,
            )],
            ..PLAIN
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
            ..PLAIN
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
            ..PLAIN
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
            ..PLAIN
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
            attachments: &[("tickets.pdf", "application/pdf", 214_000)],
            ..PLAIN
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
            draft: Some(DRAFT_ID),
            ..PLAIN
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
            attachments: &[("invoice-2291.pdf", "application/pdf", 96_000)],
            ..PLAIN
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
            ..PLAIN
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
            // A page that asks which address to take off the list, and a
            // mail request the page comes before.
            unsubscribe: Some(
                "<mailto:leave@trailnotes.example?subject=unsubscribe>, <{pages}/trail-notes>",
            ),
            in_the_clear: true,
            ..PLAIN
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
            // A page with one button on it, which is the shape most
            // senders use.
            unsubscribe: Some("<{pages}/linden-books>"),
            ..PLAIN
        },
        Sample {
            account: 0,
            thread: "t-recipes",
            id: "recipes-1",
            from: ("Cedar Kitchen", "recipes@cedarkitchen.example"),
            to: &[ME],
            subject: "Three things to do with a glut of tomatoes",
            minutes_ago: 2 * DAY + 4 * HOUR,
            labels: &["INBOX", "CATEGORY_PROMOTIONS"],
            text: "Roast them slow, char them under the grill, or leave them overnight in salt and oil. Recipes for all three, plus what to do with the last of the basil.",
            // A sender who keeps the promise RFC 8058 asks for, so one
            // request is the whole of it and no page is loaded.
            unsubscribe: Some(
                "<mailto:leave@cedarkitchen.example?subject=unsubscribe>, <https://cedarkitchen.example/u/dana>",
            ),
            one_click: true,
            ..PLAIN
        },
        Sample {
            account: 0,
            thread: "t-beacon",
            id: "beacon-1",
            from: ("Beacon Outdoors", "news@beaconoutdoors.example"),
            to: &[ME],
            subject: "New winter jackets, and a weekend on the ridge",
            minutes_ago: 3 * DAY + 6 * HOUR,
            labels: &["INBOX", "CATEGORY_PROMOTIONS"],
            text: "The winter range is in, and there are eight places left on the guided ridge weekend in November.",
            // A preferences centre with a box that means all of it.
            unsubscribe: Some("<{pages}/beacon-outdoors>"),
            ..PLAIN
        },
        Sample {
            account: 0,
            thread: "t-arts",
            id: "arts-1",
            from: ("Harbour City Arts", "list@harbourarts.example"),
            to: &[ME],
            subject: "What's on this month: October",
            minutes_ago: 4 * DAY + HOUR,
            labels: &["INBOX", "CATEGORY_PROMOTIONS"],
            text: "Autumn season tickets are on sale, the print studio reopens on the 12th, and there are two late-night concerts at the docks.",
            // No header at all: the only way out is the link in the
            // footer, and it leads to a list of topics, which is the one
            // page the rules leave to the person.
            html: Some(
                "<p>Autumn season tickets are on sale, the print studio reopens on the 12th, \
                 and there are two late-night concerts at the docks.</p>\
                 <hr><p style=\"font-size:small;color:#77767b\">You are reading this because \
                 you asked us to. <a href=\"{pages}/harbour-arts\">Manage preferences</a>.</p>",
            ),
            ..PLAIN
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
            ..PLAIN
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
            // A list run by mailing-list software, which takes a
            // request by mail and offers nothing else.
            unsubscribe: Some("<mailto:grad-seminar-leave@uni.example?subject=unsubscribe>"),
            ..PLAIN
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
            attachments: &[("invite.ics", "text/calendar", 1_284)],
            invitation: Some(Invite {
                uid: INVITE_UID,
                ics: invitation_ics,
                replaces: None,
                series: None,
            }),
            ..PLAIN
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
            attachments: &[("invite.ics", "text/calendar", 892)],
            invitation: Some(Invite {
                uid: MOVED_UID,
                ics: moved_ics,
                replaces: Some(Older {
                    message_id: "planning-0",
                    starts: planning_was,
                }),
                series: Some(planning_series),
            }),
            ..PLAIN
        },
        Sample {
            account: 2,
            thread: "t-lab-move",
            id: "lab-move-1",
            from: ("Facilities", "facilities@uni.example"),
            to: &[("Physics All", "physics-all@uni.example")],
            subject: "[physics-all] Moving the microscopes, week of the 14th",
            minutes_ago: 3 * DAY,
            labels: &["MUTE"],
            text: "The two scopes in B110 move to B214 that week. Nobody needs to do anything.",
            ..PLAIN
        },
        Sample {
            account: 2,
            thread: "t-lab-move",
            id: "lab-move-2",
            from: ("Tomas Lind", "t.lind@uni.example"),
            to: &[("Physics All", "physics-all@uni.example")],
            subject: "Re: [physics-all] Moving the microscopes, week of the 14th",
            minutes_ago: 2 * DAY,
            labels: &["MUTE"],
            text: "Does that include the one nobody has booked since March?",
            ..PLAIN
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
            ..PLAIN
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
            ..PLAIN
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

/// Adds the demo accounts to an empty store, puts the samples in each
/// one's Gmail, and lets sync fill the store from there. Each body the
/// store can hold is read once through sync's cache, so opening a sample
/// asks Gmail nothing and the invitation cards have their events.
pub async fn seed(db: &Db, now: EpochMillis) -> std::result::Result<DemoGmail, SyncError> {
    let samples = samples();
    let mut gmail = HashMap::new();
    for (index, account) in ACCOUNTS.iter().enumerate() {
        let email = account.email;
        let account_id = db
            .write(move |c| accounts::insert_account(c, email, now))
            .await?;
        let fake = Arc::new(account.gmail());
        fake.keep_sent_copies(account_id);
        let mine: Vec<&Sample> = samples.iter().filter(|s| s.account == index).collect();
        for sample in &mine {
            sample.put_in(&fake, account_id, now);
        }
        // Nobody listens yet: the window reads the store once it opens.
        let (events, _) = async_channel::unbounded();
        let sync = AccountSync::new(
            account_id,
            AccountServices::fake(Arc::clone(&fake)),
            db.clone(),
            events,
        );
        fill_store(&sync).await?;
        for sample in &mine {
            sync.body(sample.id).await?;
            if let Some(older) = sample.remembered(now) {
                db.write(move |c| invitations::remember(c, account_id, &older, now))
                    .await?;
            }
        }
        if index == 0 {
            queue_samples(db, account_id, now).await?;
        }
        gmail.insert(account_id, fake);
    }
    Ok(DemoGmail(gmail))
}

/// Puts two messages in the first account's outbox, so the Outbox and
/// Send Later have something to show: one that could not reach Gmail and
/// waits for its next try, and one Send Later holds until tomorrow
/// morning. Neither ever reached Gmail, so each is named by its place in
/// the outbox, as such a message is for real.
async fn queue_samples(
    db: &Db,
    account_id: AccountId,
    now: EpochMillis,
) -> std::result::Result<(), SyncError> {
    use crate::compose::{Draft, build_mime};
    use mailrs_store::outbox::{self, Queued};

    let letters = [
        (
            "Dinner on Friday?",
            "Theo Brandt <theo.brandt@example.net>",
            "Hi Theo,\n\nAre you free for dinner on Friday? The new place on Alder \
             Street takes bookings for eight.\n\nDana",
            Some("Could not reach Gmail"),
            now + 20 * 60 * 1000,
        ),
        (
            "Book club in October",
            "Lena Novak <lena.novak@example.net>",
            "Lena,\n\nOctober's book is The Overstory. We meet at mine on the 14th \
             at seven.\n\nDana",
            None,
            tomorrow_at_eight(now),
        ),
    ];
    for (subject, to, words, problem, send_at) in letters {
        let mut draft = Draft::new(
            account_id,
            Address {
                name: Some(DISPLAY_NAME.to_string()),
                email: ACCOUNTS[0].email.to_string(),
            },
        );
        draft.to = crate::compose::parse_recipients(to);
        draft.subject = subject.to_string();
        draft.markdown = words.to_string();
        let raw = build_mime(&draft, now / 1000, "demo-queued@example.com").ok();
        let queued = Queued {
            account_id,
            subject: subject.to_string(),
            recipients: draft
                .to
                .iter()
                .map(|a| a.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
            send_at,
            raw,
            composer: serde_json::to_string(&draft).unwrap_or_default(),
            attempts: u32::from(problem.is_some()) * 3,
            problem: problem.map(str::to_string),
            ..Queued::default()
        };
        db.write(move |c| outbox::put(c, &queued)).await?;
    }
    Ok(())
}

/// Eight tomorrow morning, which is when Send Later sends the sample.
fn tomorrow_at_eight(now: EpochMillis) -> EpochMillis {
    use chrono::{Days, Local, NaiveTime, TimeZone};
    let today = Local
        .timestamp_millis_opt(now)
        .single()
        .unwrap_or_else(Local::now)
        .date_naive();
    let eight = NaiveTime::from_hms_opt(8, 0, 0).unwrap_or_default();
    let when = (today + Days::new(1)).and_time(eight);
    Local
        .from_local_datetime(&when)
        .earliest()
        .map_or(now + 86_400_000, |at| at.timestamp_millis())
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

impl SampleAccount {
    /// An empty in-memory Gmail for this account, with its identity, its
    /// labels, and the addresses it sends as.
    fn gmail(&self) -> FakeGmail {
        let system = SYSTEM_LABELS.iter().map(|&id| RemoteLabel {
            id: id.into(),
            name: id.into(),
            kind: Some("system".into()),
            color: None,
        });
        let user = self.labels.iter().map(|&(id, name, color)| RemoteLabel {
            id: id.into(),
            name: name.into(),
            kind: Some("user".into()),
            color: color.map(|c| LabelColor {
                background_color: c.into(),
                text_color: "#ffffff".into(),
            }),
        });
        let fake = FakeGmail::new();
        fake.with(|state| {
            state.email = self.email.into();
            state.display_name = Some(DISPLAY_NAME.into());
            state.signature = Some(format!("{DISPLAY_NAME}\nSent from Penguin Mail"));
            state.send_as = self
                .aliases
                .iter()
                .map(|alias| SendAs {
                    send_as_email: alias.email.into(),
                    display_name: alias.name.into(),
                    is_default: false,
                    is_primary: false,
                    signature: alias.signature.into(),
                    verification_status: Some(
                        if alias.confirmed {
                            "accepted"
                        } else {
                            "pending"
                        }
                        .into(),
                    ),
                })
                .collect();
            // The demo lists a mailbox in one page, as a Gmail search does.
            state.page_size = 1000;
            state.labels = system.chain(user).collect();
        });
        fake
    }
}

/// What the demo hands back for an attachment, since the samples name files
/// that do not exist.
fn stand_in(attachment_id: &str, mime_type: &str, size: i64) -> Vec<u8> {
    if mime_type.starts_with("image/")
        && let Some(png) = stand_in_picture(attachment_id)
    {
        return png;
    }
    // The body reads each file's size off its bytes, so the file runs to
    // the size the sample gives it and the attachment row shows that.
    let mut file =
        format!("This is {attachment_id}, a stand-in file from Penguin Mail demo mode.\n").into_bytes();
    file.resize(file.len().max(size as usize), b'\n');
    file
}

/// A picture for a demo photo, so the attachment row has something to show
/// and Quick Look has something to open. Two bands of colour chosen from
/// the attachment id, which keeps the same file the same colour.
fn stand_in_picture(attachment_id: &str) -> Option<Vec<u8>> {
    const WIDTH: i32 = 640;
    const HEIGHT: i32 = 420;
    let seed = attachment_id.bytes().fold(0u32, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as u32)
    });
    let top = [
        (0x3b, 0x6e, 0xa5),
        (0xc2, 0x6b, 0x4a),
        (0x4a, 0x8f, 0x5e),
        (0x6d, 0x53, 0x9b),
    ][seed as usize % 4];
    let bottom = (
        (top.0 as u32 * 2 / 5) as u8,
        (top.1 as u32 * 2 / 5) as u8,
        (top.2 as u32 * 2 / 5) as u8,
    );
    let pixbuf =
        gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, WIDTH, HEIGHT)?;
    let horizon = HEIGHT * 2 / 3;
    pixbuf.new_subpixbuf(0, 0, WIDTH, horizon).fill(rgba(top));
    pixbuf
        .new_subpixbuf(0, horizon, WIDTH, HEIGHT - horizon)
        .fill(rgba((bottom.0, bottom.1, bottom.2)));
    pixbuf.save_to_bufferv("png", &[]).ok()
}

fn rgba((r, g, b): (u8, u8, u8)) -> u32 {
    u32::from_be_bytes([r, g, b, 0xff])
}

impl Sample {
    /// Puts the message in its account's Gmail as it sits there once
    /// delivered, with the attachment bytes, the draft, and the calendar
    /// event Google keeps beside it.
    fn put_in(&self, fake: &FakeGmail, account_id: AccountId, now: EpochMillis) {
        let meta = self.meta(account_id, now);
        let body = self.body(now);
        fake.with(|state| {
            for attachment in &body.attachments {
                let id = attachment.attachment_id.clone().unwrap_or_default();
                // An invitation's file holds the same event as its card.
                let bytes = match (&body.calendar, attachment.mime_type.as_str()) {
                    (Some(ics), "text/calendar") => ics.clone().into_bytes(),
                    _ => stand_in(&id, &attachment.mime_type, attachment.size),
                };
                state.attachments.insert((meta.id.clone(), id.clone()), bytes);
            }
            if let Some(draft_id) = self.draft {
                state
                    .drafts
                    .insert(draft_id.into(), self.text.as_bytes().to_vec());
                state
                    .draft_messages
                    .insert(draft_id.into(), meta.id.clone());
            }
            if let Some(invite) = &self.invitation {
                // Google puts an invitation on the guest's calendar as it
                // arrives, so the demo has an event to answer.
                state.calendar.insert(invite.uid.into(), None);
                if let Some(series) = invite.series {
                    state.series.insert(invite.uid.into(), series(now));
                }
            }
            state.bodies.insert(meta.id.clone(), body);
            state.messages.insert(meta.id.clone(), meta);
        });
    }

    /// The older version of the meeting this sample moves, as the store
    /// keeps an invitation it has seen.
    fn remembered(&self, now: EpochMillis) -> Option<invitations::Saved> {
        let invite = self.invitation.as_ref()?;
        let older = invite.replaces.as_ref()?;
        let summary = mailrs_domain::invitation::read(&(invite.ics)(now))
            .map(|event| event.summary)
            .unwrap_or_default();
        Some(invitations::Saved {
            uid: invite.uid.into(),
            sequence: 0,
            starts_at: Some((older.starts)(now).timestamp_millis()),
            all_day: false,
            summary,
            cancelled: false,
            answer: None,
            message_id: older.message_id.into(),
            news: None,
            moved_from: None,
        })
    }

    /// The sample's `List-Unsubscribe` header, with the demo's page
    /// server put in where it says `{pages}`.
    fn header(&self) -> Option<String> {
        self.unsubscribe.map(at_pages)
    }

    fn meta(&self, account_id: AccountId, now: EpochMillis) -> MessageMeta {
        let me = Address {
            name: Some(DISPLAY_NAME.into()),
            email: ACCOUNTS[self.account].email.into(),
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
        let mut meta = MessageMeta {
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
            // Gmail's estimate counts each file in its base64 form, a
            // third larger than the file. That puts the roadmap's PDF and
            // the lake photos over the raw limit, and the smaller files
            // under it, so the demo opens mail by both paths.
            size: self.text.len() as i64
                + self.attachments.iter().map(|a| a.2 * 4 / 3).sum::<i64>(),
            has_attachments: !self.attachments.is_empty(),
            held: Default::default(),
            roles: vec![],
            list_unsubscribe: self.header(),
            one_click: self.one_click,
        };
        let labels: Vec<String> = self.labels.iter().map(|l| l.to_string()).collect();
        mailrs_gmail::labels::set_label_ids(&mut meta, &labels);
        meta
    }

    fn body(&self, now: EpochMillis) -> MessageBody {
        MessageBody {
            text: Some(self.text.into()),
            // A newsletter's own footer link points at the demo's pages
            // the same way a header does.
            html: self.html.map(at_pages),
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
            list_unsubscribe: self.header(),
            one_click_unsubscribe: self.one_click,
            calendar: self.invitation.as_ref().map(|invite| (invite.ics)(now)),
            provenance: Provenance {
                mailed_by: Some(sender_domain(self.from.1)),
                signed_by: Some(sender_domain(self.from.1)),
                encrypted: Some(!self.in_the_clear),
            },
            ..MessageBody::default()
        }
    }
}

/// `text` with `{pages}` replaced by the address the demo serves its
/// unsubscribe pages at. A sample cannot hold that address: the port is
/// picked when the demo opens.
fn at_pages(text: &str) -> String {
    match text.contains("{pages}") {
        true => text.replace("{pages}", pages::base()),
        false => text.to_string(),
    }
}

/// The domain a demo sender writes from.
fn sender_domain(address: &str) -> String {
    address
        .rsplit_once('@')
        .map(|(_, domain)| domain.to_string())
        .unwrap_or_default()
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
            ACCOUNTS[1].email
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
        // One Wednesday of a weekly series moves to Thursday, so the card
        // asks whether an answer covers this one or all of them.
        format!(
            "RECURRENCE-ID:{}",
            stamp(planning_was(now).with_timezone(&chrono::Utc))
        ),
        "STATUS:CONFIRMED".to_string(),
        "SUMMARY:Sprint planning".to_string(),
        "LOCATION:Meeting Room 1\\, Fernwood HQ".to_string(),
        format!("DTSTAMP:{}", stamp(sent)),
        format!("DTSTART:{}", stamp(start.with_timezone(&chrono::Utc))),
        format!("DTEND:{}", stamp(end.with_timezone(&chrono::Utc))),
        "ORGANIZER;CN=Jonas Weber:mailto:jonas@fernwood.example".to_string(),
        format!(
            "ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE;CN=Dana Reyes:mailto:{}",
            ACCOUNTS[1].email
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
/// Sprint planning runs on Wednesdays for eight weeks, two of them gone,
/// so the moved one has six of the series left from it.
fn planning_series(now: EpochMillis) -> Series {
    let week = chrono::Duration::weeks(1);
    let first = planning_was(now) - week * 2;
    let starts = (0..8)
        .map(|n| (first + week * n).timestamp_millis())
        .collect();
    ("FREQ=WEEKLY;BYDAY=WE;COUNT=8".to_string(), starts)
}

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
    use mailrs_domain::{Folder, MailSet, Role};
    use mailrs_gmail::labels as gmail;
    use mailrs_store::bodies;
    use mailrs_store::threads::{self, ThreadFilter};
    use mailrs_sync::{AccountServices, GmailApi, IdentityService, MailBackend, now_millis};

    use super::*;

    /// A demo store, its Gmail, and the time it was seeded at. The fake
    /// searches by the clock, so the samples hang off the real time.
    struct Demo {
        db: Db,
        gmail: DemoGmail,
        now: EpochMillis,
        _dir: tempfile::TempDir,
    }

    async fn demo() -> Demo {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("demo.db")).unwrap();
        let now = now_millis();
        let gmail = seed(&db, now).await.unwrap();
        Demo {
            db,
            gmail,
            now,
            _dir: dir,
        }
    }

    impl Demo {
        async fn account(&self, index: usize) -> AccountId {
            let email = ACCOUNTS[index].email;
            self.db
                .read(move |c| accounts::account_by_email(c, email))
                .await
                .unwrap()
                .expect("the demo account is stored")
                .id
        }

        async fn threads(&self, filter: ThreadFilter) -> Vec<mailrs_domain::ThreadSummary> {
            self.db
                .read(move |c| threads::list_threads(c, &filter, 0, 100))
                .await
                .unwrap()
        }

        /// Ids a search brings back from one account's demo Gmail.
        async fn found(&self, account: AccountId, query: &str) -> Vec<String> {
            self.gmail
                .account(account)
                .expect("the account has a mailbox")
                .list_messages(query, None, mailrs_sync::ID_PAGE_SIZE)
                .await
                .expect("the search runs")
                .messages
                .into_iter()
                .map(|m| m.id)
                .collect()
        }
    }

    #[tokio::test]
    async fn the_demo_contacts_have_photos_and_rank_first() {
        let demo = demo().await;
        let dir = tempfile::tempdir().unwrap();
        let photos = dir.path().to_path_buf();
        demo.db
            .write(move |c| seed_contacts(c, &photos))
            .await
            .unwrap();

        let (mara, suggestions) = demo
            .db
            .read(|c| {
                Ok((
                    address_book::find(c, "mara.okafor@example.org")?,
                    mailrs_store::contacts::suggestions(c)?,
                ))
            })
            .await
            .unwrap();
        let mara = mara.expect("Mara is in the demo address book");
        assert_eq!(mara.organization.as_deref(), Some("Ridgeline Trails"));
        let photo = dir.path().join(mara.photo_file.expect("Mara has a photo"));
        // A PNG, so the avatar can read it.
        assert_eq!(&std::fs::read(&photo).unwrap()[1..4], b"PNG");

        let known: Vec<&str> = suggestions
            .iter()
            .take_while(|s| s.known)
            .map(|s| s.email.as_str())
            .collect();
        assert_eq!(known.len(), 4, "every demo contact comes before the rest");
        assert!(known.contains(&"jonas@fernwood.example"));
    }

    #[tokio::test]
    async fn two_sent_messages_wait_for_a_reply() {
        let demo = demo().await;
        let now = demo.now;
        let waiting: Vec<String> = demo
            .db
            .read(move |c| mailrs_store::follow_ups::waiting(c, now))
            .await
            .unwrap()
            .into_iter()
            .map(|f| f.thread_id)
            .collect();
        assert_eq!(waiting, ["t-invoice", "t-lease"]);
    }

    #[tokio::test]
    async fn a_sent_reply_lands_in_sent_and_in_its_conversation() {
        let demo = demo().await;
        let work = demo.account(1).await;
        let fake = demo.gmail.account(work).unwrap();
        let (events, _) = async_channel::unbounded();
        let sync = AccountSync::new(
            work,
            AccountServices::fake(Arc::clone(&fake)),
            demo.db.clone(),
            events,
        );
        let raw = "From: Dana Reyes <dana@fernwood.example>\r\n\
            To: Priya Raman <priya@fernwood.example>\r\n\
            Subject: Re: Q4 roadmap review\r\n\
            Message-ID: <reply-1@demo.example>\r\n\
            In-Reply-To: <roadmap-3@demo.example>\r\n\
            Date: Tue, 1 Sep 2026 10:00:00 +0000\r\n\
            Content-Type: text/plain; charset=utf-8\r\n\r\n\
            October works for me.\r\n";
        // The outbox sends a reply with no thread id when the draft has
        // none, so the copy finds its conversation from In-Reply-To.
        sync.send(raw.as_bytes().to_vec(), None, None)
            .await
            .unwrap();
        sync.incremental().await.unwrap();

        let sent = demo.threads(ThreadFilter::unified(MailSet::Role(Role::Sent))).await;
        assert!(sent.iter().any(|t| t.id == "t-roadmap"), "{sent:?}");
        let body = sync.body("sent1").await.unwrap();
        assert_eq!(body.text.as_deref(), Some("October works for me.\r\n"));
        assert_eq!(body.html, None);
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

    #[tokio::test]
    async fn the_invitations_land_in_the_demo_mailbox() {
        let demo = demo().await;
        let (work, now) = (demo.account(1).await, demo.now);
        for id in ["design-review-1", "planning-1"] {
            let body = demo
                .db
                .read(move |c| bodies::peek_body(c, work, id))
                .await
                .unwrap()
                .expect("sync cached the body");
            let ics = body.calendar.expect("the message carries an invitation");
            let invitation =
                mailrs_domain::invitation::read(&ics).expect("the part holds an event");
            assert!(invitation.when.is_some(), "{id}");
            assert!(!invitation.guests.is_empty(), "{id}");
        }
        // The update in the inbox moves a meeting the demo already knows.
        let held = demo
            .db
            .read(move |c| mailrs_store::invitations::saved(c, work, MOVED_UID))
            .await
            .unwrap()
            .expect("the older version is remembered");
        assert_eq!(held.sequence, 0);
        assert_eq!(held.summary, "Sprint planning");
        assert_eq!(held.starts_at, Some(planning_was(now).timestamp_millis()));
    }

    #[tokio::test]
    async fn the_muted_mailbox_has_a_thread_the_inbox_never_sees() {
        let demo = demo().await;
        let muted = demo.threads(ThreadFilter::unified(MailSet::muted())).await;
        assert_eq!(muted.len(), 1);
        assert_eq!(muted[0].id, "t-lab-move");
        assert!(muted[0].muted);
        assert_eq!(muted[0].message_count, 2);
        let inbox = demo
            .threads(ThreadFilter::unified(MailSet::Role(Role::Inbox)))
            .await;
        assert!(inbox.iter().all(|t| t.id != "t-lab-move"));
    }

    #[tokio::test]
    async fn every_inbox_category_has_demo_mail() {
        let demo = demo().await;
        for labels in [
            &["CATEGORY_UPDATES"][..],
            &["CATEGORY_PROMOTIONS"],
            &["CATEGORY_SOCIAL", "CATEGORY_FORUMS"],
        ] {
            let filter =
                ThreadFilter::unified(MailSet::Role(Role::Inbox)).with_categories(labels, &[]);
            let count = demo
                .db
                .read(move |c| threads::count_threads(c, &filter))
                .await
                .unwrap();
            assert!(count >= 2, "{labels:?}");
        }
    }

    #[tokio::test]
    async fn the_demo_store_has_a_lively_unified_inbox() {
        let demo = demo().await;
        let inbox = ThreadFilter::unified(MailSet::Role(Role::Inbox));
        let threads = demo.threads(inbox.clone()).await;
        assert!(threads.len() >= 10, "{}", threads.len());
        let unread = demo
            .db
            .read(move |c| threads::unread_threads(c, &inbox))
            .await
            .unwrap();
        assert!(unread >= 3);
        assert_eq!(threads[0].id, "t-hike");
        let accounts_seen: std::collections::HashSet<_> =
            threads.iter().map(|t| t.account_id).collect();
        assert_eq!(accounts_seen.len(), 3);
        assert_eq!(
            demo.threads(ThreadFilter::unified(MailSet::Role(Role::Drafts)))
                .await
                .len(),
            1
        );
    }

    /// Sync stores every sample but the ones in Junk and Trash, which Gmail
    /// leaves out of the window, and caches each stored body.
    #[tokio::test]
    async fn sync_stores_every_sample_outside_junk_and_trash() {
        let demo = demo().await;
        for sample in samples() {
            let account = demo.account(sample.account).await;
            let id = sample.id;
            let (thread, body) = demo
                .db
                .read(move |c| {
                    Ok((
                        mailrs_store::messages::thread_id_of(c, account, id)?,
                        bodies::peek_body(c, account, id)?,
                    ))
                })
                .await
                .unwrap();
            let hidden = sample
                .labels
                .iter()
                .any(|l| [gmail::SPAM, gmail::TRASH].contains(l));
            assert_eq!(thread.is_some(), !hidden, "{id}");
            assert_eq!(body.is_some(), !hidden, "{id}");
        }
    }

    #[tokio::test]
    async fn the_folders_come_from_the_demo_gmail() {
        let demo = demo().await;
        let account = demo.account(0).await;
        let text = |folder: Folder| mailrs_gmail::query::print(&folder.query());
        assert_eq!(demo.found(account, &text(Folder::Junk)).await, ["prize-1"]);
        assert_eq!(
            demo.found(account, &text(Folder::Trash)).await,
            ["webinar-1"]
        );
        let all = demo.found(account, &text(Folder::AllMail)).await;
        assert!(!all.contains(&"prize-1".to_string()));
        assert!(!all.contains(&"webinar-1".to_string()));
        assert!(all.contains(&"hike-1".to_string()));
        // What the search bar sends: plain words across sender and subject.
        assert_eq!(demo.found(account, "sunrise").await, ["lake-1"]);
    }

    #[tokio::test]
    async fn attachments_and_the_sample_draft_come_from_the_demo_gmail() {
        let demo = demo().await;
        let work = demo.account(1).await;
        let api = demo.gmail.account(work).expect("the account has a mailbox");
        let listed = api.list_drafts().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].draft_id, DRAFT_ID);
        assert_eq!(listed[0].message_id, "draft-1");
        let file = AccountServices::fake(Arc::clone(&api))
            .mail
            .fetch_part("roadmap-1", &mailrs_sync::fake::attachment_path(0))
            .await
            .unwrap();
        assert!(String::from_utf8(file).unwrap().contains("stand-in file"));
        let identities = AccountServices::fake(Arc::clone(&api))
            .identities
            .identities()
            .await
            .unwrap();
        let signature = identities
            .iter()
            .find(|address| address.default)
            .map(|address| address.signature.clone());
        assert_eq!(
            signature,
            Some(format!("{DISPLAY_NAME}\nSent from Penguin Mail"))
        );
        let from: Vec<String> = api
            .send_as()
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.send_as_email)
            .collect();
        assert_eq!(
            from,
            [
                ACCOUNTS[1].email,
                "hello@fernwood.example",
                "press@fernwood.example"
            ]
        );
    }
}
