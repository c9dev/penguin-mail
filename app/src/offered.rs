//! What the window and the assistant offer for each account, read from
//! the account's services, and the words for what an account lacks.

use mailrs_domain::translate::{fill, fill_plural, gettext, with_reason};
use mailrs_domain::{Account, AccountId, Category, MailRoute, Provider, RemoveSetting};
use mailrs_store::pop3::{FailReason, Failing};
use mailrs_store::services::Miss;
use mailrs_sync::{AccountServices, BackendError, Mailbox, Missing, Offers, SyncError, Withheld};

/// What an account offers. An account that is not running yet has no
/// services to ask, and the window assumes it offers everything until it
/// starts, so nothing flickers away.
pub fn offers_for(services: Option<&AccountServices>) -> Offers {
    services.map_or(Offers::EVERYTHING, AccountServices::offers)
}

/// What an account's own consent left withheld. An account that is not
/// running yet has no services to ask, and nothing is withheld until a
/// read of its grants says otherwise.
pub fn withheld_for(services: Option<&AccountServices>) -> Withheld {
    services.map_or(Withheld::NONE, AccountServices::withheld)
}

/// Whether an answer to an invitation on the account, whose provider is
/// `provider`, reaches its organizer. A calendar account's server says
/// whether it sends replies only once it has been read, so the event
/// popover asks each time it opens; until the account has started, and
/// until its server has said, nobody is taken to hear. An account with
/// mail that is not running yet sends the reply once it starts. An
/// account the window does not know (`None`) is not said to be mute.
pub fn organizer_hears(provider: Option<Provider>, services: Option<&AccountServices>) -> bool {
    match services {
        Some(services) => services.tells_organizer(),
        None => provider.is_none_or(Provider::serves_mail),
    }
}

/// Whether the account served by `provider` can propose another time for
/// a meeting. A proposal goes to the organizer as mail, whatever the
/// calendar's server does, and a calendar account has none to send,
/// running or not. An account with mail that is not running yet is not
/// held back, nor is one the window does not know.
pub fn can_propose(provider: Option<Provider>, services: Option<&AccountServices>) -> bool {
    match services {
        Some(services) => services.serves_mail(),
        None => provider.is_none_or(Provider::serves_mail),
    }
}

/// Whether the window has accounts and none of them has mail. It then
/// shows the calendar and offers no switch to a mail space that would be
/// empty. No account at all is the first-run page instead.
pub fn only_calendars(accounts: &[Account]) -> bool {
    !accounts.is_empty()
        && accounts
            .iter()
            .all(|account| !account.provider.serves_mail())
}

/// Whether the window may show `space`. With calendar accounts alone the
/// calendar is the one space there is, shown though no switch offers it,
/// and there is no mail to show. Otherwise the calendar shows only while
/// the switch does.
pub fn may_show(space: crate::settings::Space, switch_visible: bool, accounts: &[Account]) -> bool {
    match space {
        crate::settings::Space::Mail => !only_calendars(accounts),
        crate::settings::Space::Calendar => only_calendars(accounts) || switch_visible,
    }
}

/// Whether the sidebar offers the Mail / Calendar switch. `started` holds
/// what each running account offers; `waiting` says some account has not
/// started yet. Once one running account offers a calendar the switch
/// shows. Until the accounts have started nobody knows, so the switch
/// follows the space the window opened on last, which keeps it from
/// blinking away and back for someone who uses the calendar.
pub fn shows_space_switch(
    started: &[Offers],
    waiting: bool,
    remembered: crate::settings::Space,
) -> bool {
    started.iter().any(|offers| offers.calendar)
        || (waiting && remembered == crate::settings::Space::Calendar)
}

/// Whether the category bar shows over `mailbox`: the person has
/// categories on, the mailbox is an inbox, and an account it lists sorts
/// its inbox into categories. In the unified inbox one such account is
/// enough; mail from the others counts as Primary.
pub fn shows_categories(
    on: bool,
    mailbox: &Mailbox,
    accounts: &[AccountId],
    offers: impl Fn(AccountId) -> Offers,
) -> bool {
    on && mailbox.takes_categories()
        && match mailbox.account() {
            Some(id) => offers(id).categories,
            None => accounts.iter().any(|id| offers(*id).categories),
        }
}

/// The switcher above an inbox: Gmail's categories, or Focused Inbox's two
/// tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxBar {
    Categories,
    Focus,
}

/// Which switcher shows over `mailbox`, if any. Gmail's categories win
/// wherever they apply, the unified inbox included; Focused and Other
/// show over one account's inbox when that account sorts it so. The
/// person's inbox categories preference (`on`) turns both off.
pub fn inbox_bar(
    on: bool,
    mailbox: &Mailbox,
    accounts: &[AccountId],
    offers: impl Fn(AccountId) -> Offers,
) -> Option<InboxBar> {
    if shows_categories(on, mailbox, accounts, &offers) {
        return Some(InboxBar::Categories);
    }
    let focus =
        on && mailbox.takes_categories() && mailbox.account().is_some_and(|id| offers(id).focused);
    focus.then_some(InboxBar::Focus)
}

/// The slice to list once `bar` shows: the one on screen when the bar has
/// it, else Focused for the tabs and `default` for Gmail's categories.
pub fn category_after(bar: InboxBar, current: Category, default: Category) -> Category {
    let focus = Category::FOCUS.contains(&current);
    match (bar, focus) {
        (InboxBar::Focus, true) | (InboxBar::Categories, false) => current,
        (InboxBar::Focus, false) => Category::Focused,
        (InboxBar::Categories, true) => default,
    }
}

/// Whether Tags… is on: the mail reached comes from one account, and it
/// keeps tags.
pub fn tags_on(reached: &[AccountId], offers: impl Fn(AccountId) -> Offers) -> bool {
    let mut accounts = reached.to_vec();
    accounts.sort_unstable();
    accounts.dedup();
    matches!(accounts.as_slice(), [one] if offers(*one).tags)
}

/// The slices Categorize Sender offers for an account: Focused and Other
/// where the inbox splits that way, else Gmail's four.
pub fn categorize_choices(offers: Offers) -> Vec<Category> {
    match offers.focused {
        true => Category::FOCUS.to_vec(),
        false => Category::ALL
            .into_iter()
            .filter(|c| *c != Category::All)
            .collect(),
    }
}

/// Whether the sender's own actions are on for an account that `offers`
/// what it offers. Blocking a sender and sorting its mail into a category
/// both leave a rule on the server for the mail still to come, so both
/// need rules. A Microsoft account sorts senders into Focused and Other.
pub fn sender_actions(offers: Offers) -> [(&'static str, bool); 2] {
    [
        ("block-sender", offers.rules),
        (
            "categorize-sender",
            (offers.categories || offers.focused) && offers.rules,
        ),
    ]
}

/// The window's actions that need rules or an automatic reply, and
/// whether each is on for accounts that offer `offers`: on while one of
/// them can do it. Each account action also turns away an account that
/// cannot, through [`account_action_on`], since one action serves every
/// account's menu.
pub fn account_actions(offers: &[Offers]) -> [(&'static str, bool); 4] {
    let rules = offers.iter().any(|o| o.rules);
    let hides = offers.iter().any(|o| hides_addresses(*o));
    let auto_reply = offers.iter().any(|o| o.auto_reply);
    [
        ("hide-my-email", hides),
        ("account-rules", rules),
        ("account-hide-my-email", hides),
        ("account-vacation", auto_reply),
    ]
}

/// Whether the account action `name` runs for an account that offers
/// `offers`. An account's menu reaches these through its own actions
/// ([`account_menu_actions`]), which are off where the account lacks what
/// they open, but the demo's script activates the `win.` action with any
/// account's id. Hide My Email needs masked addresses, or rules and
/// labels ([`hides_addresses`]).
pub fn account_action_on(name: &str, offers: Offers) -> bool {
    match name {
        "account-rules" => offers.rules,
        "account-hide-my-email" => hides_addresses(offers),
        "account-vacation" => offers.auto_reply,
        _ => true,
    }
}

/// The actions an account's own menu holds under the `account` prefix,
/// and whether each is on for an account that offers `offers`. A
/// parameterized `win.` action serves every account, so it cannot be off
/// for one; these belong to one account's row, so each is off where that
/// account lacks what it opens. Hide My Email needs masked addresses or
/// a rule for each address ([`hides_addresses`]).
pub fn account_menu_actions(offers: Offers) -> [(&'static str, bool); 3] {
    [
        ("vacation", offers.auto_reply),
        ("rules", offers.rules),
        ("hide-my-email", hides_addresses(offers)),
    ]
}

/// Whether Hide My Email works for an account: a server that makes
/// masked addresses, or a plus address with a rule behind it on an
/// account that keeps rules and labels.
pub fn hides_addresses(offers: Offers) -> bool {
    offers.masked_email || (offers.rules && offers.labels)
}

/// How the accounts on screen file mail: with labels, several at once, or
/// in folders, one at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filing {
    Labels,
    Folders,
}

impl Filing {
    /// Folders when every account in question files in folders, labels
    /// otherwise, including when there is no account in question.
    pub fn of(offers: impl IntoIterator<Item = Offers>) -> Filing {
        let mut any = false;
        for offer in offers {
            if offer.labels {
                return Filing::Labels;
            }
            any = true;
        }
        if any { Filing::Folders } else { Filing::Labels }
    }

    /// How mail from several accounts can be filed at once: by label
    /// name when every account files with labels. Adding a label on a
    /// folder account copies the mail into the folder and leaves it where
    /// it was, so one folder account makes it folders, and the picker asks
    /// for mail from one account instead.
    pub fn across(offers: impl IntoIterator<Item = Offers>) -> Filing {
        match offers.into_iter().all(|offer| offer.labels) {
            true => Filing::Labels,
            false => Filing::Folders,
        }
    }

    /// How the Labels button words itself, which is how the picker it
    /// opens words itself. `reached` are the accounts of the mail it acts
    /// on, one entry per row; `shown` the accounts the mailbox on screen
    /// lists. With nothing reached the mailbox's accounts decide, as the
    /// picker's "Open or select mail" line does; mail from several
    /// accounts reads as it files across them.
    pub fn picker(
        reached: &[AccountId],
        shown: &[AccountId],
        offers: impl Fn(AccountId) -> Offers,
    ) -> Filing {
        let mut accounts = reached.to_vec();
        accounts.sort_unstable();
        accounts.dedup();
        match accounts.as_slice() {
            [] => Filing::of(shown.iter().map(|id| offers(*id))),
            [one] => Filing::of([offers(*one)]),
            many => Filing::across(many.iter().map(|id| offers(*id))),
        }
    }

    /// The Keyboard Shortcuts line for the key that opens the picker.
    pub fn shortcut_line(self) -> String {
        match self {
            Filing::Labels => gettext("Labels"),
            Filing::Folders => gettext("Move to folder"),
        }
    }

    pub fn menu_item(self) -> String {
        match self {
            Filing::Labels => gettext("Labels…"),
            Filing::Folders => gettext("Move to Folder…"),
        }
    }

    /// The name and tooltip of the arrow beside the header button. It
    /// differs from the button's own, so a screen reader does not say
    /// "Labels" twice.
    pub fn arrow(self) -> String {
        match self {
            Filing::Labels => gettext("Choose Labels"),
            Filing::Folders => gettext("Choose a Folder"),
        }
    }

    /// The header button's tooltip, with its key.
    pub fn tooltip(self) -> String {
        match self {
            Filing::Labels => gettext("Labels (L)"),
            Filing::Folders => gettext("Move to Folder (L)"),
        }
    }

    /// The header button's icon: the tag Gmail's labels wear, since mail
    /// there can carry several at once, or the plain folder icon once
    /// every account in question keeps mail in one place at a time.
    /// `ui::sidebar::label_icon` draws the same choice for a sidebar row.
    pub fn icon(self) -> &'static str {
        match self {
            Filing::Labels => "penguin-mail-tag-symbolic",
            Filing::Folders => "folder-symbolic",
        }
    }

    pub fn new_item(self) -> String {
        match self {
            Filing::Labels => gettext("New Label…"),
            Filing::Folders => gettext("New Folder…"),
        }
    }

    /// The menu item that opens that dialog with this one's name and a
    /// slash already typed, so the new one nests under it.
    pub fn new_inside_item(self) -> String {
        match self {
            Filing::Labels => gettext("New Label Inside…"),
            Filing::Folders => gettext("New Folder Inside…"),
        }
    }

    /// What the button at the end of a sidebar row says out loud.
    pub fn options_name(self, name: &str) -> String {
        match self {
            Filing::Labels => fill(&gettext("Label Options for {name}"), &[("name", name)]),
            Filing::Folders => fill(&gettext("Folder Options for {name}"), &[("name", name)]),
        }
    }

    /// That button's tooltip.
    pub fn options_tooltip(self) -> String {
        match self {
            Filing::Labels => gettext("Label options"),
            Filing::Folders => gettext("Folder options"),
        }
    }

    /// Why one dragged into its own subtree stays put.
    pub fn inside_itself(self) -> String {
        match self {
            Filing::Labels => gettext("A label cannot go inside a label nested under it"),
            Filing::Folders => gettext("A folder cannot go inside a folder nested under it"),
        }
    }

    /// Why one dragged beside or into a namesake stays put.
    pub fn name_taken(self, name: &str) -> String {
        let name = name.replace('/', " › ");
        match self {
            Filing::Labels => fill(
                &gettext("There is a label called “{name}” already"),
                &[("name", &name)],
            ),
            Filing::Folders => fill(
                &gettext("There is a folder called “{name}” already"),
                &[("name", &name)],
            ),
        }
    }

    /// The heading of the dialog that asks for a new one's name.
    pub fn new_heading(self) -> String {
        match self {
            Filing::Labels => gettext("New Label"),
            Filing::Folders => gettext("New Folder"),
        }
    }

    /// What that dialog says when the server refuses, with `{reason}`
    /// still to fill.
    pub fn create_failed(self) -> String {
        match self {
            Filing::Labels => gettext("Could not create the label: {reason}"),
            Filing::Folders => gettext("Could not create the folder: {reason}"),
        }
    }

    /// What the picker says when no mail is open or selected.
    pub fn nothing_picked(self) -> String {
        match self {
            Filing::Labels => gettext("Open or select mail to label it."),
            Filing::Folders => gettext("Open or select mail to move it."),
        }
    }

    /// What the picker says for mail from several accounts that it cannot
    /// file in one go.
    pub fn one_account_only(self) -> String {
        match self {
            Filing::Labels => gettext("Select mail from one account to label it."),
            Filing::Folders => gettext("Select mail from one account to move it."),
        }
    }

    pub fn rename_heading(self) -> String {
        match self {
            Filing::Labels => gettext("Rename Label"),
            Filing::Folders => gettext("Rename Folder"),
        }
    }

    pub fn rename_body(self) -> String {
        match self {
            Filing::Labels => gettext("Labels nested under it move along."),
            Filing::Folders => gettext("Folders nested under it move along."),
        }
    }

    /// What renaming says when the server refuses, with `{reason}` still
    /// to fill.
    pub fn rename_failed(self) -> String {
        match self {
            Filing::Labels => gettext("Could not rename the label: {reason}"),
            Filing::Folders => gettext("Could not rename the folder: {reason}"),
        }
    }

    /// What deleting one does to its mail. A label comes off the mail and
    /// the mail stays in `provider`; a folder holds the only copy, and its
    /// mail goes with it. An empty provider names no account.
    pub fn delete_body(self, provider: &str) -> String {
        match self {
            Filing::Labels if !provider.is_empty() => fill(
                &gettext(
                    "Its mail stays in {provider}, without the label. Nested labels stay too.",
                ),
                &[("provider", provider)],
            ),
            Filing::Labels => {
                gettext("Its mail stays in the account, without the label. Nested labels stay too.")
            }
            Filing::Folders => gettext("The mail in the folder is deleted with it."),
        }
    }

    /// What deleting says when the server refuses, with `{reason}` still
    /// to fill.
    pub fn delete_failed(self) -> String {
        match self {
            Filing::Labels => gettext("Could not delete the label: {reason}"),
            Filing::Folders => gettext("Could not delete the folder: {reason}"),
        }
    }

    /// What the picker says when the account has nothing to file in yet.
    pub fn none_yet(self) -> String {
        match self {
            Filing::Labels => gettext("This account has no labels yet."),
            Filing::Folders => gettext("This account has no folders yet."),
        }
    }
}

/// One line saying why `account` lacks `missing`, naming who serves it.
/// `missed` is why the last search found no calendar or contacts server
/// for an IMAP or POP3 account, when the store kept a reason.
pub fn reason(account: &Account, missing: Missing, missed: Option<Miss>) -> String {
    let template = match (account.provider, missing) {
        // IMAP carries mail; a calendar and contacts come from CalDAV and
        // CardDAV servers the app looks for, and Preferences says where
        // it looked. Rules always have a place, on the server or here.
        // A Microsoft account offers every service; one it lacks is one
        // its organization refused (`Refused`).
        (Provider::Microsoft, Missing::Calendar) => {
            gettext("Your organization does not allow Penguin Mail to use this calendar.")
        }
        (Provider::Microsoft, Missing::Contacts) => {
            gettext("Your organization does not allow Penguin Mail to read these contacts.")
        }
        (Provider::Microsoft, Missing::Rules) => {
            gettext("Your organization does not allow Penguin Mail to change these rules.")
        }
        (Provider::Microsoft, Missing::AutoReply) => {
            gettext("Your organization does not allow Penguin Mail to change the automatic reply.")
        }
        (Provider::Imap | Provider::Pop3, Missing::Calendar) => match missed {
            Some(Miss::Refused) => gettext(
                "{provider} refused the password for calendars. Some providers need an app \
                 password with access to calendars and contacts.",
            ),
            Some(Miss::Unreachable) => {
                gettext("Penguin Mail could not reach the calendar server for {provider}.")
            }
            Some(Miss::NotFound) | None => {
                gettext("Penguin Mail found no calendar server for {provider}.")
            }
        },
        (Provider::Imap | Provider::Pop3, Missing::Contacts) => match missed {
            Some(Miss::Refused) => gettext(
                "{provider} refused the password for contacts. Some providers need an app \
                 password with access to calendars and contacts.",
            ),
            Some(Miss::Unreachable) => {
                gettext("Penguin Mail could not reach the contacts server for {provider}.")
            }
            Some(Miss::NotFound) | None => {
                gettext("Penguin Mail found no contacts server for {provider}.")
            }
        },
        (Provider::Pop3, Missing::AutoReply) => {
            gettext("{provider} cannot send automatic replies over POP3.")
        }
        (Provider::Imap, Missing::AutoReply) => {
            gettext("{provider} cannot send automatic replies.")
        }
        // A calendar or contacts server beside a browser sign-in that
        // refused its token waits for an app password.
        (Provider::Jmap, Missing::Calendar) if missed == Some(Miss::Refused) => gettext(
            "{provider}'s calendar server refused the browser sign-in. Choose Sign In Again and give an app password for the calendar.",
        ),
        (Provider::Jmap, Missing::Contacts) if missed == Some(Miss::Refused) => gettext(
            "{provider}'s contacts server refused the browser sign-in. Choose Sign In Again and give an app password for the calendar.",
        ),
        // Penguin Mail reads mail over JMAP and nothing else yet, so what
        // the server may well offer is not set up on the account.
        (Provider::Jmap, Missing::Calendar) => {
            gettext("Penguin Mail cannot show {provider}'s calendar over JMAP yet.")
        }
        (Provider::Jmap, Missing::Contacts) => {
            gettext("Penguin Mail cannot read {provider}'s contacts over JMAP yet.")
        }
        (Provider::Jmap, Missing::Rules) => {
            gettext("Penguin Mail cannot change {provider}'s rules over JMAP yet.")
        }
        (Provider::Jmap, Missing::AutoReply) => {
            gettext("Penguin Mail cannot change {provider}'s automatic reply over JMAP yet.")
        }
        (_, Missing::Calendar) => gettext("{provider} has no calendar that other apps can reach."),
        (_, Missing::Contacts) => {
            gettext("{provider} keeps no contacts that other apps can reach.")
        }
        (_, Missing::Rules) => gettext("{provider} has no rules that other apps can change."),
        (_, Missing::AutoReply) => {
            gettext("{provider} has no automatic reply that other apps can change.")
        }
        (_, Missing::DeleteForever) => {
            gettext("{provider} cannot delete mail for good. Delete moves it to the Trash.")
        }
        (_, Missing::Categories) => gettext("{provider} does not sort the inbox into categories."),
    };
    let provider = mailrs_discover::resolved_provider_name(account.provider_name());
    fill(&template, &[("provider", &provider)])
}

/// Where Gmail keeps the filters and the automatic reply, for an account
/// that reads its mail over IMAP and so cannot change either.
pub const FILTERS_LINK: &str = "https://mail.google.com/mail/#settings/filters";
pub const SETTINGS_LINK: &str = "https://mail.google.com/mail/#settings/general";

/// What an IMAP-route Gmail account says of its filters.
pub fn filters_gap() -> String {
    // Translators: Gmail and gmail.com are names; keep them as they are.
    gettext("Your Gmail filters still run. Change them on gmail.com.")
}

/// What an IMAP-route Gmail account says of its automatic reply.
pub fn reply_gap() -> String {
    // Translators: gmail.com is a name; keep it as it is.
    gettext("Set your automatic reply on gmail.com.")
}

/// The link under [`filters_gap`].
pub fn filters_link_label() -> String {
    // Translators: Gmail is a name; keep it as it is.
    gettext("Open Gmail Filters")
}

/// The link under [`reply_gap`].
pub fn settings_link_label() -> String {
    // Translators: Gmail is a name; keep it as it is.
    gettext("Open Gmail Settings")
}

/// [`reason`] for an account that may read its mail over IMAP. A Gmail
/// account on that route has no rules or automatic reply to change
/// here, and its words point to gmail.com; every other case reads as
/// [`reason`] does.
pub fn reason_on(
    account: &Account,
    route: MailRoute,
    missing: Missing,
    missed: Option<Miss>,
) -> String {
    if account.provider == Provider::Gmail && route == MailRoute::Imap {
        match missing {
            Missing::Rules => return filters_gap(),
            Missing::AutoReply => return reply_gap(),
            _ => {}
        }
    }
    reason(account, missing, missed)
}

/// Whether a Gmail account's calendar and contacts reach Penguin Mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Calendar {
    /// They read, or the account is not a Gmail account.
    Reads,
    /// The account holds no calendar token, as an account whose mail
    /// comes over IMAP does until its calendar consent.
    NotConnected,
}

impl Calendar {
    pub fn shows_connect(self) -> bool {
        self != Calendar::Reads
    }
}

/// How `account`'s Google calendar and contacts stand, from what its
/// services offer.
pub fn calendar_link(account: &Account, offers: Offers) -> Calendar {
    match account.provider {
        Provider::Gmail if !offers.calendar => Calendar::NotConnected,
        _ => Calendar::Reads,
    }
}

/// The rows Preferences shows under Not Available: one for each account
/// that lacks something, as its address and one reason for each thing it
/// lacks. The category bar needs no reason because it is not there.
/// `missed` says why the search for an account's calendar or contacts
/// server failed, where the store kept a reason.
/// Each Gmail account's route says where its calendar, contacts, filters
/// and automatic reply are: on the IMAP route they have rows of their own
/// in Preferences, so they stay out of the account's Not Available row.
pub fn missing_lines(
    accounts: &[(Account, Offers)],
    missed: impl Fn(AccountId, Missing) -> Option<Miss>,
    route_of: impl Fn(AccountId) -> Option<MailRoute>,
) -> Vec<(String, Vec<String>)> {
    accounts
        .iter()
        .filter_map(|(account, offers)| {
            let route = route_of(account.id).unwrap_or_default();
            let own_rows = account.provider == Provider::Gmail && route == MailRoute::Imap;
            let mut reasons: Vec<String> = offers
                .missing()
                .into_iter()
                .filter(|m| *m != Missing::Categories)
                .filter(|m| {
                    !(own_rows
                        && matches!(
                            m,
                            Missing::Calendar
                                | Missing::Contacts
                                | Missing::Rules
                                | Missing::AutoReply
                        ))
                })
                .map(|m| match (m, offers.auto_reply_needs_sign_in) {
                    (Missing::AutoReply, true) => sign_in_reason(account),
                    _ => reason_on(account, route, m, missed(account.id, m)),
                })
                .collect();
            reasons.dedup();
            (!reasons.is_empty()).then(|| (account.email.clone(), reasons))
        })
        .collect()
}

/// Why a JMAP account signed in with a token has no automatic reply.
pub fn sign_in_reason(account: &Account) -> String {
    let provider = mailrs_discover::resolved_provider_name(account.provider_name());
    fill(
        &gettext(
            "{provider} does not let an API token change the automatic reply. Sign in again through the browser to change it here.",
        ),
        &[("provider", &provider)],
    )
}

/// What a screen reader calls a Not Available row: the account's address
/// and every reason under it.
pub fn missing_name(address: &str, reasons: &[String]) -> String {
    fill(
        &gettext("{address}: {reason}"),
        &[("address", address), ("reason", &reasons.join(" "))],
    )
}

/// One line per message a POP3 server would not hand over after three
/// tries: its subject and sender where a `TOP` read them, else its place
/// in the list, then why, in the server's own words for a refusal.
pub fn failing_lines(failing: &[Failing]) -> Vec<String> {
    failing
        .iter()
        .enumerate()
        .map(|(n, failed)| {
            let words = match failed.reason {
                FailReason::Refused if !failed.words.is_empty() => failed.words.clone(),
                FailReason::Refused => gettext("The server would not hand it over."),
                FailReason::TooLarge => {
                    gettext("The message is larger than Penguin Mail downloads.")
                }
                FailReason::Unreadable => {
                    gettext("Penguin Mail could not read the server's answer.")
                }
                FailReason::Dropped => {
                    gettext("The connection closed while the message downloaded.")
                }
            };
            let number = (n + 1).to_string();
            match (failed.subject.as_deref(), failed.sender.as_deref()) {
                (Some(subject), Some(sender)) => fill(
                    &gettext("“{subject}” from {sender}: {words}"),
                    &[("subject", subject), ("sender", sender), ("words", &words)],
                ),
                (Some(subject), None) => fill(
                    &gettext("“{subject}”: {words}"),
                    &[("subject", subject), ("words", &words)],
                ),
                (None, Some(sender)) => fill(
                    &gettext("A message from {sender}: {words}"),
                    &[("sender", sender), ("words", &words)],
                ),
                (None, None) => fill(
                    &gettext("Message {number}: {words}"),
                    &[("number", &number), ("words", &words)],
                ),
            }
        })
        .collect()
}

/// What Remove Account's confirmation says happens to `account`'s mail,
/// or to a calendar account's calendars.
/// `remove` is a POP3 account's removal setting. Under one that removes
/// mail from the server, the server has let go of what came down, so
/// removing the account loses it.
pub fn remove_account_body(account: &Account, remove: RemoveSetting) -> String {
    match (account.provider, remove) {
        (Provider::Pop3, RemoveSetting::Never) => gettext(
            "Removing the account deletes this computer's copy of its mail and the \
             saved sign-in. Mail still on the server stays there.",
        ),
        (Provider::Pop3, _) => gettext(
            "Mail already removed from the server exists only on this computer, \
             and removing the account deletes it with the saved sign-in. Mail \
             still on the server stays there.",
        ),
        (Provider::CalDav, _) => gettext(
            "Its calendars and saved sign-in are deleted from this computer. \
             Nothing changes on the server.",
        ),
        _ => fill(
            &gettext(
                "Its downloaded mail and saved sign-in are deleted from this computer. \
                 Nothing changes in {provider}.",
            ),
            &[("provider", account.provider_name())],
        ),
    }
}

/// What Remove Account's confirmation adds when the account's server
/// holds `count` Send Later messages: they are called back first. `None`
/// when it holds none.
pub fn held_back_line(count: usize) -> Option<String> {
    (count > 0).then(|| {
        fill_plural(
            "{count} message waiting in Send Later on the server goes back to Drafts first.",
            "{count} messages waiting in Send Later on the server go back to Drafts first.",
            count,
            &[("count", &count.to_string())],
        )
    })
}

/// What Remove Account asks before it goes ahead when `count` held
/// messages would not come back.
pub fn still_held_line(count: usize) -> String {
    fill_plural(
        "Penguin Mail could not call back {count} message waiting on the server. The server may still send it at its hour.",
        "Penguin Mail could not call back {count} messages waiting on the server. The server may still send them at their hour.",
        count,
        &[("count", &count.to_string())],
    )
}

/// About's line for each account whose mail lives only in the store.
pub fn kept_here_lines(accounts: &[Account]) -> Vec<String> {
    accounts
        .iter()
        .filter(|account| account.provider == Provider::Pop3)
        .map(|account| {
            fill(
                &gettext(
                    "Mail from {address} is kept only on this computer. To keep a copy, quit Penguin Mail, then back up this file.",
                ),
                &[("address", &account.email)],
            )
        })
        .collect()
}

/// Whether the app asks the server which addresses `account` sends as.
/// Gmail keeps send-as addresses with their names, and a JMAP server its
/// identities. An IMAP server keeps neither, and asking it would replace
/// the name the person typed when adding the account with none.
pub fn reads_send_as(account: &Account) -> bool {
    matches!(account.provider, Provider::Gmail | Provider::Jmap)
}

/// One row of the Switch to JMAP dialog: an icon, what it is, and a line
/// on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchRow {
    pub icon: &'static str,
    pub title: String,
    pub subtitle: String,
}

/// What Switch to JMAP changes and what it keeps, as the dialog's rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchRows {
    pub changes: Vec<SwitchRow>,
    pub stays: Vec<SwitchRow>,
}

fn switch_row(icon: &'static str, title: String, subtitle: String) -> SwitchRow {
    SwitchRow {
        icon,
        title,
        subtitle,
    }
}

/// The dialog's rows for `account`. `calendars` says the account reads
/// calendars from a CalDAV server. Where the JMAP sign-in takes a `token`,
/// a line says the app password those calendars sign in with stays, as at
/// Fastmail, whose JMAP serves no calendar. Where it takes the password,
/// the server may serve calendars over JMAP, and a line says the switch
/// then reads them again from there.
pub fn switch_rows(account: &Account, calendars: bool, token: bool) -> SwitchRows {
    let provider = mailrs_discover::resolved_provider_name(account.provider_name());
    let mut changes = vec![
        switch_row(
            "penguin-mail-inbox-symbolic",
            gettext("New mail arrives as it is sent"),
            fill(
                &gettext("{provider} pushes it over JMAP, and you can send from your aliases."),
                &[("provider", &provider)],
            ),
        ),
        switch_row(
            "weather-overcast-symbolic",
            gettext("Mail is matched over JMAP"),
            gettext(
                "Penguin Mail keeps the mail on this computer and downloads again only the messages it cannot match.",
            ),
        ),
    ];
    let mut stays = vec![
        switch_row(
            "penguin-mail-person-symbolic",
            gettext("Name, color and place"),
            fill(
                &gettext("{address} keeps its spot in the sidebar."),
                &[("address", &account.email)],
            ),
        ),
        switch_row(
            "penguin-mail-flag-outline-symbolic",
            gettext("Your own data"),
            gettext("Reminders, follow-ups, flags, rules, VIPs, signatures and drafts."),
        ),
    ];
    if calendars && !token {
        changes.push(switch_row(
            "penguin-mail-calendar-symbolic",
            gettext("Calendars are read again where JMAP serves them"),
            fill(
                &gettext(
                    "If {provider} offers calendars over JMAP, Penguin Mail reads them from there.",
                ),
                &[("provider", &provider)],
            ),
        ));
    }
    if calendars && token {
        stays.push(switch_row(
            "penguin-mail-calendar-symbolic",
            gettext("Calendar sign-in"),
            gettext("The app password you use for calendars stays as it is."),
        ));
    }
    SwitchRows { changes, stays }
}

/// What the switch dialog says when the switch did not go through, and
/// whether it offers Try Again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchFailure {
    pub line: String,
    /// The server's mail kept changing while the switch matched it, and
    /// another try may find it still.
    pub again: bool,
}

/// What the switch dialog asked for, which a failure's words name.
#[derive(Debug, Clone, Copy)]
pub struct SwitchAsked<'a> {
    pub provider: &'a str,
    pub address: &'a str,
    /// The field takes a token rather than a password.
    pub token: bool,
    /// The server also offers a sign-in in the browser.
    pub browser: bool,
}

/// The failure toast's line for `reason`, a whole sentence.
fn switch_line(reason: &str) -> String {
    format!(
        "{} {}",
        with_reason(&gettext("Could not switch to JMAP: {reason}"), &reason, &[]),
        gettext("Your IMAP account is unchanged.")
    )
}

/// The words for a failure the switch has no sentence of its own for.
/// Its text may hold an address or a server's own words, so it goes to
/// the log as a kind, and the toast says a fixed sentence.
fn unexpected(err: &anyhow::Error) -> String {
    tracing::warn!(
        kind = crate::add_account::failure_kind(err),
        "the switch to JMAP failed"
    );
    gettext("Something unexpected stopped it.")
}

fn does_not_answer(provider: &str) -> String {
    fill(
        &gettext("{provider} does not answer."),
        &[("provider", provider)],
    )
}

fn cannot_use_answer(provider: &str) -> String {
    fill(
        &gettext("{provider} gave an answer Penguin Mail cannot use. Try again later."),
        &[("provider", provider)],
    )
}

/// The failure toast's words for `err` from `Core::switch_to_jmap`.
pub fn switch_failure(err: &anyhow::Error, asked: &SwitchAsked) -> SwitchFailure {
    use mailrs_jmap::JmapError;
    use mailrs_sync::switch::SwitchError;
    let (provider, token) = (asked.provider, asked.token);
    let named = [("provider", provider)];
    let mut again = false;
    let reason = match err.downcast_ref::<SwitchError>() {
        Some(SwitchError::NotImap) => {
            return SwitchFailure {
                line: gettext("This account uses JMAP already."),
                again,
            };
        }
        Some(SwitchError::SignIn(JmapError::Unauthorized)) => {
            let refused = match token {
                true => fill(&gettext("{provider} did not accept the token."), &named),
                false => fill(&gettext("{provider} did not accept the password."), &named),
            };
            match asked.browser {
                true => format!(
                    "{refused} {}",
                    gettext("You can choose Sign In With Browser instead.")
                ),
                false => refused,
            }
        }
        Some(SwitchError::OtherAccount) => fill(
            &gettext("{provider} signed in to another account, not {address}."),
            &[("provider", provider), ("address", asked.address)],
        ),
        Some(SwitchError::SignIn(JmapError::BadCredential)) => match token {
            true => gettext("The token holds a character that cannot be sent."),
            false => gettext("The password holds a character that cannot be sent."),
        },
        Some(SwitchError::SignIn(JmapError::Network(_))) => does_not_answer(provider),
        Some(SwitchError::SignIn(_)) => cannot_use_answer(provider),
        Some(SwitchError::Drafts(_)) => fill(
            &gettext("{provider} did not give the draft of a message waiting in Send Later."),
            &named,
        ),
        Some(SwitchError::Sync(SyncError::Backend(BackendError::Changed))) => {
            again = true;
            fill(
                &gettext("The mail on {provider} kept changing while Penguin Mail matched it."),
                &named,
            )
        }
        _ => unexpected(err),
    };
    SwitchFailure {
        line: switch_line(&reason),
        again,
    }
}

/// The failure toast's words when Sign In With Browser did not end in a
/// grant, or None when the person canceled it.
pub fn switch_browser_failure(err: &anyhow::Error, provider: &str) -> Option<SwitchFailure> {
    use mailrs_jmap::JmapError;
    use mailrs_sync::sign_in::BrowserSignInError;
    let named = [("provider", provider)];
    let reason = match err.downcast_ref::<BrowserSignInError>() {
        Some(BrowserSignInError::Canceled) => return None,
        Some(BrowserSignInError::Jmap(
            JmapError::Unauthorized
            | JmapError::BadCredential
            | JmapError::UnknownClient
            | JmapError::Forbidden(_),
        )) => fill(
            &gettext("{provider} refused the sign-in in the browser."),
            &named,
        ),
        Some(BrowserSignInError::Jmap(JmapError::Declined)) => fill(
            &gettext("You did not allow Penguin Mail on {provider}'s page."),
            &named,
        ),
        Some(BrowserSignInError::Jmap(JmapError::Network(_))) => does_not_answer(provider),
        Some(BrowserSignInError::Jmap(_)) => cannot_use_answer(provider),
        _ => unexpected(err),
    };
    Some(SwitchFailure {
        line: switch_line(&reason),
        again: false,
    })
}

/// The window's toast once `address` has switched, with the local rules
/// the switch turned off because the server lacks their folders.
pub fn switched_line(address: &str, rules_off: usize) -> String {
    let named = [("address", address)];
    if rules_off == 0 {
        return fill(&gettext("{address} now uses JMAP"), &named);
    }
    let count = rules_off.to_string();
    fill_plural(
        "{address} now uses JMAP. {count} rule is off because its folder is not on the server.",
        "{address} now uses JMAP. {count} rules are off because their folders are not on the server.",
        rules_off,
        &[("address", address), ("count", &count)],
    )
}

#[cfg(test)]
mod tests {
    use mailrs_domain::{Account, AccountState, Provider, RemoveSetting};
    use mailrs_store::pop3::{FailReason, Failing};
    use mailrs_store::services::Miss;
    use mailrs_sync::{Missing, Offers};

    use super::{
        Calendar, calendar_link, failing_lines, held_back_line, hides_addresses, kept_here_lines,
        offers_for, reason, reason_on, remove_account_body, shows_space_switch, still_held_line,
        withheld_for,
    };
    use crate::settings::Space;
    use mailrs_domain::MailRoute;

    fn gmail_in(state: AccountState) -> Account {
        Account {
            id: 8,
            email: "maria.costa@gmail.com".into(),
            state,
            provider: Provider::Gmail,
            provider_name: Some("Gmail".into()),
        }
    }

    #[test]
    fn the_reply_gap_reads_the_same_in_the_window() {
        let account = gmail_in(AccountState::Ok);
        assert_eq!(
            reason_on(&account, MailRoute::Imap, Missing::AutoReply, None),
            "Set your automatic reply on gmail.com."
        );
        assert_eq!(
            reason_on(&account, MailRoute::Imap, Missing::Rules, None),
            "Your Gmail filters still run. Change them on gmail.com."
        );
    }

    #[test]
    fn the_api_route_and_other_gaps_keep_their_old_words() {
        let account = gmail_in(AccountState::Ok);
        assert_eq!(
            reason_on(&account, MailRoute::GmailApi, Missing::AutoReply, None),
            reason(&account, Missing::AutoReply, None)
        );
        assert_eq!(
            reason_on(&account, MailRoute::Imap, Missing::DeleteForever, None),
            reason(&account, Missing::DeleteForever, None)
        );
    }

    #[test]
    fn an_account_without_a_calendar_asks_to_connect_it() {
        assert_eq!(
            calendar_link(&gmail_in(AccountState::Ok), NO_CALENDAR),
            Calendar::NotConnected
        );
        assert_eq!(
            calendar_link(&gmail_in(AccountState::Ok), Offers::EVERYTHING),
            Calendar::Reads
        );
        assert_eq!(calendar_link(&pop3_account(), NO_CALENDAR), Calendar::Reads);
    }

    #[test]
    fn an_imap_route_account_leaves_what_its_own_rows_say_out_of_not_available() {
        let account = gmail_in(AccountState::Ok);
        let lacking = Offers {
            calendar: false,
            contacts: false,
            rules: false,
            auto_reply: false,
            ..Offers::EVERYTHING
        };
        let on_imap = missing_lines(
            &[(account.clone(), lacking)],
            |_, _| None,
            |_| Some(MailRoute::Imap),
        );
        assert_eq!(on_imap, []);
        let on_api = missing_lines(
            &[(account, lacking)],
            |_, _| None,
            |_| Some(MailRoute::GmailApi),
        );
        assert_eq!(on_api.len(), 1);
    }

    const NO_CALENDAR: Offers = Offers {
        calendar: false,
        ..Offers::EVERYTHING
    };

    fn pop3_account() -> Account {
        Account {
            id: 5,
            email: "dana@example.org".into(),
            state: AccountState::Ok,
            provider: Provider::Pop3,
            provider_name: Some("example.org".into()),
        }
    }

    #[test]
    fn a_pop3_account_says_why_it_has_no_automatic_reply() {
        assert_eq!(
            reason(&pop3_account(), Missing::AutoReply, None),
            "example.org cannot send automatic replies over POP3."
        );
        assert_eq!(
            reason(&pop3_account(), Missing::Calendar, None),
            "Penguin Mail found no calendar server for example.org."
        );
    }

    fn failed(
        reason: FailReason,
        words: &str,
        sender: Option<&str>,
        subject: Option<&str>,
    ) -> Failing {
        Failing {
            uidl: "u1".into(),
            reason,
            words: words.into(),
            sender: sender.map(Into::into),
            subject: subject.map(Into::into),
        }
    }

    #[test]
    fn each_message_that_will_not_download_reads_with_its_name_and_reason() {
        let failing = [
            failed(
                FailReason::Refused,
                "no such message",
                Some("Ana Lima"),
                Some("Photos"),
            ),
            failed(FailReason::TooLarge, "", None, Some("Scans")),
            failed(FailReason::Dropped, "", Some("Bo"), None),
            failed(FailReason::Unreadable, "", None, None),
            failed(FailReason::Refused, "", None, None),
        ];
        assert_eq!(
            failing_lines(&failing),
            [
                "“Photos” from Ana Lima: no such message",
                "“Scans”: The message is larger than Penguin Mail downloads.",
                "A message from Bo: The connection closed while the message downloaded.",
                "Message 4: Penguin Mail could not read the server's answer.",
                "Message 5: The server would not hand it over.",
            ]
        );
    }

    #[test]
    fn removing_a_pop3_account_says_what_its_removal_setting_left_on_the_server() {
        let kept = "Removing the account deletes this computer's copy of its mail and the \
                    saved sign-in. Mail still on the server stays there.";
        let only_here = "Mail already removed from the server exists only on this computer, \
                         and removing the account deletes it with the saved sign-in. Mail \
                         still on the server stays there.";
        let account = pop3_account();
        assert_eq!(remove_account_body(&account, RemoveSetting::Never), kept);
        assert_eq!(
            remove_account_body(&account, RemoveSetting::Downloaded),
            only_here
        );
        assert_eq!(
            remove_account_body(&account, RemoveSetting::Days(30)),
            only_here
        );
    }

    #[test]
    fn removing_an_account_says_how_many_messages_the_server_holds() {
        assert_eq!(held_back_line(0), None);
        assert_eq!(
            held_back_line(1).as_deref(),
            Some("1 message waiting in Send Later on the server goes back to Drafts first.")
        );
        assert_eq!(
            held_back_line(3).as_deref(),
            Some("3 messages waiting in Send Later on the server go back to Drafts first.")
        );
    }

    #[test]
    fn removing_an_account_warns_of_holds_that_would_not_come_back() {
        assert_eq!(
            still_held_line(1),
            "Penguin Mail could not call back 1 message waiting on the server. The server may still send it at its hour."
        );
        assert_eq!(
            still_held_line(2),
            "Penguin Mail could not call back 2 messages waiting on the server. The server may still send them at their hour."
        );
    }

    #[test]
    fn removing_another_account_names_its_own_service() {
        let named = |provider, name: Option<&str>| Account {
            provider,
            provider_name: name.map(Into::into),
            ..pop3_account()
        };
        for (account, service) in [
            (named(Provider::Gmail, None), "Gmail"),
            (named(Provider::Imap, Some("Fastmail")), "Fastmail"),
            (
                named(Provider::Microsoft, Some("Outlook.com")),
                "Outlook.com",
            ),
        ] {
            assert_eq!(
                remove_account_body(&account, RemoveSetting::Never),
                format!(
                    "Its downloaded mail and saved sign-in are deleted from this computer. \
                     Nothing changes in {service}."
                )
            );
        }
    }

    fn calendar_account() -> Account {
        Account {
            id: 7,
            email: "robin@cloud.example.org".into(),
            state: AccountState::Ok,
            provider: Provider::CalDav,
            provider_name: None,
        }
    }

    /// A calendar account's server says whether it sends replies once
    /// its calendars are read, which is after the window read what the
    /// account offers.
    #[tokio::test]
    async fn whether_the_organizer_hears_is_read_from_the_services_as_they_stand() {
        use mailrs_sync::fake::{DavKind, FakeDav};
        use mailrs_sync::{AccountServices, CalendarService};

        let dav = std::sync::Arc::new(FakeDav::new());
        dav.add_collection("/cal/personal/", DavKind::Calendar, "Personal", None);
        dav.set_auto_schedule(true);
        let services = AccountServices::calendar_only(std::sync::Arc::clone(&dav));

        let calendars = Some(Provider::CalDav);
        assert!(
            !super::organizer_hears(calendars, None),
            "a calendar account that has not started is not heard"
        );
        assert!(
            super::organizer_hears(Some(Provider::Imap), None),
            "an account with mail sends the reply once it starts"
        );
        assert!(
            !super::organizer_hears(calendars, Some(&services)),
            "a server that has not said it sends replies is taken to send none"
        );
        services
            .calendar
            .as_ref()
            .expect("the calendar")
            .calendars()
            .await
            .expect("the calendar list");
        assert!(
            super::organizer_hears(calendars, Some(&services)),
            "the server has said it sends them"
        );
    }

    #[test]
    fn a_proposal_needs_mail_to_carry_it() {
        use mailrs_sync::AccountServices;
        use mailrs_sync::fake::{FakeDav, FakeGmail};

        let with_mail = AccountServices::google(std::sync::Arc::new(FakeGmail::new()));
        let alone = AccountServices::calendar_only(std::sync::Arc::new(FakeDav::new()));
        let calendars = Some(Provider::CalDav);
        assert!(super::can_propose(Some(Provider::Gmail), Some(&with_mail)));
        assert!(
            !super::can_propose(calendars, Some(&alone)),
            "a calendar account has no mail to send it"
        );
        assert!(
            !super::can_propose(calendars, None),
            "nor one that has not started"
        );
        assert!(
            super::can_propose(Some(Provider::Imap), None),
            "an account with mail that has not started is not held back"
        );
        assert!(
            super::can_propose(None, None),
            "nor one the window does not know"
        );
    }

    #[test]
    fn a_window_with_calendar_accounts_alone_has_no_mail_to_show() {
        use crate::settings::Space;

        assert!(super::only_calendars(&[calendar_account()]));
        assert!(!super::only_calendars(&[
            calendar_account(),
            pop3_account()
        ]));
        assert!(
            !super::only_calendars(&[]),
            "no account at all is the first-run page's business"
        );

        let switch_hidden = false;
        let (calendars_alone, with_mail) =
            ([calendar_account()], [calendar_account(), pop3_account()]);
        assert!(
            super::may_show(Space::Calendar, switch_hidden, &calendars_alone),
            "it is all there is"
        );
        assert!(
            !super::may_show(Space::Mail, switch_hidden, &calendars_alone),
            "a shortcut must not open an empty mail window"
        );
        assert!(super::may_show(Space::Mail, switch_hidden, &with_mail));
        assert!(
            !super::may_show(Space::Calendar, switch_hidden, &with_mail),
            "no account offers a calendar"
        );
        assert!(super::may_show(Space::Calendar, true, &with_mail));
    }

    #[test]
    fn removing_a_calendar_account_speaks_of_calendars_not_mail() {
        let body = remove_account_body(&calendar_account(), RemoveSetting::Never);
        assert!(body.contains("calendars"), "{body}");
        assert!(!body.contains("mail"), "{body}");
    }

    #[test]
    fn about_says_which_accounts_keep_their_mail_here_alone() {
        let imap = Account {
            provider: Provider::Imap,
            email: "ana@fastmail.com".into(),
            ..pop3_account()
        };
        assert_eq!(
            kept_here_lines(&[imap, pop3_account()]),
            [
                "Mail from dana@example.org is kept only on this computer. To keep a copy, quit Penguin Mail, then back up this file."
            ]
        );
    }

    #[test]
    fn the_switch_shows_once_a_started_account_offers_a_calendar() {
        assert!(shows_space_switch(
            &[NO_CALENDAR, Offers::EVERYTHING],
            false,
            Space::Mail
        ));
    }

    #[test]
    fn the_switch_hides_when_no_started_account_offers_a_calendar() {
        assert!(!shows_space_switch(&[NO_CALENDAR], false, Space::Calendar));
        assert!(!shows_space_switch(&[], false, Space::Calendar));
    }

    #[test]
    fn before_the_accounts_start_the_switch_follows_the_last_space() {
        assert!(shows_space_switch(&[NO_CALENDAR], true, Space::Calendar));
        assert!(!shows_space_switch(&[], true, Space::Mail));
    }

    fn gmail() -> Account {
        Account {
            id: 1,
            email: "me@gmail.com".into(),
            state: AccountState::Ok,
            provider: Provider::Gmail,
            provider_name: None,
        }
    }

    fn fastmail() -> Account {
        Account {
            id: 2,
            email: "dana@fastmail.com".into(),
            state: AccountState::Ok,
            provider: Provider::Imap,
            provider_name: Some("Fastmail".into()),
        }
    }

    #[test]
    fn the_switch_says_what_moves_and_what_stays() {
        let rows = super::switch_rows(&fastmail(), true, true);
        let said: Vec<String> = rows
            .changes
            .iter()
            .chain(&rows.stays)
            .map(|r| format!("{} {}", r.title, r.subtitle))
            .collect();
        let said = said.join("\n");
        assert!(said.contains("Fastmail"), "{said}");
        assert!(said.contains("Reminders"), "{said}");
        assert!(said.contains("dana@fastmail.com"), "{said}");
    }

    #[test]
    fn a_jmap_calendar_that_refused_the_browser_sign_in_asks_for_an_app_password() {
        let account = Account {
            provider: Provider::Jmap,
            ..fastmail()
        };
        assert_eq!(
            super::reason(&account, Missing::Calendar, Some(Miss::Refused)),
            "Fastmail's calendar server refused the browser sign-in. Choose Sign In Again and give an app password for the calendar."
        );
        assert_eq!(
            super::reason(&account, Missing::Contacts, Some(Miss::Refused)),
            "Fastmail's contacts server refused the browser sign-in. Choose Sign In Again and give an app password for the calendar."
        );
    }

    #[test]
    fn only_a_token_sign_in_names_the_calendar_password_that_stays() {
        assert_eq!(super::switch_rows(&fastmail(), true, true).stays.len(), 3);
        assert_eq!(super::switch_rows(&fastmail(), false, true).stays.len(), 2);
    }

    #[test]
    fn a_password_server_says_its_calendars_may_come_over_jmap() {
        let rows = super::switch_rows(&fastmail(), true, false);
        assert_eq!(
            rows.stays.len(),
            2,
            "no app password stays for the calendar"
        );
        let last = rows.changes.last().expect("rows");
        assert_eq!(
            last.title,
            "Calendars are read again where JMAP serves them"
        );
        assert_eq!(
            last.subtitle,
            "If Fastmail offers calendars over JMAP, Penguin Mail reads them from there."
        );
    }

    const TOKEN: super::SwitchAsked<'static> = super::SwitchAsked {
        provider: "Fastmail",
        address: "dana@fastmail.com",
        token: true,
        browser: false,
    };

    #[test]
    fn a_refused_password_names_the_browser_when_the_server_offers_it() {
        let refused = anyhow::Error::new(mailrs_sync::switch::SwitchError::SignIn(
            mailrs_jmap::JmapError::Unauthorized,
        ));
        let asked = super::SwitchAsked {
            token: false,
            browser: true,
            ..TOKEN
        };
        assert_eq!(
            super::switch_failure(&refused, &asked).line,
            "Could not switch to JMAP: Fastmail did not accept the password. You can choose Sign In With Browser instead. \
             Your IMAP account is unchanged."
        );
    }

    #[test]
    fn a_sign_in_to_another_account_names_the_one_that_switches() {
        let other = anyhow::Error::new(mailrs_sync::switch::SwitchError::OtherAccount);
        assert_eq!(
            super::switch_failure(&other, &TOKEN).line,
            "Could not switch to JMAP: Fastmail signed in to another account, not dana@fastmail.com. \
             Your IMAP account is unchanged."
        );
    }

    #[test]
    fn a_browser_sign_in_the_server_refused_says_so() {
        let refused = anyhow::Error::new(mailrs_sync::sign_in::BrowserSignInError::Jmap(
            mailrs_jmap::JmapError::Unauthorized,
        ));
        let failed = super::switch_browser_failure(&refused, "Fastmail").expect("words");
        assert_eq!(
            failed.line,
            "Could not switch to JMAP: Fastmail refused the sign-in in the browser. Your IMAP account is unchanged."
        );
    }

    /// An error the switch does not know may carry an address or a
    /// server's own words, which do not belong in a toast.
    #[test]
    fn an_unknown_failure_says_a_fixed_sentence() {
        let raw = || anyhow::anyhow!("imap.example.com refused dana@fastmail.com: BAD [x]");
        let expected = "Could not switch to JMAP: Something unexpected stopped it. Your IMAP account is unchanged.";
        assert_eq!(super::switch_failure(&raw(), &TOKEN).line, expected);
        assert_eq!(
            super::switch_browser_failure(&raw(), "Fastmail")
                .expect("words")
                .line,
            expected
        );
    }

    #[test]
    fn a_canceled_browser_sign_in_says_nothing() {
        let canceled = anyhow::Error::new(mailrs_sync::sign_in::BrowserSignInError::Canceled);
        assert_eq!(super::switch_browser_failure(&canceled, "Fastmail"), None);
    }

    #[test]
    fn a_refused_token_says_the_imap_account_is_unchanged() {
        let refused = anyhow::Error::new(mailrs_sync::switch::SwitchError::SignIn(
            mailrs_jmap::JmapError::Unauthorized,
        ));
        let failed = super::switch_failure(&refused, &TOKEN);
        assert_eq!(
            failed.line,
            "Could not switch to JMAP: Fastmail did not accept the token. Your IMAP account is unchanged."
        );
        assert!(!failed.again);
    }

    #[test]
    fn a_send_later_draft_the_server_kept_back_says_so() {
        let kept = anyhow::Error::new(mailrs_sync::switch::SwitchError::Drafts(
            mailrs_sync::SyncError::Backend(mailrs_sync::BackendError::NotFound),
        ));
        let failed = super::switch_failure(&kept, &TOKEN);
        assert_eq!(
            failed.line,
            "Could not switch to JMAP: Fastmail did not give the draft of a message waiting in Send Later. \
             Your IMAP account is unchanged."
        );
    }

    #[test]
    fn mail_that_kept_changing_offers_another_try() {
        let changed = anyhow::Error::new(mailrs_sync::switch::SwitchError::Sync(
            mailrs_sync::SyncError::Backend(mailrs_sync::BackendError::Changed),
        ));
        let failed = super::switch_failure(&changed, &TOKEN);
        assert!(failed.again);
        assert!(
            failed.line.ends_with("Your IMAP account is unchanged."),
            "{}",
            failed.line
        );
    }

    #[test]
    fn the_done_toast_counts_the_rules_the_switch_turned_off() {
        assert_eq!(
            super::switched_line("dana@fastmail.com", 0),
            "dana@fastmail.com now uses JMAP"
        );
        assert!(super::switched_line("dana@fastmail.com", 2).contains("2 rules"));
    }

    #[test]
    fn each_missing_service_says_why_and_names_the_provider() {
        for missing in [
            Missing::Calendar,
            Missing::Contacts,
            Missing::Rules,
            Missing::AutoReply,
            Missing::DeleteForever,
            Missing::Categories,
        ] {
            let said = reason(&gmail(), missing, None);
            assert!(said.starts_with("Gmail "), "{said}");
            assert!(said.ends_with('.'), "{said}");
        }
    }

    #[test]
    fn a_jmap_account_says_penguin_mail_does_not_reach_it_there_yet() {
        let jmap = Account {
            provider: Provider::Jmap,
            ..fastmail()
        };
        for (missing, said) in [
            (
                Missing::Calendar,
                "Penguin Mail cannot show Fastmail's calendar over JMAP yet.",
            ),
            (
                Missing::Contacts,
                "Penguin Mail cannot read Fastmail's contacts over JMAP yet.",
            ),
            (
                Missing::Rules,
                "Penguin Mail cannot change Fastmail's rules over JMAP yet.",
            ),
            (
                Missing::AutoReply,
                "Penguin Mail cannot change Fastmail's automatic reply over JMAP yet.",
            ),
        ] {
            assert_eq!(reason(&jmap, missing, None), said);
        }
    }

    #[test]
    fn an_imap_account_without_a_calendar_server_says_none_was_found() {
        assert_eq!(
            reason(&fastmail(), Missing::Calendar, None),
            "Penguin Mail found no calendar server for Fastmail."
        );
        assert_eq!(
            reason(&fastmail(), Missing::Contacts, None),
            "Penguin Mail found no contacts server for Fastmail."
        );
    }

    #[test]
    fn a_refused_login_says_so_and_names_the_app_password() {
        assert_eq!(
            reason(&fastmail(), Missing::Calendar, Some(Miss::Refused)),
            "Fastmail refused the password for calendars. Some providers need an app \
             password with access to calendars and contacts."
        );
        assert_eq!(
            reason(&fastmail(), Missing::Contacts, Some(Miss::Refused)),
            "Fastmail refused the password for contacts. Some providers need an app \
             password with access to calendars and contacts."
        );
    }

    #[test]
    fn no_answer_says_the_server_could_not_be_reached() {
        assert_eq!(
            reason(&fastmail(), Missing::Calendar, Some(Miss::Unreachable)),
            "Penguin Mail could not reach the calendar server for Fastmail."
        );
        assert_eq!(
            reason(&fastmail(), Missing::Contacts, Some(Miss::Unreachable)),
            "Penguin Mail could not reach the contacts server for Fastmail."
        );
        assert_eq!(
            reason(&fastmail(), Missing::Contacts, Some(Miss::NotFound)),
            "Penguin Mail found no contacts server for Fastmail."
        );
    }

    #[test]
    fn preferences_words_each_line_by_why_the_search_failed() {
        let imap = Offers {
            calendar: false,
            contacts: false,
            ..Offers::EVERYTHING
        };
        let lines = missing_lines(
            &[(fastmail(), imap)],
            |_, missing| (missing == Missing::Calendar).then_some(Miss::Refused),
            |_| None,
        );
        let reasons = &lines[0].1;
        assert!(
            reasons[0].starts_with("Fastmail refused the password for calendars."),
            "{lines:?}"
        );
        assert_eq!(
            reasons[1],
            "Penguin Mail found no contacts server for Fastmail."
        );
    }

    #[test]
    fn a_refusal_on_a_microsoft_account_names_the_organization() {
        let account = Account {
            provider: Provider::Microsoft,
            provider_name: Some("Microsoft 365".into()),
            ..gmail()
        };
        assert_eq!(
            reason(&account, Missing::Calendar, None),
            "Your organization does not allow Penguin Mail to use this calendar."
        );
        assert_eq!(
            reason(&account, Missing::Contacts, None),
            "Your organization does not allow Penguin Mail to read these contacts."
        );
        assert_eq!(
            reason(&account, Missing::Rules, None),
            "Your organization does not allow Penguin Mail to change these rules."
        );
        assert_eq!(
            reason(&account, Missing::AutoReply, None),
            "Your organization does not allow Penguin Mail to change the automatic reply."
        );
    }

    #[test]
    fn an_imap_account_without_sieve_cannot_send_automatic_replies() {
        assert_eq!(
            reason(&fastmail(), Missing::AutoReply, None),
            "Fastmail cannot send automatic replies."
        );
    }

    #[test]
    fn masked_email_alone_lets_hide_my_email_in() {
        let masked = Offers {
            masked_email: true,
            rules: false,
            labels: false,
            ..Offers::EVERYTHING
        };
        assert!(hides_addresses(masked));
    }

    #[test]
    fn hide_my_email_needs_rules_and_labels() {
        let folders = Offers {
            labels: false,
            ..Offers::EVERYTHING
        };
        assert!(hides_addresses(Offers::EVERYTHING));
        assert!(!hides_addresses(folders));
        assert_eq!(account_menu_actions(folders)[2], ("hide-my-email", false));
        assert!(!account_action_on("account-hide-my-email", folders));
        assert!(
            account_action_on("account-rules", folders),
            "rules stay on for a folder account"
        );
        assert_eq!(account_actions(&[folders])[0], ("hide-my-email", false));
    }

    #[test]
    fn an_account_that_is_not_running_yet_hides_nothing() {
        assert_eq!(offers_for(None), Offers::EVERYTHING);
    }

    #[test]
    fn an_account_that_is_not_running_yet_withholds_nothing() {
        assert_eq!(withheld_for(None), mailrs_sync::Withheld::NONE);
    }

    use mailrs_sync::Mailbox;
    use mailrs_sync::mailbox::Standard;

    use super::shows_categories;

    fn without_categories() -> Offers {
        Offers {
            categories: false,
            ..Offers::EVERYTHING
        }
    }

    #[test]
    fn a_gmail_inbox_shows_its_categories() {
        let inbox = Mailbox::Standard {
            account_id: 1,
            which: Standard::Inbox,
        };
        assert!(shows_categories(true, &inbox, &[1], |_| Offers::EVERYTHING));
        assert!(!shows_categories(false, &inbox, &[1], |_| {
            Offers::EVERYTHING
        }));
    }

    #[test]
    fn an_inbox_whose_server_has_no_categories_hides_the_bar() {
        let inbox = Mailbox::Standard {
            account_id: 2,
            which: Standard::Inbox,
        };
        assert!(!shows_categories(true, &inbox, &[2], |_| {
            without_categories()
        }));
    }

    #[test]
    fn the_unified_inbox_shows_the_bar_when_one_account_sorts() {
        let all = Mailbox::Unified(Standard::Inbox);
        let offers = |id| {
            if id == 1 {
                Offers::EVERYTHING
            } else {
                without_categories()
            }
        };
        assert!(shows_categories(true, &all, &[1, 2], offers));
        assert!(!shows_categories(true, &all, &[2], offers));
    }

    use super::Filing;

    fn folders() -> Offers {
        Offers {
            labels: false,
            ..Offers::EVERYTHING
        }
    }

    #[test]
    fn filing_reads_folders_only_when_every_account_files_in_folders() {
        assert_eq!(Filing::of([Offers::EVERYTHING]), Filing::Labels);
        assert_eq!(Filing::of([folders()]), Filing::Folders);
        assert_eq!(Filing::of([folders(), Offers::EVERYTHING]), Filing::Labels);
        assert_eq!(Filing::of([]), Filing::Labels);
        assert_eq!(Filing::Labels.menu_item(), "Labels…");
        assert_eq!(Filing::Folders.menu_item(), "Move to Folder…");
    }

    #[test]
    fn the_labels_button_says_what_its_picker_says() {
        let offers = |id| {
            if id == 2 {
                folders()
            } else {
                Offers::EVERYTHING
            }
        };
        // Nothing picked: the accounts the mailbox lists decide.
        assert_eq!(Filing::picker(&[], &[2], offers), Filing::Folders);
        assert_eq!(Filing::picker(&[], &[1, 2], offers), Filing::Labels);
        // One account decides for itself, however many of its rows.
        assert_eq!(Filing::picker(&[1, 1], &[2], offers), Filing::Labels);
        assert_eq!(Filing::picker(&[2], &[1], offers), Filing::Folders);
        // Mail from a label account and a folder account files the way
        // folders do, one account at a time.
        assert_eq!(Filing::picker(&[1, 2], &[1, 2], offers), Filing::Folders);
        assert_eq!(Filing::picker(&[1, 3], &[1, 3], offers), Filing::Labels);
    }

    #[test]
    fn the_arrow_beside_the_labels_button_has_a_name_of_its_own() {
        assert_eq!(Filing::Labels.arrow(), "Choose Labels");
        assert_eq!(Filing::Folders.arrow(), "Choose a Folder");
    }

    #[test]
    fn the_shortcut_line_follows_filing() {
        assert_eq!(Filing::Labels.shortcut_line(), "Labels");
        assert_eq!(Filing::Folders.shortcut_line(), "Move to folder");
    }

    #[test]
    fn the_new_folder_dialog_says_folder_where_the_new_label_one_says_label() {
        assert_eq!(Filing::Labels.new_heading(), "New Label");
        assert_eq!(Filing::Folders.new_heading(), "New Folder");
        assert_eq!(
            Filing::Labels.create_failed(),
            "Could not create the label: {reason}"
        );
        assert_eq!(
            Filing::Folders.create_failed(),
            "Could not create the folder: {reason}"
        );
    }

    use super::sender_actions;

    #[test]
    fn blocking_a_sender_needs_rules() {
        let no_rules = Offers {
            rules: false,
            ..Offers::EVERYTHING
        };
        assert_eq!(
            sender_actions(Offers::EVERYTHING),
            [("block-sender", true), ("categorize-sender", true)]
        );
        assert_eq!(
            sender_actions(no_rules),
            [("block-sender", false), ("categorize-sender", false)]
        );
    }

    #[test]
    fn categorizing_a_sender_needs_categories_and_rules() {
        assert_eq!(
            sender_actions(without_categories()),
            [("block-sender", true), ("categorize-sender", false)]
        );
    }

    use super::{account_action_on, account_actions};

    #[test]
    fn an_account_action_is_on_while_one_account_can_do_it() {
        let bare = Offers {
            rules: false,
            auto_reply: false,
            auto_reply_subject: false,
            auto_reply_contacts_only: false,
            ..Offers::EVERYTHING
        };
        assert_eq!(
            account_actions(&[bare, Offers::EVERYTHING]),
            [
                ("hide-my-email", true),
                ("account-rules", true),
                ("account-hide-my-email", true),
                ("account-vacation", true),
            ]
        );
        assert_eq!(
            account_actions(&[bare]),
            [
                ("hide-my-email", false),
                ("account-rules", false),
                ("account-hide-my-email", false),
                ("account-vacation", false),
            ]
        );
    }

    #[test]
    fn an_account_action_turns_away_an_account_that_lacks_it() {
        let bare = Offers {
            rules: false,
            auto_reply: false,
            auto_reply_subject: false,
            auto_reply_contacts_only: false,
            ..Offers::EVERYTHING
        };
        assert!(!account_action_on("account-rules", bare));
        assert!(!account_action_on("account-hide-my-email", bare));
        assert!(!account_action_on("account-vacation", bare));
        assert!(account_action_on("account-signature", bare));
        assert!(account_action_on("account-rules", Offers::EVERYTHING));
    }

    use super::account_menu_actions;

    #[test]
    fn an_accounts_own_rules_action_is_off_when_its_server_has_no_rules() {
        let imap = Offers {
            labels: false,
            categories: false,
            calendar: false,
            contacts: false,
            rules: false,
            auto_reply: false,
            auto_reply_subject: false,
            auto_reply_contacts_only: false,
            ..Offers::EVERYTHING
        };
        assert_eq!(
            account_menu_actions(imap),
            [
                ("vacation", false),
                ("rules", false),
                ("hide-my-email", false)
            ]
        );
        assert_eq!(
            account_menu_actions(Offers::EVERYTHING),
            [("vacation", true), ("rules", true), ("hide-my-email", true)]
        );
        let replies_only = Offers {
            rules: false,
            ..Offers::EVERYTHING
        };
        assert_eq!(
            account_menu_actions(replies_only),
            [
                ("vacation", true),
                ("rules", false),
                ("hide-my-email", false)
            ],
            "Hide My Email needs rules, the automatic reply does not"
        );
    }

    #[test]
    fn mail_from_several_accounts_takes_labels_only_when_every_one_has_them() {
        assert_eq!(
            Filing::across([Offers::EVERYTHING, Offers::EVERYTHING]),
            Filing::Labels
        );
        assert_eq!(
            Filing::across([Offers::EVERYTHING, folders()]),
            Filing::Folders
        );
    }

    #[test]
    fn the_picker_asks_for_mail_in_the_words_of_the_filing() {
        assert_eq!(
            Filing::Labels.nothing_picked(),
            "Open or select mail to label it."
        );
        assert_eq!(
            Filing::Folders.nothing_picked(),
            "Open or select mail to move it."
        );
        assert_eq!(
            Filing::Labels.one_account_only(),
            "Select mail from one account to label it."
        );
        assert_eq!(
            Filing::Folders.one_account_only(),
            "Select mail from one account to move it."
        );
    }

    #[test]
    fn renaming_and_deleting_keep_the_gmail_words_for_labels() {
        assert_eq!(Filing::Labels.rename_heading(), "Rename Label");
        assert_eq!(
            Filing::Labels.rename_body(),
            "Labels nested under it move along."
        );
        assert_eq!(
            Filing::Labels.rename_failed(),
            "Could not rename the label: {reason}"
        );
        assert_eq!(
            Filing::Labels.delete_body("Gmail"),
            "Its mail stays in Gmail, without the label. Nested labels stay too."
        );
        assert_eq!(
            Filing::Labels.delete_body("Fastmail"),
            "Its mail stays in Fastmail, without the label. Nested labels stay too."
        );
        assert_eq!(
            Filing::Labels.delete_body(""),
            "Its mail stays in the account, without the label. Nested labels stay too."
        );
        assert_eq!(
            Filing::Labels.delete_failed(),
            "Could not delete the label: {reason}"
        );
    }

    #[test]
    fn deleting_a_folder_says_its_mail_goes_with_it() {
        assert_eq!(Filing::Folders.rename_heading(), "Rename Folder");
        assert_eq!(
            Filing::Folders.rename_body(),
            "Folders nested under it move along."
        );
        assert_eq!(
            Filing::Folders.rename_failed(),
            "Could not rename the folder: {reason}"
        );
        assert_eq!(
            Filing::Folders.delete_body("Gmail"),
            "The mail in the folder is deleted with it."
        );
        assert_eq!(
            Filing::Folders.delete_failed(),
            "Could not delete the folder: {reason}"
        );
    }

    use super::missing_lines;

    #[test]
    fn preferences_names_what_each_account_lacks_and_nothing_for_gmail() {
        let bare = Account {
            id: 2,
            email: "me@example.com".into(),
            ..gmail()
        };
        let lacking = Offers {
            rules: false,
            auto_reply: false,
            auto_reply_subject: false,
            auto_reply_contacts_only: false,
            ..Offers::EVERYTHING
        };
        let lines = missing_lines(
            &[(gmail(), Offers::EVERYTHING), (bare.clone(), lacking)],
            |_, _| None,
            |_| None,
        );
        assert_eq!(
            lines,
            [(
                "me@example.com".to_string(),
                vec![
                    reason(&bare, Missing::Rules, None),
                    reason(&bare, Missing::AutoReply, None),
                ],
            )]
        );
    }

    #[test]
    fn preferences_gives_each_account_one_entry_whatever_it_lacks() {
        let pop3 = Account {
            id: 3,
            email: "dana@reyes-home.example".into(),
            provider: Provider::Pop3,
            provider_name: Some("reyes-home.example".into()),
            ..gmail()
        };
        let lacking = Offers {
            calendar: false,
            contacts: false,
            auto_reply: false,
            auto_reply_subject: false,
            auto_reply_contacts_only: false,
            ..Offers::EVERYTHING
        };
        let lines = missing_lines(
            &[(fastmail(), lacking), (pop3, lacking)],
            |_, _| None,
            |_| None,
        );
        let addresses: Vec<&str> = lines.iter().map(|(address, _)| address.as_str()).collect();
        assert_eq!(addresses, ["dana@fastmail.com", "dana@reyes-home.example"]);
        assert_eq!(lines[1].1.len(), 3, "{lines:?}");
    }

    #[test]
    fn preferences_lists_what_an_imap_account_lacks_once_each() {
        let imap = Offers {
            labels: false,
            categories: false,
            calendar: false,
            contacts: false,
            auto_reply: false,
            auto_reply_subject: false,
            auto_reply_contacts_only: false,
            ..Offers::EVERYTHING
        };
        let lines = missing_lines(&[(fastmail(), imap)], |_, _| None, |_| None);
        assert_eq!(
            lines[0].1,
            [
                "Penguin Mail found no calendar server for Fastmail.",
                "Penguin Mail found no contacts server for Fastmail.",
                "Fastmail cannot send automatic replies.",
            ]
        );
    }

    #[test]
    fn each_not_available_row_is_named_for_its_account_and_every_reason() {
        assert_eq!(
            super::missing_name(
                "dana@fastmail.example",
                &[
                    "Penguin Mail found no calendar server for Fastmail.".to_string(),
                    "Fastmail cannot send automatic replies.".to_string(),
                ]
            ),
            "dana@fastmail.example: Penguin Mail found no calendar server for Fastmail. \
             Fastmail cannot send automatic replies."
        );
    }

    use super::reads_send_as;

    #[test]
    fn only_gmail_is_asked_which_addresses_it_sends_as() {
        assert!(reads_send_as(&gmail()));
        assert!(!reads_send_as(&fastmail()));
        let jmap = Account {
            provider: Provider::Jmap,
            ..fastmail()
        };
        assert!(reads_send_as(&jmap));
    }

    #[test]
    fn a_mailbox_that_is_not_an_inbox_never_shows_the_bar() {
        let sent = Mailbox::Unified(Standard::Sent);
        assert!(!shows_categories(true, &sent, &[1], |_| Offers::EVERYTHING));
    }

    fn microsoft() -> Offers {
        Offers {
            labels: false,
            categories: false,
            tags: true,
            focused: true,
            ..Offers::EVERYTHING
        }
    }

    use super::{InboxBar, categorize_choices, category_after, inbox_bar, tags_on};
    use mailrs_domain::{AccountId, Category};

    #[test]
    fn one_microsoft_inbox_shows_focused_and_other() {
        let offers = |id: AccountId| {
            if id == 2 {
                microsoft()
            } else {
                Offers::EVERYTHING
            }
        };
        let outlook = Mailbox::Standard {
            account_id: 2,
            which: Standard::Inbox,
        };
        assert_eq!(
            inbox_bar(true, &outlook, &[1, 2], offers),
            Some(InboxBar::Focus)
        );
        assert_eq!(
            inbox_bar(false, &outlook, &[1, 2], offers),
            None,
            "the categories preference turns both off"
        );
        let gmail = Mailbox::Standard {
            account_id: 1,
            which: Standard::Inbox,
        };
        assert_eq!(
            inbox_bar(true, &gmail, &[1, 2], offers),
            Some(InboxBar::Categories)
        );
        // The unified inbox keeps Gmail's categories; Outlook mail there
        // counts as Primary.
        assert_eq!(
            inbox_bar(true, &Mailbox::Unified(Standard::Inbox), &[1, 2], offers),
            Some(InboxBar::Categories)
        );
        assert_eq!(
            inbox_bar(true, &Mailbox::Unified(Standard::Inbox), &[2], offers),
            None
        );
    }

    #[test]
    fn switching_bars_moves_the_slice_to_one_the_bar_has() {
        assert_eq!(
            category_after(InboxBar::Focus, Category::Social, Category::Primary),
            Category::Focused
        );
        assert_eq!(
            category_after(InboxBar::Focus, Category::Other, Category::Primary),
            Category::Other
        );
        assert_eq!(
            category_after(InboxBar::Categories, Category::Other, Category::Primary),
            Category::Primary
        );
        assert_eq!(
            category_after(InboxBar::Categories, Category::Social, Category::Primary),
            Category::Social
        );
    }

    #[test]
    fn tags_show_for_mail_from_one_account_that_keeps_them() {
        let offers = |id: AccountId| {
            if id == 2 {
                microsoft()
            } else {
                Offers::EVERYTHING
            }
        };
        assert!(tags_on(&[2, 2], offers));
        assert!(!tags_on(&[1], offers));
        assert!(!tags_on(&[1, 2], offers), "tags belong to one account");
        assert!(!tags_on(&[], offers));
    }

    #[test]
    fn categorize_sender_offers_focus_on_a_microsoft_account() {
        assert_eq!(
            categorize_choices(microsoft()),
            [Category::Focused, Category::Other]
        );
        assert_eq!(
            categorize_choices(Offers::EVERYTHING),
            [
                Category::Primary,
                Category::Updates,
                Category::Promotions,
                Category::Social
            ]
        );
        let on = sender_actions(microsoft());
        assert!(on.contains(&("categorize-sender", true)));
    }

    #[test]
    fn a_reply_a_token_cannot_reach_names_the_browser_sign_in() {
        let jmap = Account {
            provider: Provider::Jmap,
            provider_name: Some("Fastmail".into()),
            ..fastmail()
        };
        let lacking = Offers {
            auto_reply: false,
            auto_reply_needs_sign_in: true,
            ..Offers::EVERYTHING
        };
        let lines = missing_lines(&[(jmap, lacking)], |_, _| None, |_| None);
        assert_eq!(
            lines[0].1,
            [
                "Fastmail does not let an API token change the automatic reply. Sign in again through the browser to change it here."
            ]
        );
    }
}
