//! The demo's POP3 account: Dana's address on a small host, served by
//! sync's in-memory POP3 server. Its Inbox holds a few messages, a folder
//! of her own holds two more, and the server refuses two messages three
//! times, so the account menu offers "Messages That Will Not Download".

use std::sync::Arc;

use mailrs_domain::{AccountId, EpochMillis, RemoveSetting};
use mailrs_store::messages::{self, Change};
use mailrs_store::{Db, StoreError, accounts};
use mailrs_sync::fake::{FakePop3, FakeSmtp};
use mailrs_sync::{AccountServices, AccountSync, Pop3Settings, SyncError};

pub const POP3: &str = "dana@reyes-home.example";

/// The folder of Dana's own on this account.
pub const FOLDER: &str = "Garden club";

const HOUR: i64 = 60 * 60 * 1000;

pub fn settings() -> Pop3Settings {
    Pop3Settings { address: POP3.into(), provider_name: "reyes-home.example".into() }
}

/// What a POP3 demo account talks to.
pub struct Pop3Server {
    pop3: Arc<FakePop3>,
    smtp: Arc<FakeSmtp>,
}

impl Pop3Server {
    pub fn services(&self, db: &Db, account_id: AccountId) -> AccountServices {
        AccountServices::pop3(
            db.clone(),
            account_id,
            Arc::clone(&self.pop3),
            Arc::clone(&self.smtp),
            settings(),
        )
    }
}

struct Letter {
    uidl: &'static str,
    from: (&'static str, &'static str),
    subject: &'static str,
    text: &'static str,
    hours_ago: i64,
}

const INBOX: [Letter; 4] = [
    Letter {
        uidl: "in-1",
        from: ("Marta Reyes", "marta@reyes-home.example"),
        subject: "Photos from Saturday",
        text: "I put the good ones in the shared album. The one of Dad with the hose is my favorite.",
        hours_ago: 3,
    },
    Letter {
        uidl: "in-2",
        from: ("Hollis Hardware", "orders@hollis-hardware.example"),
        subject: "Your order is ready for pickup",
        text: "Two bags of potting soil and a trowel are at the counter until Friday.",
        hours_ago: 20,
    },
    Letter {
        uidl: "in-3",
        from: ("Dr. Leal's office", "front-desk@leal-clinic.example"),
        subject: "Appointment on Thursday at 10:30",
        text: "Please arrive ten minutes early and bring your card.",
        hours_ago: 30,
    },
    Letter {
        uidl: "in-4",
        from: ("Tomás", "tomas@reyes-home.example"),
        subject: "Can I borrow the ladder?",
        text: "Only until the weekend. I will bring it back clean.",
        hours_ago: 52,
    },
];

const CLUB: [Letter; 2] = [
    Letter {
        uidl: "club-1",
        from: ("Inês Calado", "ines@garden-club.example"),
        subject: "Seed swap on the 18th",
        text: "Bring whatever you saved this year. I will bring tomato and courgette.",
        hours_ago: 5 * 24,
    },
    Letter {
        uidl: "club-2",
        from: ("Garden club", "list@garden-club.example"),
        subject: "Minutes from the autumn meeting",
        text: "The greenhouse repair is approved. Dues stay the same.",
        hours_ago: 9 * 24,
    },
];

/// Two messages the server answers with an error however often it is
/// asked.
const REFUSED: [Letter; 2] = [
    Letter {
        uidl: "bad-1",
        from: ("Lucas Reyes", "lucas@reyes-home.example"),
        subject: "Holiday video, full size",
        text: "Too big for this server to hand over.",
        hours_ago: 6 * 24,
    },
    Letter {
        uidl: "bad-2",
        from: ("Tomás", "tomas@reyes-home.example"),
        subject: "Scan of the contract",
        text: "The scan did not survive the trip.",
        hours_ago: 40,
    },
];

fn raw(letter: &Letter, now: EpochMillis) -> Vec<u8> {
    let date = chrono::DateTime::from_timestamp_millis(now - letter.hours_ago * HOUR)
        .unwrap_or_default()
        .to_rfc2822();
    format!(
        "From: {} <{}>\r\nTo: {POP3}\r\nSubject: {}\r\nMessage-ID: <{}@reyes-home.example>\r\n\
         Date: {date}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}\r\n",
        letter.from.0, letter.from.1, letter.subject, letter.uidl, letter.text
    )
    .into_bytes()
}

/// Adds the POP3 account, lets the downloader check its server three
/// times so the refused messages count as failing, and files the club's
/// mail in a folder Dana made.
pub async fn seed_pop3(
    db: &Db,
    now: EpochMillis,
) -> Result<(AccountId, Pop3Server, Arc<AccountSync>), SyncError> {
    let account_id = db
        .write(move |c| {
            accounts::insert_pop3_account(c, POP3, "reyes-home.example", RemoveSetting::Never, now)?
                .ok_or(StoreError::Corrupt { column: "accounts.provider", value: POP3.to_string() })
        })
        .await?;
    let fake = FakePop3::default();
    // The refused two come first, so their message numbers read 1 and 2.
    for letter in REFUSED.iter().chain(&INBOX).chain(&CLUB) {
        fake.add(letter.uidl, &raw(letter, now));
    }
    let fake = REFUSED.iter().fold(fake, |fake, letter| fake.failing_retr(letter.uidl));
    let server = Pop3Server { pop3: Arc::new(fake), smtp: Arc::new(FakeSmtp::default()) };
    let (events, _) = async_channel::unbounded();
    let sync = Arc::new(AccountSync::new(account_id, server.services(db, account_id), db.clone(), events));
    sync.refresh_labels().await?;
    for _ in 0..3 {
        sync.pop3_check().await?;
    }
    let folder = sync.create_label(FOLDER).await?;
    let moved: Vec<Change> = CLUB
        .iter()
        .flat_map(|letter| {
            let id = format!("pop3/{}", letter.uidl);
            [
                Change::AddToMailbox { message_id: id.clone(), mailbox: folder.id.clone() },
                Change::RemoveFromMailbox { message_id: id, mailbox: "inbox".into() },
            ]
        })
        .collect();
    db.write(move |c| messages::apply(c, account_id, &moved).map(drop)).await?;
    Ok((account_id, server, sync))
}
