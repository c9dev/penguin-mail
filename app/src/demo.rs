//! Sample mail for `penguin-mail --demo`: four accounts and a few weeks of
//! conversations. Every address uses a reserved `.example` domain.
//!
//! Three accounts get a `FakeGmail` holding their mail, and the fourth, on
//! Fastmail, gets a `FakeImap`, so the demo shows a folder account too.
//! Sync's own first sync against each fills a throwaway store, so the demo
//! opens on a full inbox that holds what a real account's store would.
//! From there the demo runs the same code as a real account.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::calendar::{
    Access, Calendar as CalendarModel, Event as CalendarEvent, Guest as CalendarGuest, Reminder, ReminderMethod,
    Status as CalendarStatus,
};
use mailrs_domain::invitation::Answer;
use mailrs_domain::{
    AccountId, Address, Attachment, EpochMillis, Filter, FilterAction, FilterCriteria, MailSet,
    MessageBody, MessageMeta, Provenance, Role,
};
use mailrs_gmail::{LabelColor, RemoteLabel, SendAs};
use mailrs_store::servers::{self, Saved, Security, Servers};
use mailrs_store::{Db, Result, StoreError, accounts, address_book, invitations};
use mailrs_sync::calendar_copy::CalendarCopy;
use mailrs_sync::fake::{DavKind, FakeDav, FakeGmail, FakeGraph, FakeImap, FakeSmtp, fill_store};
use mailrs_store::services::{FoundService, ServiceKind};
use mailrs_sync::{
    AccountServices, AccountSync, AnyCalendar, AnyContacts, AnyRules, CalDav, CardDav, ContactBook, DEFAULT_WINDOW_DAYS,
    Imap, ImapSettings, LocalRules, SyncError,
};
use rusqlite::Connection;

pub mod folder;
mod outlook;
mod pop3;
mod pages;

/// The id of the draft behind the sample draft message, as Gmail would hold it.
const DRAFT_ID: &str = "demo-draft";

/// The booking behind the sample ticket. Its two events add `-out` and
/// `-back` to it.
const TICKET_UID: &str = "cp-88213@cp.example";

/// The event behind the sample invitation, as Google would write it.
const INVITE_UID: &str = "7f3k2q9demo1invite@google.com";

/// The event the sample update moves. The demo remembers an older version
/// of it, so opening the update says what changed.
const MOVED_UID: &str = "2b8h5x0demo2moved@google.com";

/// The zone every demo calendar and timed event is written in.
const LISBON: &str = "Europe/Lisbon";

/// The second demo account's shared calendar, beyond its own primary.
const DESIGN_TEAM: &str = "design-team";

/// The first demo account's subscribed, read-only calendar, beyond its
/// own primary.
const HOLIDAYS_PT: &str = "holidays-pt";

/// The first demo account's own second calendar, for its non-work life.
const FAMILY: &str = "family";

/// The second demo account's calendar it only reads, and keeps out of
/// the merged view: the mockup's "Marketing", read-only and hidden.
const MARKETING: &str = "marketing";

/// Sprint planning's video call link, on both its series and its moved
/// occurrence.
const SPRINT_PLANNING_LINK: &str = "https://meet.google.com/fernwood-sprint";

/// Quarterly review's video call link.
const QUARTERLY_REVIEW_LINK: &str = "https://meet.google.com/fernwood-quarterly";

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
        // Nested a level deep, so the sidebar shows labels inside labels
        // and a drag has somewhere to nest one.
        labels: &[
            ("Label_personal", "Personal", Some("#16a766")),
            ("Label_bills", "Personal/bills", None),
            ("Label_important", "Personal/Important", None),
            ("Label_work", "Work", Some("#4a86e8")),
            ("Label_bugs", "Work/bugs", None),
        ],
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
    /// Whether Google already holds the event, so an answer finds it. A
    /// ticket the sender only published is on no calendar until added.
    answerable: bool,
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

/// The Client workshop's notes: an agenda and a video-call footer long
/// enough to need "Show more" and then to scroll.
const CLIENT_WORKSHOP_NOTES: &str = "Agenda:\n1. Where the pilot stands\n2. What the client saw in week one\n3. Changes to the rollout\n4. Training for the support team\n5. Next steps and owners\n\nPlease read the pilot report before the call.\n\n________________________________________\nVideo call\nJoin: https://meet.example.com/client-workshop\nMeeting ID: 482 193 775 204\nPasscode: 5KQ2\n\nDial in by phone\n+351 21 000 0000, Lisbon\n+44 20 0000 0000, London\nPhone conference ID: 918 227 441#\n\nFind a local number\nReset dial-in PIN\n\nFor organizers: Meeting options\n________________________________________";

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
            text: "Dana, can you sanity-check Jonas's estimate before Friday? If last-write-wins is acceptable to support, I'm happy to commit to October.\n\n-- \nPriya Raman\nHead of Product, Fernwood\n\nOn Tue, 22 Sept 2026 at 14:10, Jonas Weber <jonas@fernwood.example> wrote:\n> Left my comments. Short version: October is possible if we cut sync\n> conflict resolution down to last-write-wins for the first release.\n>\n> > Could you each leave comments by Thursday?\n>\n> Jonas",
            // A reply as Gmail writes it, so the page has quoted history to
            // fold away.
            html: Some(
                r#"<div dir="ltr">Dana, can you sanity-check Jonas's estimate before Friday? If last-write-wins is acceptable to support, I'm happy to commit to October.<br><br><div class="gmail_signature">Priya Raman<br>Head of Product, Fernwood</div></div><br><div class="gmail_quote gmail_quote_container"><div dir="ltr" class="gmail_attr">On Tue, 22 Sept 2026 at 14:10, Jonas Weber &lt;<a href="mailto:jonas@fernwood.example">jonas@fernwood.example</a>&gt; wrote:<br></div><blockquote class="gmail_quote" style="margin:0px 0px 0px 0.8ex;border-left:1px solid rgb(204,204,204);padding-left:1ex"><div dir="ltr">Left my comments. Short version: October is possible if we cut sync conflict resolution down to last-write-wins for the first release.<br><br><blockquote class="gmail_quote" style="margin:0px 0px 0px 0.8ex;border-left:1px solid rgb(204,204,204);padding-left:1ex">Could you each leave comments by Thursday?</blockquote><br>Jonas</div></blockquote></div>"#,
            ),
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
            account: 1,
            thread: "t-ci",
            id: "ci-1",
            from: ("Fernwood CI", "notifications@github.example"),
            to: &[ME],
            subject: "[fernwood/kite-app] Run failed: CI - main (0a1b2c3)",
            // Old enough to stay below the threads that the demo's tests
            // and the store screenshots expect at the top.
            minutes_ago: 11 * DAY,
            labels: &["INBOX"],
            text: "CI workflow run failed for main branch\n\nCI / Tests\nFailed in 6 minutes and 43 seconds",
            // A notification in GitHub's shape: its font and line height
            // sit on <body>, and the annotation count beside its icon is
            // a table cell inside a link.
            html: Some(include_str!("demo/github-ci.html")),
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
            thread: "t-train-ticket",
            id: "train-ticket-1",
            from: ("CP Comboios", "bilhetes@cp.example"),
            to: &[ME],
            subject: "Your tickets: Lisboa to Porto, Friday and Sunday",
            minutes_ago: 25,
            labels: &["INBOX", "UNREAD"],
            text: "Your booking is confirmed. The calendar file attached holds both journeys.\n\nShow the QR code in the app when you board.",
            attachments: &[("tickets.ics", "text/calendar", 1_106)],
            invitation: Some(Invite {
                uid: TICKET_UID,
                ics: ticket_ics,
                replaces: None,
                series: None,
                answerable: false,
            }),
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
                answerable: true,
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
                answerable: true,
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

/// What the demo's accounts talk to: an in-memory Gmail for each Gmail
/// sample account and an in-memory IMAP server for the Fastmail one. The
/// demo keeps them for as long as the app runs, so rules, hidden
/// addresses, and automatic replies made in the demo survive a sync
/// restart.
pub struct DemoMail(HashMap<AccountId, DemoServer>);

enum DemoServer {
    Gmail(Arc<FakeGmail>),
    Imap {
        imap: Arc<FakeImap>,
        smtp: Arc<FakeSmtp>,
        dav: Arc<FakeDav>,
        db: Db,
    },
    Microsoft(Arc<FakeGraph>),
    Pop3 { server: pop3::Pop3Server, db: Db },
}

impl DemoMail {
    /// A demo account's services, built the way a real account's are.
    pub fn services(&self, account_id: AccountId) -> Option<AccountServices> {
        Some(match self.0.get(&account_id)? {
            DemoServer::Gmail(gmail) => AccountServices::fake(Arc::clone(gmail)),
            DemoServer::Imap { imap, smtp, dav, db } => imap_services(imap, smtp, dav, db, account_id),
            DemoServer::Microsoft(graph) => {
                AccountServices::fake_microsoft_with(Arc::clone(graph), outlook::settings())
            }
            DemoServer::Pop3 { server, db } => server.services(db, account_id),
        })
    }

    /// The Gmail behind a demo Gmail account.
    #[cfg(test)]
    fn gmail(&self, account_id: AccountId) -> Option<Arc<FakeGmail>> {
        match self.0.get(&account_id)? {
            DemoServer::Gmail(gmail) => Some(Arc::clone(gmail)),
            DemoServer::Imap { .. } | DemoServer::Microsoft(_) | DemoServer::Pop3 { .. } => None,
        }
    }
}

/// Adds the demo accounts to an empty store, puts the samples in each
/// one's server, and lets sync fill the store from there. Each Gmail body
/// the store can hold is read once through sync's cache, so opening a
/// sample asks Gmail nothing and the invitation cards have their events.
pub async fn seed(db: &Db, now: EpochMillis) -> std::result::Result<DemoMail, SyncError> {
    let samples = samples();
    let mut mail = HashMap::new();
    // Kept so a `CalendarCopy` can read each Gmail account's calendars
    // once every sample and event is in its fake, before the window
    // ever opens.
    let mut syncing: HashMap<AccountId, Arc<AccountSync>> = HashMap::new();
    let mut first_account = None;
    for (index, account) in ACCOUNTS.iter().enumerate() {
        let email = account.email;
        let account_id = db
            .write(move |c| accounts::insert_account(c, email, now))
            .await?;
        let fake = Arc::new(account.gmail());
        fake.keep_sent_copies(account_id);
        if index == 2 && std::env::var_os("MAILRS_DEMO_FULL_CONSENT").is_none() {
            // Shows the Grant Access banner: this account was never asked
            // for the settings scope, the way an account added before
            // sign-in asked for every scope at once never was. gmail.settings.basic covers
            // nothing else and nothing covers it, so withholding it alone
            // is unambiguous.
            // `MAILRS_DEMO_FULL_CONSENT` grants it, for screenshots that are
            // not about the banner.
            fake.withhold(mailrs_gmail::SETTINGS_SCOPE);
        }
        let mine: Vec<&Sample> = samples.iter().filter(|s| s.account == index).collect();
        for sample in &mine {
            sample.put_in(&fake, account_id, now);
        }
        fake.with(|s| s.calendars = demo_calendars(index));
        fake.with(|s| s.filters = demo_rules(index));
        for event in demo_events(index, now) {
            fake.put_calendar_event(event);
        }
        // Nobody listens yet: the window reads the store once it opens.
        let (events, _) = async_channel::unbounded();
        let sync = Arc::new(AccountSync::new(
            account_id,
            AccountServices::fake(Arc::clone(&fake)),
            db.clone(),
            events,
        ));
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
            first_account = Some(account_id);
        }
        syncing.insert(account_id, Arc::clone(&sync));
        mail.insert(account_id, DemoServer::Gmail(fake));
    }
    let (account_id, server, sync) = seed_fastmail(db, now).await?;
    // The Fastmail account's sync is built over plain mail services, so
    // the copy and the address book read its calendar and contacts from
    // the demo's own.
    let mut own_services = HashMap::new();
    if let DemoServer::Imap { imap, smtp, dav, db } = &server {
        own_services.insert(account_id, imap_services(imap, smtp, dav, db, account_id));
    }
    syncing.insert(account_id, sync);
    mail.insert(account_id, server);
    let (outlook_id, graph, sync) = outlook::seed_outlook(db, now).await?;
    syncing.insert(outlook_id, sync);
    mail.insert(outlook_id, DemoServer::Microsoft(graph));
    let (pop3_id, server, sync) = pop3::seed_pop3(db, now).await?;
    syncing.insert(pop3_id, sync);
    mail.insert(pop3_id, DemoServer::Pop3 { server, db: db.clone() });

    // Reads each account's calendars into the store now, so the demo
    // opens already synced: the assistant and the invitation card's clash
    // line read the copy from the first screen.
    let accounts_synced: Vec<AccountId> = syncing.keys().copied().collect();
    let seeding = Arc::new(Seeding { syncing, own_services });
    let copy = CalendarCopy::new(Arc::clone(&seeding), db.clone());
    for account_id in accounts_synced {
        copy.refresh(account_id, now).await?;
    }
    // The demo's cards carry no photos, so the address book writes none
    // and the photo folder is never touched.
    let book = ContactBook::new(seeding, db.clone(), std::env::temp_dir());
    book.refresh(account_id).await?;
    book.refresh(outlook_id).await?;
    // Parents' evening is a change made on this computer and not sent
    // yet, as the mockup draws it. It goes straight into the copy with no
    // queued change, so the demo never sends it and it stays waiting;
    // the copy's sweep keeps a waiting row.
    if let Some(account_id) = first_account {
        let monday = week_monday(now);
        let waiting = CalendarEvent {
            pending: true,
            ..timed_event(
                FAMILY,
                "parents-evening",
                "Parents' evening",
                at_week(monday, 3, 17, 0),
                at_week(monday, 3, 18, 0),
            )
        };
        db.write(move |c| mailrs_store::calendar::save_events(c, account_id, &[waiting], now))
            .await?;
    }

    Ok(DemoMail(mail))
}

/// The accounts `seed` builds, so a `CalendarCopy` can read their
/// calendars before `Core` has an engine of its own to build one over.
/// `mailrs_sync::fake::Connected`-alike test harnesses live in `sync`'s
/// own test module and are not reachable from here, so `seed` keeps this
/// small one instead.
struct Seeding {
    syncing: HashMap<AccountId, Arc<AccountSync>>,
    /// Accounts whose services hold more than their sync's do: the
    /// Fastmail account's calendar, contacts and rules.
    own_services: HashMap<AccountId, AccountServices>,
}

impl mailrs_sync::Accounts for Seeding {
    fn account(&self, account_id: AccountId) -> Option<Arc<AccountSync>> {
        self.syncing.get(&account_id).cloned()
    }

    fn services(&self, account_id: AccountId) -> Option<AccountServices> {
        self.own_services
            .get(&account_id)
            .cloned()
            .or_else(|| self.account(account_id).map(|sync| sync.services().clone()))
    }
}

/// The rules demo account `index` has at Gmail. The Fernwood account
/// gets two the Rules form shows whole and one made in Gmail's own
/// settings, whose forward the form has no field for.
fn demo_rules(index: usize) -> Vec<Filter> {
    if index != 1 {
        return Vec::new();
    }
    let from = |address: &str| FilterCriteria {
        from: Some(address.into()),
        ..FilterCriteria::default()
    };
    vec![
        Filter {
            id: Some("demo-rule-post".into()),
            criteria: from("tracking@packetpost.example"),
            action: FilterAction {
                remove: vec![MailSet::Role(Role::Inbox), MailSet::Unseen],
                ..FilterAction::default()
            },
            ..Filter::default()
        },
        Filter {
            id: Some("demo-rule-tickets".into()),
            criteria: FilterCriteria {
                subject: Some("tickets".into()),
                ..from("tickets@hollowpines.example")
            },
            action: FilterAction {
                add: vec![MailSet::Mailbox("Label_travel".into()), MailSet::flagged()],
                ..FilterAction::default()
            },
            ..Filter::default()
        },
        Filter {
            id: Some("demo-rule-bank".into()),
            criteria: FilterCriteria {
                query: Some("statement".into()),
                ..from("statements@juniper.example")
            },
            action: FilterAction {
                add: vec![MailSet::Mailbox("Label_clients".into())],
                remove: vec![MailSet::Role(Role::Inbox)],
                forward: Some("books@fernwood.example".into()),
            },
            ..Filter::default()
        },
    ]
}

/// The calendars demo account `index` keeps, its own primary always
/// among them. Split across the three accounts so the merged view shows
/// each event once, and named and coloured as the approved mockup's
/// sidebar draws them (`calendar-mockup/mockups.py`).
fn demo_calendars(index: usize) -> Vec<CalendarModel> {
    match index {
        0 => vec![
            CalendarModel {
                id: "primary".into(),
                name: "Personal".into(),
                color: "#e8660c".into(),
                access: Access::Owner,
                zone: LISBON.into(),
                primary: true,
                shown: true,
                hidden: false,
                // A common choice on Google's side; lets a run of the
                // demo show a reminder for the call with Rita below.
                reminders: vec![Reminder { minutes: 10, method: ReminderMethod::Notification }],
            },
            CalendarModel {
                id: FAMILY.into(),
                name: "Family".into(),
                color: "#2ec27e".into(),
                access: Access::Owner,
                zone: LISBON.into(),
                primary: false,
                shown: true,
                hidden: false,
                reminders: Vec::new(),
            },
            CalendarModel {
                id: HOLIDAYS_PT.into(),
                name: "Holidays in Portugal".into(),
                color: "#e01b24".into(),
                access: Access::Reader,
                zone: LISBON.into(),
                primary: false,
                shown: true,
                hidden: false,
                reminders: Vec::new(),
            },
        ],
        1 => vec![
            CalendarModel {
                id: "primary".into(),
                name: "Work".into(),
                color: "#3584e4".into(),
                access: Access::Owner,
                zone: LISBON.into(),
                primary: true,
                shown: true,
                hidden: false,
                reminders: Vec::new(),
            },
            CalendarModel {
                id: DESIGN_TEAM.into(),
                name: "Design team".into(),
                color: "#9141ac".into(),
                access: Access::Writer,
                zone: LISBON.into(),
                primary: false,
                shown: true,
                hidden: false,
                reminders: Vec::new(),
            },
            CalendarModel {
                id: MARKETING.into(),
                name: "Marketing".into(),
                color: "#e5a50a".into(),
                access: Access::Reader,
                zone: LISBON.into(),
                primary: false,
                shown: false,
                hidden: false,
                reminders: Vec::new(),
            },
        ],
        _ => vec![CalendarModel {
            id: "primary".into(),
            name: "Personal".into(),
            color: "#e8660c".into(),
            access: Access::Owner,
            zone: LISBON.into(),
            primary: true,
            shown: true,
            hidden: false,
            reminders: Vec::new(),
        }],
    }
}

/// A one-off, busy, confirmed event, the shape most of the week's sample
/// events take before a row overrides one field or two.
fn timed_event(calendar: &str, id: &str, title: &str, start: EpochMillis, end: EpochMillis) -> CalendarEvent {
    CalendarEvent {
        calendar: calendar.into(),
        id: id.into(),
        uid: format!("{id}@local"),
        start,
        end,
        zone: LISBON.into(),
        title: title.into(),
        busy: true,
        status: CalendarStatus::Confirmed,
        // Read from the provider, files and all: an empty list, which the
        // editor may add to.
        attachments: Some(Vec::new()),
        ..CalendarEvent::default()
    }
}

/// A Drive file on a sample event. The link goes nowhere real.
fn drive_file(title: &str, mime_type: &str, id: &str) -> mailrs_domain::calendar::Attachment {
    mailrs_domain::calendar::Attachment {
        title: title.into(),
        file_url: format!("https://drive.google.com/file/d/{id}/view"),
        mime_type: mime_type.into(),
        icon_link: String::new(),
        file_id: id.into(),
        ..mailrs_domain::calendar::Attachment::default()
    }
}

/// The Monday, midnight local, the demo's week of events hangs off: the
/// Monday inside the week the calendar shows for `now`. The calendar's
/// week can start on Sunday, and on a Sunday the ISO week's Monday lies
/// in the week before the one on screen, which left that week empty.
fn week_monday(now: EpochMillis) -> chrono::DateTime<chrono::Local> {
    use chrono::TimeZone;
    let from = chrono::DateTime::from_timestamp_millis(now)
        .unwrap_or_default()
        .with_timezone(&chrono::Local);
    let start = mailrs_domain::calendar::week::week_start(
        mailrs_domain::calendar::week::WeekStart::Automatic,
        crate::locale_time::first_weekday(),
    );
    demo_monday(from.date_naive(), start)
        .and_hms_opt(0, 0, 0)
        .and_then(|at| chrono::Local.from_local_datetime(&at).earliest())
        .unwrap_or(from)
}

/// The Monday inside the week that starts on `start` and holds `today`.
fn demo_monday(today: chrono::NaiveDate, start: chrono::Weekday) -> chrono::NaiveDate {
    use chrono::Datelike;
    let into_week = (today.weekday().num_days_from_monday() + 7 - start.num_days_from_monday()) % 7;
    let first = today - chrono::Days::new(u64::from(into_week));
    let to_monday = (7 - start.num_days_from_monday()) % 7;
    first + chrono::Days::new(u64::from(to_monday))
}

/// `hour:minute` local, `day_offset` days after `monday`.
fn at_week(monday: chrono::DateTime<chrono::Local>, day_offset: i64, hour: u32, minute: u32) -> EpochMillis {
    use chrono::TimeZone;
    let day = monday.date_naive() + chrono::Duration::days(day_offset);
    day.and_hms_opt(hour, minute, 0)
        .and_then(|at| chrono::Local.from_local_datetime(&at).earliest())
        .map(|at| at.timestamp_millis())
        .unwrap_or_else(|| monday.timestamp_millis())
}

/// The neutral span of an all-day event starting on `date` and running
/// `days` of them: midnight UTC of `date` to midnight UTC `days` later.
fn all_day_utc(date: chrono::NaiveDate, days: i64) -> (EpochMillis, EpochMillis) {
    let start = date.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc().timestamp_millis();
    let end = (date + chrono::Duration::days(days))
        .and_hms_opt(0, 0, 0)
        .unwrap_or_default()
        .and_utc()
        .timestamp_millis();
    (start, end)
}

/// The first demo account's week: Personal for its own doings, Family
/// for Ana's birthday and the two family outings, and Holidays in
/// Portugal for the one public holiday near `now`. Titles, times and
/// calendars follow the approved mockup (`calendar-mockup/mockups.py`),
/// for the week it draws (Monday to Sunday).
fn account0_events(now: EpochMillis) -> Vec<CalendarEvent> {
    use chrono::Datelike;
    let monday = week_monday(now);
    let mut events = vec![
        CalendarEvent {
            organizer: Some(ACCOUNTS[0].email.into()),
            my_answer: Some(Answer::Yes),
            guests: vec![
                CalendarGuest {
                    email: ACCOUNTS[0].email.into(),
                    organizer: true,
                    me: true,
                    answer: Some(Answer::Yes),
                    ..CalendarGuest::default()
                },
                CalendarGuest {
                    email: "ana.reyes@example.com".into(),
                    name: Some("Ana Reyes".into()),
                    answer: Some(Answer::Yes),
                    ..CalendarGuest::default()
                },
            ],
            attachments: Some(vec![
                // Uploaded from this computer, so the guests can open it.
                mailrs_domain::calendar::Attachment {
                    share: Some(true),
                    shared_with: vec!["ana.reyes@example.com".into()],
                    ..drive_file("Restaurant menu.pdf", "application/pdf", "pmdemo-menu")
                },
                drive_file("Summer trip budget", "application/vnd.google-apps.spreadsheet", "pmdemo-budget"),
            ]),
            ..timed_event("primary", "lunch-with-ana", "Lunch with Ana", at_week(monday, 1, 13, 0), at_week(monday, 1, 14, 0))
        },
        timed_event("primary", "dentist", "Dentist", at_week(monday, 2, 11, 0), at_week(monday, 2, 12, 0)),
        timed_event("primary", "gym", "Gym", at_week(monday, 1, 16, 0), at_week(monday, 1, 17, 0)),
        timed_event("primary", "yoga", "Yoga", at_week(monday, 9, 18, 30), at_week(monday, 9, 19, 30)),
        timed_event(
            FAMILY,
            "swimming-lessons",
            "Swimming lessons",
            at_week(monday, 5, 10, 0),
            at_week(monday, 5, 12, 0),
        ),
        timed_event(
            FAMILY,
            "family-lunch",
            "Family lunch",
            at_week(monday, 6, 13, 0),
            at_week(monday, 6, 15, 30),
        ),
    ];
    let (birthday_start, birthday_end) = all_day_utc(monday.date_naive(), 1);
    events.push(CalendarEvent {
        calendar: FAMILY.into(),
        id: "anas-birthday".into(),
        uid: "anas-birthday@local".into(),
        start: birthday_start,
        end: birthday_end,
        zone: "UTC".into(),
        all_day: true,
        title: "Ana's birthday".into(),
        busy: false,
        status: CalendarStatus::Confirmed,
        rules: vec!["RRULE:FREQ=YEARLY".into()],
        kind: mailrs_domain::calendar::Kind::Birthday,
        ..CalendarEvent::default()
    });
    // Three days over next weekend into the Monday after, so Month
    // draws one bar that carries on into the next week row.
    let (trip_start, trip_end) = all_day_utc(monday.date_naive() + chrono::Duration::days(12), 3);
    events.push(CalendarEvent {
        calendar: FAMILY.into(),
        id: "porto-weekend".into(),
        uid: "porto-weekend@local".into(),
        start: trip_start,
        end: trip_end,
        zone: "UTC".into(),
        all_day: true,
        title: "Porto weekend".into(),
        busy: true,
        status: CalendarStatus::Confirmed,
        ..CalendarEvent::default()
    });
    let year = monday.year();
    let republic_day = chrono::NaiveDate::from_ymd_opt(year, 10, 5).unwrap_or_else(|| monday.date_naive());
    let (start, end) = all_day_utc(republic_day, 1);
    events.push(CalendarEvent {
        calendar: HOLIDAYS_PT.into(),
        id: "implantacao-da-republica".into(),
        uid: "implantacao-da-republica@local".into(),
        start,
        end,
        zone: "UTC".into(),
        all_day: true,
        title: "Implantação da República".into(),
        busy: false,
        status: CalendarStatus::Confirmed,
        rules: vec!["RRULE:FREQ=YEARLY".into()],
        ..CalendarEvent::default()
    });
    // Ten to fifteen minutes after the demo starts, so a run of it puts
    // a reminder up within five minutes.
    let call = now - now.rem_euclid(5 * 60_000) + 15 * 60_000;
    events.push(CalendarEvent {
        calendar: "primary".into(),
        id: "democallrita".into(),
        uid: "democallrita@google.com".into(),
        start: call,
        end: call + 30 * 60_000,
        zone: LISBON.into(),
        title: "Call with Rita".into(),
        busy: true,
        status: CalendarStatus::Confirmed,
        conference: Some("https://meet.google.com/pmd-demo-call".into()),
        ..CalendarEvent::default()
    });
    // Five to ten minutes after the demo starts, sooner than the call
    // with Rita above, so the next-event card at the foot of the mail
    // sidebar has something to show whenever the demo starts. Coloured
    // like the mockup's own card (blue) rather than Personal's orange,
    // so the screenshot reads the same way.
    let soon = now - now.rem_euclid(5 * 60_000) + 10 * 60_000;
    events.push(CalendarEvent {
        color: Some("#3584e4".into()),
        ..timed_event("primary", "demo-next-event", "Sprint planning", soon, soon + 30 * 60_000)
    });
    events.extend(two_years_ago(now));
    events
}

/// A few events in the week two years back, which the copy's first read
/// (a year back) leaves out. Going there fetches them from the demo's
/// Google, as it does from the real one.
fn two_years_ago(now: EpochMillis) -> Vec<CalendarEvent> {
    let back = chrono::DateTime::from_timestamp_millis(now)
        .unwrap_or_default()
        .with_timezone(&chrono::Local)
        .checked_sub_months(chrono::Months::new(24))
        .map_or(now, |at| at.timestamp_millis());
    let monday = week_monday(back);
    vec![
        timed_event("primary", "old-planning", "Quarter planning", at_week(monday, 0, 10, 0), at_week(monday, 0, 12, 0)),
        timed_event("primary", "old-lunch", "Lunch with Rui", at_week(monday, 1, 13, 0), at_week(monday, 1, 14, 0)),
        timed_event("primary", "old-review", "Design review", at_week(monday, 2, 15, 0), at_week(monday, 2, 16, 30)),
        timed_event(FAMILY, "old-recital", "Piano recital", at_week(monday, 3, 18, 0), at_week(monday, 3, 19, 30)),
        timed_event("primary", "old-run", "Morning run", at_week(monday, 4, 7, 30), at_week(monday, 4, 8, 30)),
    ]
}

/// The second demo account's week: the weekday Stand-up and the rest of
/// its own doings on Work, the Design team's own meetings, and, on the
/// Work calendar, the two invitation-linked events under the same UIDs
/// the sample mail carries, with lunch and a retro either side of the
/// design review for its card's day strip. The invitations' events sit there alone, so the
/// week holds no second "Sprint planning". Titles, times and calendars
/// otherwise follow the approved mockup (`calendar-mockup/mockups.py`),
/// for the week it draws (Monday to Sunday).
fn account1_events(now: EpochMillis) -> Vec<CalendarEvent> {
    let monday = week_monday(now);

    let standup_start = at_week(monday, 0, 9, 30);
    let mut events = vec![
        CalendarEvent {
            rules: vec!["RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR".into()],
            ..timed_event("primary", "standup", "Stand-up", standup_start, at_week(monday, 0, 9, 45))
        },
        timed_event(
            DESIGN_TEAM,
            "design-team-meeting",
            "Design review",
            at_week(monday, 0, 11, 0),
            at_week(monday, 0, 12, 0),
        ),
        timed_event(
            "primary",
            "one-on-one-rita",
            "1:1 with Rita",
            at_week(monday, 0, 14, 0),
            at_week(monday, 0, 15, 30),
        ),
        // Long notes, as a video-call invitation carries, so the popover
        // shows "Show more" and scrolls once they open whole.
        CalendarEvent {
            description: CLIENT_WORKSHOP_NOTES.into(),
            ..timed_event(
                "primary",
                "client-workshop",
                "Client workshop",
                at_week(monday, 3, 11, 0),
                at_week(monday, 3, 13, 0),
            )
        },
        timed_event(DESIGN_TEAM, "retro", "Retro", at_week(monday, 4, 16, 30), at_week(monday, 4, 17, 30)),
        // Next Wednesday is the busiest day, so a short window folds it
        // into an "N more" button in Month. It sits outside the week the
        // mockup draws.
        timed_event("primary", "sprint-review", "Sprint review", at_week(monday, 9, 11, 0), at_week(monday, 9, 12, 0)),
        timed_event(DESIGN_TEAM, "design-sync", "Design sync", at_week(monday, 9, 14, 0), at_week(monday, 9, 15, 0)),
        timed_event("primary", "hiring-panel", "Hiring panel", at_week(monday, 9, 16, 0), at_week(monday, 9, 17, 0)),
    ];
    events.extend(status_entries(monday));
    let (offsite_start, offsite_end) = all_day_utc(monday.date_naive() + chrono::Duration::days(3), 2);
    events.push(CalendarEvent {
        calendar: "primary".into(),
        id: "lisbon-offsite".into(),
        uid: "lisbon-offsite@local".into(),
        start: offsite_start,
        end: offsite_end,
        zone: "UTC".into(),
        all_day: true,
        title: "Lisbon offsite".into(),
        busy: true,
        status: CalendarStatus::Confirmed,
        ..CalendarEvent::default()
    });

    // The sprint planning series the sample mail's update moved, and its
    // moved occurrence, on the primary calendar rather than "Design
    // team", so it agrees with the invitation reaching the guest's own
    // calendar.
    let (rule, starts) = planning_series(now);
    let sprint_start = *starts.first().unwrap_or(&at_week(monday, 2, 10, 0));
    events.push(CalendarEvent {
        calendar: "primary".into(),
        id: "sprint-planning".into(),
        uid: MOVED_UID.into(),
        start: sprint_start,
        end: sprint_start + 90 * 60_000,
        zone: LISBON.into(),
        title: "Sprint planning".into(),
        busy: true,
        status: CalendarStatus::Confirmed,
        rules: vec![format!("RRULE:{rule}")],
        conference: Some(SPRINT_PLANNING_LINK.into()),
        ..CalendarEvent::default()
    });
    let moved_start = planning_is(now).timestamp_millis();
    events.push(CalendarEvent {
        calendar: "primary".into(),
        id: "sprint-planning-moved".into(),
        uid: MOVED_UID.into(),
        start: moved_start,
        end: moved_start + 90 * 60_000,
        zone: LISBON.into(),
        title: "Sprint planning".into(),
        busy: true,
        status: CalendarStatus::Confirmed,
        series: Some("sprint-planning".into()),
        original_start: Some(planning_was(now).timestamp_millis()),
        conference: Some(SPRINT_PLANNING_LINK.into()),
        ..CalendarEvent::default()
    });

    // The design review invitation's own event, still unanswered, with
    // lunch before it and the retro after, so the card's day strip reads
    // as the mockup draws it: the hour free, a neighbour on each side.
    let sent = chrono::DateTime::from_timestamp_millis(now).unwrap_or_default();
    let start = next_tuesday(sent.with_timezone(&chrono::Local));
    let design_review_start = start.timestamp_millis();
    // The event is kept in Lisbon's zone, so its repeats fall at Lisbon's
    // wall-clock time; UNTIL has to be worked out there too, not in the
    // machine's zone, or on a UTC machine the clock change on 25 October
    // moves the last Tuesday past it.
    let until = eight_weeks_later(start.with_timezone(&chrono_tz::Europe::Lisbon));
    events.push(CalendarEvent {
        calendar: "primary".into(),
        id: "design-review".into(),
        uid: INVITE_UID.into(),
        start: design_review_start,
        end: design_review_start + 45 * 60_000,
        zone: LISBON.into(),
        title: "Offline editor design review".into(),
        // Purple, as the mockup's card draws the meeting's bar.
        color: Some("#9141ac".into()),
        place: "Meeting Room 2, Fernwood HQ".into(),
        description: "Agenda in the deck. Bring questions about conflict resolution.".into(),
        busy: true,
        status: CalendarStatus::Confirmed,
        organizer: Some("priya@fernwood.example".into()),
        guests: vec![
            CalendarGuest {
                email: ACCOUNTS[1].email.into(),
                me: true,
                answer: None,
                ..CalendarGuest::default()
            },
            CalendarGuest {
                email: "priya@fernwood.example".into(),
                name: Some("Priya Raman".into()),
                answer: Some(Answer::Yes),
                organizer: true,
                me: false,
            },
            // The same guests the invitation lists, so the card in the
            // message and the event's popover count the same answers.
            CalendarGuest {
                email: "jonas@fernwood.example".into(),
                name: Some("Jonas Weber".into()),
                answer: Some(Answer::Maybe),
                ..CalendarGuest::default()
            },
            CalendarGuest {
                email: "mara.okafor@example.org".into(),
                name: Some("Mara Okafor".into()),
                answer: Some(Answer::No),
                ..CalendarGuest::default()
            },
        ],
        rules: vec![format!("RRULE:FREQ=WEEKLY;BYDAY=TU;UNTIL={}", until_stamp(until))],
        ..CalendarEvent::default()
    });
    events.push(CalendarEvent {
        color: Some("#e8660c".into()),
        ..timed_event(
            "primary",
            "lunch-with-ana",
            "Lunch with Ana",
            design_review_start - 120 * 60_000,
            design_review_start - 60 * 60_000,
        )
    });
    // On Work in the Design team's purple: the strip, like the clash
    // line, counts only the calendars the account owns.
    events.push(CalendarEvent {
        color: Some("#9141ac".into()),
        ..timed_event(
            "primary",
            "design-retro",
            "Retro",
            design_review_start + 90 * 60_000,
            design_review_start + 150 * 60_000,
        )
    });

    // Quarterly review, on the Design team calendar, still waiting for an
    // answer, as the mockup's popover draws it: dashed on the grid, four
    // of its six guests already said yes. The agenda runs past the
    // popover's own line cap, so the demo also shows "Show more" and,
    // with six guests, "Show all".
    let quarterly_start = at_week(monday, 2, 15, 0);
    events.push(CalendarEvent {
        calendar: DESIGN_TEAM.into(),
        id: "quarterly-review".into(),
        uid: "quarterly-review@local".into(),
        start: quarterly_start,
        end: quarterly_start + 60 * 60_000,
        zone: LISBON.into(),
        title: "Quarterly review".into(),
        place: "Room 2.04, Rua Augusta 24".into(),
        description: "Agenda:\n1. Roadmap review\n2. Budget for next quarter\n3. Hiring plan\n4. Open questions\n\nPre-read: https://docs.example.com/quarterly-review".into(),
        busy: true,
        status: CalendarStatus::Confirmed,
        organizer: Some("rita@fernwood.example".into()),
        conference: Some(QUARTERLY_REVIEW_LINK.into()),
        guests: vec![
            CalendarGuest {
                email: "rita@fernwood.example".into(),
                name: Some("Rita Lopes".into()),
                answer: Some(Answer::Yes),
                organizer: true,
                me: false,
            },
            CalendarGuest {
                email: ACCOUNTS[1].email.into(),
                me: true,
                answer: None,
                ..CalendarGuest::default()
            },
            CalendarGuest {
                email: "priya@fernwood.example".into(),
                name: Some("Priya Raman".into()),
                answer: Some(Answer::Yes),
                ..CalendarGuest::default()
            },
            CalendarGuest {
                email: "jonas@fernwood.example".into(),
                name: Some("Jonas Weber".into()),
                answer: Some(Answer::Yes),
                ..CalendarGuest::default()
            },
            CalendarGuest {
                email: "theo.alves@fernwood.example".into(),
                name: Some("Theo Alves".into()),
                answer: Some(Answer::Yes),
                ..CalendarGuest::default()
            },
            CalendarGuest {
                email: "mara.okafor@example.org".into(),
                name: Some("Mara Okafor".into()),
                answer: Some(Answer::No),
                ..CalendarGuest::default()
            },
        ],
        ..CalendarEvent::default()
    });

    events
}

/// `at`, as an iCalendar `UNTIL` in UTC.
fn until_stamp(at: EpochMillis) -> String {
    chrono::DateTime::from_timestamp_millis(at)
        .unwrap_or_default()
        .format("%Y%m%dT%H%M%SZ")
        .to_string()
}

/// The work account's entries that say where the person is, one of each
/// type Google keeps on a primary calendar: focus time on Tuesday
/// morning, out of office on Friday afternoon, and where they work on
/// each weekday but Thursday, the offsite's first day.
fn status_entries(monday: chrono::DateTime<chrono::Local>) -> Vec<CalendarEvent> {
    use mailrs_domain::calendar::{Decline, Declines, Kind, Workplace};
    let mut entries = vec![
        CalendarEvent {
            kind: Kind::Focus(Decline { meetings: Declines::New, message: "Heads down on the Q4 roadmap".into() }),
            ..timed_event("primary", "focus-tuesday", "Focus time", at_week(monday, 1, 10, 0), at_week(monday, 1, 12, 0))
        },
        CalendarEvent {
            kind: Kind::OutOfOffice(Decline {
                meetings: Declines::All,
                message: "Declined because I am out of office".into(),
            }),
            ..timed_event("primary", "away-friday", "Out of office", at_week(monday, 4, 13, 0), at_week(monday, 4, 16, 0))
        },
    ];
    let places = [
        (0, Workplace::Home),
        (1, Workplace::Office("Lisbon HQ".into())),
        (2, Workplace::Home),
        (4, Workplace::Office("Lisbon HQ".into())),
    ];
    for (offset, place) in places {
        let (start, end) = all_day_utc(monday.date_naive() + chrono::Duration::days(offset), 1);
        entries.push(CalendarEvent {
            calendar: "primary".into(),
            id: format!("where-{offset}"),
            uid: format!("where-{offset}@google.com"),
            start,
            end,
            zone: "UTC".into(),
            all_day: true,
            title: match &place {
                Workplace::Home => "Home".into(),
                _ => "Office".into(),
            },
            busy: false,
            status: CalendarStatus::Confirmed,
            kind: Kind::WorkingLocation(place),
            ..CalendarEvent::default()
        });
    }
    entries
}

/// The week of calendar events demo account `index` keeps. Only the two
/// Gmail accounts with a calendar get one; the third's primary stays
/// empty.
fn demo_events(index: usize, now: EpochMillis) -> Vec<CalendarEvent> {
    match index {
        0 => account0_events(now),
        1 => account1_events(now),
        _ => Vec::new(),
    }
}

/// The demo's fourth account, on an IMAP server rather than Gmail, so
/// screenshots and the accessibility walk cover a folder account: Move to
/// Folder, the Not Available lines, no category bar.
const FASTMAIL: &str = "dana@fastmail.example";

/// The folders the demo's IMAP server has beyond the six `FakeImap::new`
/// starts with (INBOX, Sent, Drafts, Trash, Junk and Archive, each with
/// its special use): three of the person's own, one inside another. The
/// fake separates levels with `/`, and a parent comes before its child.
const FOLDERS: [&str; 3] = ["Receipts", "Projects", "Projects/Garden"];

/// One message on the demo's IMAP server. Header names stay ASCII, as
/// mail on the wire has them; the text can be anything.
struct Letter {
    mailbox: &'static str,
    id: &'static str,
    from: (&'static str, &'static str),
    to: (&'static str, &'static str),
    subject: &'static str,
    minutes_ago: i64,
    seen: bool,
    flagged: bool,
    /// The letter this one answers, named in In-Reply-To and References.
    replies_to: Option<&'static str>,
    text: &'static str,
}

const INES: (&str, &str) = ("Ines Duarte", "ines@allotment.example");
const DANA: (&str, &str) = (DISPLAY_NAME, FASTMAIL);

fn letters() -> [Letter; 6] {
    [
        Letter {
            mailbox: "INBOX",
            id: "allotment-1",
            from: INES,
            to: DANA,
            subject: "Allotment rota for October",
            minutes_ago: 9 * HOUR,
            seen: true,
            flagged: false,
            replies_to: None,
            text: "Hi all,\n\nThe October rota is up on the shed door. Dana, you have the first two Saturdays. Could you check the water butts while you are there?\n\nInês",
        },
        Letter {
            mailbox: "INBOX",
            id: "allotment-2",
            from: ("Tomas Silva", "tomas@allotment.example"),
            to: DANA,
            subject: "Re: Allotment rota for October",
            minutes_ago: 5 * HOUR,
            seen: false,
            flagged: false,
            replies_to: Some("allotment-1"),
            text: "I can swap the 18th with anyone who wants it. The pumpkins will need picking that week.\n\nTomás",
        },
        Letter {
            mailbox: "Sent",
            id: "butts-1",
            from: DANA,
            to: INES,
            subject: "Water butts",
            minutes_ago: 3 * DAY,
            seen: true,
            flagged: false,
            replies_to: None,
            text: "Inês,\n\nThe left water butt has a crack near the tap. I'll bring a new one on Saturday.\n\nDana",
        },
        Letter {
            mailbox: "Receipts",
            id: "order-1",
            from: ("Alder Books", "orders@alderbooks.example"),
            to: DANA,
            subject: "Your order has shipped",
            minutes_ago: 2 * DAY,
            seen: true,
            flagged: false,
            replies_to: None,
            text: "Your copy of The Overstory left our shop today and should reach you by Thursday.\n\nAlder Books",
        },
        Letter {
            mailbox: "Projects/Garden",
            id: "seeds-1",
            from: ("Hollin Seeds", "hello@hollinseeds.example"),
            to: DANA,
            subject: "Seed catalogue for spring",
            minutes_ago: 6 * DAY,
            seen: true,
            flagged: false,
            replies_to: None,
            text: "Our spring catalogue is out, with twelve new tomatoes and a broad bean that shrugs off frost.\n\nHollin Seeds",
        },
        Letter {
            mailbox: "Archive",
            id: "fair-1",
            from: INES,
            to: DANA,
            subject: "Photos from the harvest fair",
            minutes_ago: 9 * DAY,
            seen: true,
            flagged: true,
            replies_to: None,
            text: "Dana,\n\nThe photos from the fair are in the shared album. The marrow picture is the best one.\n\nInês",
        },
    ]
}

impl Letter {
    fn at(&self, now: EpochMillis) -> EpochMillis {
        now - self.minutes_ago * 60 * 1000
    }

    fn flags(&self) -> Vec<&'static str> {
        let mut flags = Vec::new();
        if self.seen {
            flags.push("\\Seen");
        }
        if self.flagged {
            flags.push("\\Flagged");
        }
        flags
    }

    /// The message as the server holds it.
    fn raw(&self, now: EpochMillis) -> Vec<u8> {
        let date = chrono::DateTime::from_timestamp_millis(self.at(now))
            .unwrap_or_default()
            .to_rfc2822();
        let answers = self
            .replies_to
            .map(|id| {
                format!(
                    "In-Reply-To: <{id}@fastmail.example>\r\nReferences: <{id}@fastmail.example>\r\n"
                )
            })
            .unwrap_or_default();
        format!(
            "From: {} <{}>\r\nTo: {} <{}>\r\nSubject: {}\r\nDate: {date}\r\n\
             Message-ID: <{}@fastmail.example>\r\n{answers}MIME-Version: 1.0\r\n\
             Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{}\r\n",
            self.from.0, self.from.1, self.to.0, self.to.1, self.subject, self.id, self.text,
        )
        .into_bytes()
    }
}

/// The demo's Fastmail account as its adapter sees it: its own address,
/// and Fastmail's sent-copy rule from the provider table, as a real
/// Fastmail account reads it at start.
fn fastmail_settings() -> ImapSettings {
    ImapSettings {
        address: FASTMAIL.to_string(),
        provider_name: "Fastmail".to_string(),
        files_sent_mail: mailrs_discover::provider_named("Fastmail")
            .is_some_and(|provider| provider.files_sent_mail),
        window_days: DEFAULT_WINDOW_DAYS,
    }
}

/// The servers the demo's Fastmail account keeps, so Sign In Again has
/// something to open on. Nothing connects to them.
fn fastmail_servers() -> Servers {
    let saved = |host: &str, port| Saved {
        host: host.to_string(),
        port,
        security: Security::Tls,
        user_name: FASTMAIL.to_string(),
    };
    Servers {
        imap: saved("imap.fastmail.example", 993),
        smtp: saved("smtp.fastmail.example", 465),
    }
}

/// Adds the Fastmail account, fills its IMAP server with the letters,
/// and lets sync fill the store from it.
async fn seed_fastmail(
    db: &Db,
    now: EpochMillis,
) -> std::result::Result<(AccountId, DemoServer, Arc<AccountSync>), SyncError> {
    let account_id = db
        .write(move |c| {
            // The store is new, so nothing else holds the address.
            let id = accounts::insert_imap_account(c, FASTMAIL, "Fastmail", now)?.ok_or(
                StoreError::Corrupt {
                    column: "accounts.provider",
                    value: FASTMAIL.to_string(),
                },
            )?;
            servers::save(c, id, &fastmail_servers())?;
            for (kind, url) in [
                (ServiceKind::CalDav, "https://caldav.fastmail.example/"),
                (ServiceKind::CardDav, "https://carddav.fastmail.example/"),
            ] {
                mailrs_store::services::save(
                    c,
                    id,
                    &FoundService {
                        kind,
                        url: url.into(),
                        user_name: FASTMAIL.into(),
                        confirmed: true,
                        source: "table".into(),
                    },
                )?;
            }
            mailrs_store::local_rules::add(
                c,
                id,
                &Filter {
                    id: Some("local-demo-receipts".into()),
                    criteria: FilterCriteria {
                        subject: Some("receipt".into()),
                        ..FilterCriteria::default()
                    },
                    action: FilterAction {
                        add: vec![MailSet::Mailbox("Receipts".into())],
                        ..FilterAction::default()
                    },
                    ..Filter::default()
                },
            )?;
            mailrs_store::local_rules::start_running(c, id, now)?;
            Ok(id)
        })
        .await?;
    let imap = Arc::new(FakeImap::new());
    for name in FOLDERS {
        imap.add_mailbox(name, None);
    }
    for letter in letters() {
        // The fake delivers mail unread, as a server does; the flags a
        // letter carries go on after, as another client would set them.
        let uid = imap.deliver(letter.mailbox, letter.raw(now), letter.at(now));
        for flag in letter.flags() {
            imap.remote_flag(letter.mailbox, uid, flag, true);
        }
    }
    let smtp = Arc::new(FakeSmtp::default());
    let dav = Arc::new(FakeDav::new());
    dav.add_collection("/cal/personal/", DavKind::Calendar, "Personal", Some("#e66100"));
    dav.add_collection("/card/default/", DavKind::AddressBook, "Contacts", None);
    for (href, body) in fastmail_calendar(now).into_iter().chain(fastmail_cards()) {
        dav.put_resource(&href, &body);
    }
    let (events, _) = async_channel::unbounded();
    let sync = Arc::new(AccountSync::new(
        account_id,
        imap_services(&imap, &smtp, &dav, db, account_id),
        db.clone(),
        events,
    ));
    fill_store(&sync).await?;
    Ok((account_id, DemoServer::Imap { imap, smtp, dav, db: db.clone() }, sync))
}

/// The Fastmail account's services: its IMAP server for mail, a CalDAV and
/// a CardDAV server for the calendar and contacts, and rules kept on this
/// computer. The calendar answers an invitation through the same mail
/// adapter, as a real account's does. Fastmail has no ManageSieve here,
/// so there is no automatic reply.
fn imap_services(
    imap: &Arc<FakeImap>,
    smtp: &Arc<FakeSmtp>,
    dav: &Arc<FakeDav>,
    db: &Db,
    account_id: AccountId,
) -> AccountServices {
    let mail = Imap::new(Arc::clone(imap), Arc::clone(smtp), fastmail_settings());
    AccountServices::fake_imap_with(Arc::clone(imap), Arc::clone(smtp), fastmail_settings())
        .with_calendar(AnyCalendar::FakeDav(CalDav::new(Arc::clone(dav), mail, vec![FASTMAIL.to_string()])))
        .with_contacts(AnyContacts::FakeDav(CardDav::new(Arc::clone(dav))))
        .with_rules(AnyRules::Local(LocalRules::new(db.clone(), account_id)))
}

/// The Fastmail account's own calendar this week: climbing on Tuesday,
/// the dentist on Thursday, and a book club every Wednesday, written as a
/// CalDAV server holds them.
fn fastmail_calendar(now: EpochMillis) -> Vec<(String, String)> {
    let monday = week_monday(now);
    let stamp = |at: EpochMillis| {
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at)
            .unwrap_or_default()
            .format("%Y%m%dT%H%M%SZ")
            .to_string()
    };
    let event = |uid: &str, title: &str, start: EpochMillis, minutes: i64, rule: &str| {
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Fastmail//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:{}\r\n\
             DTSTART:{}\r\nDTEND:{}\r\n{rule}SUMMARY:{title}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            stamp(now),
            stamp(start),
            stamp(start + minutes * 60_000),
        )
    };
    vec![
        ("/cal/personal/climbing.ics".into(), event("climbing", "Climbing", at_week(monday, 1, 18, 30), 90, "")),
        ("/cal/personal/dentist.ics".into(), event("dentist", "Dentist", at_week(monday, 3, 10, 0), 45, "")),
        (
            "/cal/personal/book-club.ics".into(),
            event("book-club", "Book club", at_week(monday, 2, 19, 0), 120, "RRULE:FREQ=WEEKLY\r\n"),
        ),
    ]
}

/// The Fastmail account's address book, as vCards.
fn fastmail_cards() -> Vec<(String, String)> {
    [
        ("tomas", "Tomás Faria", "tomas@climbing.example"),
        ("rui", "Rui Pinto", "rui@bookclub.example"),
        ("ines", INES.0, INES.1),
    ]
    .into_iter()
    .map(|(uid, name, email)| {
        (
            format!("/card/default/{uid}.vcf"),
            format!(
                "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{name}\r\nEMAIL;TYPE=INTERNET:{email}\r\nEND:VCARD\r\n"
            ),
        )
    })
    .collect()
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
    png(pixels, SIZE, SIZE)
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
    const WIDTH: usize = 640;
    const HEIGHT: usize = 420;
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
    let horizon = HEIGHT * 2 / 3;
    let pixels = (0..HEIGHT)
        .flat_map(|y| {
            let (r, g, b) = if y < horizon { top } else { bottom };
            [r, g, b].repeat(WIDTH)
        })
        .collect();
    Some(png(pixels, WIDTH, HEIGHT))
}

/// RGB `pixels`, `width` by `height`, as a PNG. GDK writes it in this
/// process; gdk-pixbuf would start a sandboxed glycin process for it.
fn png(pixels: Vec<u8>, width: usize, height: usize) -> Vec<u8> {
    use gtk::prelude::TextureExt;
    gtk::gdk::MemoryTexture::new(
        width as i32,
        height as i32,
        gtk::gdk::MemoryFormat::R8g8b8,
        &gtk::glib::Bytes::from_owned(pixels),
        width * 3,
    )
    .save_to_png_bytes()
    .to_vec()
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
                if invite.answerable {
                    state.calendar.insert(invite.uid.into(), None);
                }
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
    // Adding eight weeks of milliseconds would drift UNTIL's wall-clock
    // hour by one across Lisbon's clock change (the rrule-until-dst
    // trap); `eight_weeks_later` walks the date and re-anchors to the
    // zone instead, the same fix `account1_events` already carries.
    let until = eight_weeks_later(start);
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
        format!("RRULE:FREQ=WEEKLY;BYDAY=TU;UNTIL={}", until_stamp(until)),
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

/// A ticket the sender only published: two journeys in one file, with no
/// `METHOD:REQUEST` and nobody to answer.
fn ticket_ics(now: EpochMillis) -> String {
    let stamp = |at: chrono::DateTime<chrono::Local>| {
        at.with_timezone(&chrono::Utc).format("%Y%m%dT%H%M%SZ").to_string()
    };
    let sent = chrono::DateTime::from_timestamp_millis(now).unwrap_or_default();
    let out = weekday_at(now, chrono::Weekday::Fri, 9);
    let back = weekday_at(now, chrono::Weekday::Sun, 17);
    let leg = |uid: String, title: &str, from: &str, to: &str, start: chrono::DateTime<chrono::Local>| {
        vec![
            "BEGIN:VEVENT".to_string(),
            format!("UID:{uid}"),
            format!("DTSTAMP:{}", stamp(sent.with_timezone(&chrono::Local))),
            format!("DTSTART:{}", stamp(start)),
            format!("DTEND:{}", stamp(start + chrono::Duration::minutes(195))),
            format!("SUMMARY:{title}"),
            format!("LOCATION:{from} to {to}\\, coach 4 seat 22"),
            "END:VEVENT".to_string(),
        ]
    };
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "PRODID:-//CP Comboios//Tickets//EN".to_string(),
        "VERSION:2.0".to_string(),
        "METHOD:PUBLISH".to_string(),
    ];
    lines.extend(leg(format!("{TICKET_UID}-out"), "Train to Porto", "Lisboa Santa Apolonia", "Porto Campanha", out));
    lines.extend(leg(format!("{TICKET_UID}-back"), "Train to Lisboa", "Porto Campanha", "Lisboa Santa Apolonia", back));
    lines.push("END:VCALENDAR".to_string());
    lines.push(String::new());
    lines.join("\r\n")
}

/// The update that moves the sprint planning meeting.
fn moved_ics(now: EpochMillis) -> String {
    let stamp = |at: chrono::DateTime<chrono::Utc>| at.format("%Y%m%dT%H%M%SZ").to_string();
    let sent = chrono::DateTime::from_timestamp_millis(now).unwrap_or_default();
    let start = planning_is(now);
    let end = start + chrono::Duration::minutes(90);
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
/// 10:00 local, as the approved mockup draws it overlapping Dentist.
fn planning_was(now: EpochMillis) -> chrono::DateTime<chrono::Local> {
    weekday_at(now, chrono::Weekday::Wed, 10)
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

/// The instant eight weeks after `start`, at the same local wall-clock
/// time, as an epoch timestamp. Lisbon's clocks fall back within an
/// eight-week span, so adding eight weeks of milliseconds lands an hour
/// before the real occurrence at that wall-clock time; going through
/// the date and re-anchoring to the zone, as [`next_weekday`] does,
/// keeps a series' `UNTIL` on the same Tuesday its `RRULE` would land on.
fn eight_weeks_later<Z: chrono::TimeZone>(start: chrono::DateTime<Z>) -> EpochMillis {
    use chrono::Timelike;
    let local = start.naive_local();
    let day = local.date() + chrono::Days::new(8 * 7);
    day.and_hms_opt(local.hour(), local.minute(), local.second())
        .and_then(|at| start.timezone().from_local_datetime(&at).earliest())
        .unwrap_or(start)
        .timestamp_millis()
}

#[cfg(test)]
mod tests {
    use mailrs_domain::{Folder, MailSet, Role};
    use mailrs_gmail::labels as gmail;
    use mailrs_store::bodies;
    use mailrs_store::threads::{self, ThreadFilter};
    use mailrs_sync::{AccountServices, GmailApi, IdentityService, MailBackend, now_millis};

    use super::*;

    /// A demo store, its mail, and the time it was seeded at. The fake
    /// searches by the clock, so the samples hang off the real time.
    struct Demo {
        db: Db,
        mail: DemoMail,
        now: EpochMillis,
        _dir: tempfile::TempDir,
    }

    async fn demo() -> Demo {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("demo.db")).unwrap();
        let now = now_millis();
        let mail = seed(&db, now).await.unwrap();
        Demo {
            db,
            mail,
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

        async fn fastmail(&self) -> AccountId {
            self.db
                .read(|c| accounts::account_by_email(c, FASTMAIL))
                .await
                .unwrap()
                .expect("the Fastmail account is stored")
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
            self.mail
                .gmail(account)
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
    async fn the_fourth_account_is_fastmail_on_imap_and_files_in_folders() {
        let demo = demo().await;
        let id = demo.fastmail().await;
        let account = demo
            .db
            .read(|c| accounts::account_by_email(c, FASTMAIL))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(account.provider, mailrs_domain::Provider::Imap);
        assert_eq!(account.provider_name(), "Fastmail");
        let offers = demo
            .mail
            .services(id)
            .expect("the account has a server")
            .offers();
        assert!(!offers.labels);
        assert!(!offers.categories);
        assert!(offers.calendar && offers.contacts && offers.rules);
        assert!(!offers.auto_reply, "Fastmail runs no ManageSieve here");
    }

    #[tokio::test]
    async fn the_demo_has_a_pop3_account_with_messages_that_will_not_download() {
        let demo = demo().await;
        let account = demo
            .db
            .read(|c| accounts::account_by_email(c, pop3::POP3))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(account.provider, mailrs_domain::Provider::Pop3);
        assert!(demo.mail.services(account.id).is_some(), "the demo serves it");
        let id = account.id;
        let failing = demo.db.read(move |c| mailrs_store::pop3::failing(c, id)).await.unwrap();
        assert_eq!(failing.len(), 2, "{failing:?}");
        let inbox = demo
            .db
            .read(move |c| mailrs_store::messages::held_by(c, id, &MailSet::Role(Role::Inbox)))
            .await
            .unwrap();
        assert!(inbox.len() >= 4, "{inbox:?}");
        let own = demo.db.read(move |c| mailrs_store::labels::list_labels(c, id)).await.unwrap();
        assert!(own.iter().any(|l| l.name == pop3::FOLDER), "{own:?}");
    }

    #[tokio::test]
    async fn the_demo_has_an_outlook_account_with_tags_focus_and_a_calendar() {
        let demo = demo().await;
        let account = demo
            .db
            .read(|c| accounts::account_by_email(c, outlook::OUTLOOK))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(account.provider, mailrs_domain::Provider::Microsoft);
        assert_eq!(account.provider_name(), "Outlook");
        let services = demo.mail.services(account.id).expect("the demo serves it");
        let offers = services.offers();
        assert!(offers.tags && offers.focused && offers.calendar && offers.contacts);
        let id = account.id;
        let tags = demo.db.read(move |c| mailrs_store::labels::tag_ids(c, id)).await.unwrap();
        assert!(tags.contains("category:Travel"), "{tags:?}");
        let counts = demo
            .db
            .read(move |c| {
                let filter = ThreadFilter::account(id, MailSet::Role(Role::Inbox));
                threads::category_unread_threads(c, &filter)
            })
            .await
            .unwrap();
        assert!(counts[&mailrs_domain::Category::Other] > 0 && counts[&mailrs_domain::Category::Focused] > 0);
        let calendars = demo.db.read(move |c| mailrs_store::calendar::calendars(c, id)).await.unwrap();
        assert!(!calendars.is_empty());
        let (from, to) = (demo.now - 40 * 86_400_000, demo.now + 40 * 86_400_000);
        let seen = demo
            .db
            .read(move |c| {
                mailrs_store::calendar::occurrences(c, &[id], from, to, mailrs_store::calendar::CalendarScope::Shown)
            })
            .await
            .unwrap();
        assert!(
            seen.iter().filter(|o| o.event.title == "Standup").count() >= 4,
            "the weekly standup repeats on the calendar"
        );
        let people = demo.db.read(mailrs_store::address_book::list).await.unwrap();
        assert!(people.iter().any(|p| p.account_id == id && p.name.as_deref() == Some("Rui Costa")));
    }

    #[tokio::test]
    async fn the_fastmail_account_has_a_calendar_contacts_and_rules_here() {
        let demo = demo().await;
        let fastmail = demo.fastmail().await;
        let services = demo.mail.services(fastmail).expect("the demo serves it");
        assert!(matches!(services.calendar, Some(mailrs_sync::AnyCalendar::FakeDav(_))));
        assert!(matches!(services.contacts, Some(mailrs_sync::AnyContacts::FakeDav(_))));
        assert_eq!(
            services.rules.as_ref().map(mailrs_sync::AnyRules::place),
            Some(mailrs_sync::RulesPlace::ThisComputer)
        );
        assert!(services.auto_reply.is_none(), "Fastmail runs no ManageSieve");
        let calendars = demo.db.read(move |c| mailrs_store::calendar::calendars(c, fastmail)).await.unwrap();
        assert_eq!(calendars.len(), 1, "the calendar was read at seed");
        assert!(demo.db.read(move |c| mailrs_store::calendar::synced(c, fastmail)).await.unwrap());
        let people = demo.db.read(mailrs_store::address_book::list).await.unwrap();
        assert!(people.iter().any(|p| p.account_id == fastmail && p.name.as_deref() == Some("Tomás Faria")));
        let rules = demo.db.read(move |c| mailrs_store::local_rules::list(c, fastmail)).await.unwrap();
        assert_eq!(rules.len(), 1);
    }

    #[tokio::test]
    async fn each_gmail_account_is_synced_with_its_calendars() {
        let demo = demo().await;
        for index in 0..ACCOUNTS.len() {
            let id = demo.account(index).await;
            assert!(
                demo.db.read(move |c| mailrs_store::calendar::synced(c, id)).await.unwrap(),
                "account {index} should be synced"
            );
        }
    }

    #[tokio::test]
    async fn the_demos_calendar_events_agree_with_its_invitations() {
        let demo = demo().await;
        let dana = demo.account(1).await;
        // (title, calendar, series): `series` is set on the row that
        // stands for one changed occurrence, so it does not count as a
        // second whole event under the same title.
        let events: Vec<(String, String, Option<String>)> = demo
            .db
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT title, calendar, series FROM events WHERE account_id = ?1 ORDER BY title",
                )?;
                let rows = stmt.query_map([dana], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
                Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
            .unwrap();
        assert!(
            events
                .iter()
                .any(|(title, calendar, series)| title == "Sprint planning" && calendar == "primary" && series.is_none()),
            "{events:?}"
        );
        assert!(
            events
                .iter()
                .any(|(title, calendar, _)| title == "Offline editor design review" && calendar == "primary"),
            "{events:?}"
        );
        assert!(
            events.iter().any(|(title, calendar, _)| title == "Lunch with Ana" && calendar == "primary"),
            "{events:?}"
        );
        assert_eq!(
            events.iter().filter(|(title, _, series)| title == "Sprint planning" && series.is_none()).count(),
            1,
            "no second Sprint planning: {events:?}"
        );
    }

    #[tokio::test]
    async fn the_fastmail_folders_and_mail_reach_the_store() {
        let demo = demo().await;
        let id = demo.fastmail().await;
        let names: Vec<String> = demo
            .db
            .read(move |c| mailrs_store::mailboxes::listed(c, id))
            .await
            .unwrap()
            .into_iter()
            .map(|mailbox| mailbox.name)
            .collect();
        assert!(names.iter().any(|name| name == "Receipts"), "{names:?}");
        // The reply names the first message in In-Reply-To, so local
        // threading puts the two in one conversation.
        let inbox = demo
            .threads(ThreadFilter::account(id, MailSet::Role(Role::Inbox)))
            .await;
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].message_count, 2);
    }

    #[tokio::test]
    async fn the_fastmail_servers_are_kept_for_signing_in_again() {
        let demo = demo().await;
        let id = demo.fastmail().await;
        let kept = demo
            .db
            .read(move |c| mailrs_store::servers::load(c, id))
            .await
            .unwrap();
        assert_eq!(kept, Some(fastmail_servers()));
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
        // Four Gmail contacts, the three in the Fastmail address book and the two at Outlook.
        assert_eq!(known.len(), 9, "every demo contact comes before the rest");
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
        // Fastmail's Water butts message, sent to Inês three days ago with
        // no reply, waits too: the follow-up query reads any account's Sent
        // mailbox by role, not only Gmail's.
        assert_eq!(waiting, ["Sent/1002/1", "t-invoice", "t-lease"]);
    }

    #[tokio::test]
    async fn a_sent_reply_lands_in_sent_and_in_its_conversation() {
        let demo = demo().await;
        let work = demo.account(1).await;
        let fake = demo.mail.gmail(work).unwrap();
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
    async fn the_sample_invitation_is_on_the_work_calendar() {
        let demo = demo().await;
        let (work, now) = (demo.account(1).await, demo.now);
        let day = 24 * 60 * 60 * 1_000;
        let found = demo
            .db
            .read(move |c| mailrs_store::calendar::with_uid(c, work, INVITE_UID, now - 30 * day, now + 90 * day))
            .await
            .unwrap();
        let first = found.first().expect("the design review is on the calendar");
        let sent = chrono::DateTime::from_timestamp_millis(now).unwrap().with_timezone(&chrono::Local);
        assert_eq!(first.start, next_tuesday(sent).timestamp_millis());
        assert_eq!(first.event.title, "Offline editor design review");
        assert_eq!(first.event.my_answer, None);
        // UNTIL is eight weeks after the first start, on a Tuesday at the
        // same time, and iCalendar counts it: nine Tuesdays.
        assert_eq!(found.len(), 9);
    }

    #[test]
    fn a_series_until_keeps_its_local_hour_across_lisbon_s_clock_change() {
        use chrono::TimeZone;
        // Pinned to Lisbon rather than the machine's zone, so the test
        // crosses a clock change on a UTC machine too: the clocks go back
        // on 25 October 2026, between these two Tuesdays.
        let lisbon = chrono_tz::Europe::Lisbon;
        let start = lisbon.with_ymd_and_hms(2026, 9, 29, 14, 0, 0).unwrap();
        let eighth = lisbon.with_ymd_and_hms(2026, 11, 24, 14, 0, 0).unwrap();
        assert_eq!(eight_weeks_later(start), eighth.timestamp_millis());
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
        assert_eq!(accounts_seen.len(), 6);
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
        let api = demo.mail.gmail(work).expect("the account has a mailbox");
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

#[cfg(test)]
mod week_anchor_tests {
    use super::demo_monday;
    use chrono::{NaiveDate, Weekday};

    fn day(m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, d).expect("a date")
    }

    #[test]
    fn a_sunday_in_a_week_that_starts_on_sunday_hangs_off_the_next_monday() {
        assert_eq!(demo_monday(day(10, 4), Weekday::Sun), day(10, 5));
    }

    #[test]
    fn a_sunday_in_a_week_that_starts_on_monday_keeps_its_own_monday() {
        assert_eq!(demo_monday(day(10, 4), Weekday::Mon), day(9, 28));
    }

    #[test]
    fn a_midweek_day_hangs_off_its_monday_whatever_the_start() {
        assert_eq!(demo_monday(day(9, 30), Weekday::Sun), day(9, 28));
        assert_eq!(demo_monday(day(9, 30), Weekday::Mon), day(9, 28));
        assert_eq!(demo_monday(day(10, 3), Weekday::Sat), day(10, 5));
    }
}
