//! What the window and the assistant offer for each account, read from
//! the account's services, and the words for what an account lacks.

use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{Account, AccountId, Provider};
use mailrs_sync::{AccountServices, Mailbox, Missing, Offers, Withheld};

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

/// Whether the sender's own actions are on for an account that `offers`
/// what it offers. Blocking a sender and sorting its mail into a category
/// both leave a rule on the server for the mail still to come, so both
/// need rules.
pub fn sender_actions(offers: Offers) -> [(&'static str, bool); 2] {
    [
        ("block-sender", offers.rules),
        ("categorize-sender", offers.categories && offers.rules),
    ]
}

/// The window's actions that need rules or an automatic reply, and
/// whether each is on for accounts that offer `offers`: on while one of
/// them can do it. Each account action also turns away an account that
/// cannot, through [`account_action_on`], since one action serves every
/// account's menu.
pub fn account_actions(offers: &[Offers]) -> [(&'static str, bool); 4] {
    let rules = offers.iter().any(|o| o.rules);
    let auto_reply = offers.iter().any(|o| o.auto_reply);
    [
        ("hide-my-email", rules),
        ("account-rules", rules),
        ("account-hide-my-email", rules),
        ("account-vacation", auto_reply),
    ]
}

/// Whether the account action `name` runs for an account that offers
/// `offers`. An account's menu reaches these through its own actions
/// ([`account_menu_actions`]), which are off where the account lacks what
/// they open, but the demo's script activates the `win.` action with any
/// account's id. Hide My Email writes a rule for each address, so it
/// needs rules.
pub fn account_action_on(name: &str, offers: Offers) -> bool {
    match name {
        "account-rules" | "account-hide-my-email" => offers.rules,
        "account-vacation" => offers.auto_reply,
        _ => true,
    }
}

/// The actions an account's own menu holds under the `account` prefix,
/// and whether each is on for an account that offers `offers`. A
/// parameterized `win.` action serves every account, so it cannot be off
/// for one; these belong to one account's row, so each is off where that
/// account lacks what it opens. Hide My Email writes a rule for each
/// address, so it needs rules.
pub fn account_menu_actions(offers: Offers) -> [(&'static str, bool); 3] {
    [
        ("vacation", offers.auto_reply),
        ("rules", offers.rules),
        ("hide-my-email", offers.rules),
    ]
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

    /// The header button's tooltip, with its key.
    pub fn tooltip(self) -> String {
        match self {
            Filing::Labels => gettext("Labels (L)"),
            Filing::Folders => gettext("Move to Folder (L)"),
        }
    }

    pub fn new_item(self) -> String {
        match self {
            Filing::Labels => gettext("New Label…"),
            Filing::Folders => gettext("New Folder…"),
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
    /// the mail stays; a folder holds the only copy, and its mail goes
    /// with it.
    pub fn delete_body(self) -> String {
        match self {
            Filing::Labels => {
                gettext("Its mail stays in Gmail, without the label. Nested labels stay too.")
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
pub fn reason(account: &Account, missing: Missing) -> String {
    let template = match (account.provider, missing) {
        // IMAP carries mail and nothing else. A calendar and contacts
        // need CalDAV and CardDAV, and rules need a server that runs
        // them, which a later version brings to these accounts.
        (Provider::Imap, Missing::Calendar) => {
            gettext("{provider}'s calendar comes in a later version.")
        }
        (Provider::Imap, Missing::Contacts) => {
            gettext("{provider}'s contacts come in a later version.")
        }
        (Provider::Imap, Missing::Rules | Missing::AutoReply) => {
            gettext("Rules and automatic replies need a server that runs them.")
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

/// The lines Preferences shows under Not Available: everything an
/// account lacks but the category bar, which needs no line because it is
/// not there, each as the account's address and the reason. An IMAP
/// account gives rules and the automatic reply one reason, shown once.
pub fn missing_lines(accounts: &[(Account, Offers)]) -> Vec<(String, String)> {
    let mut lines: Vec<(String, String)> = accounts
        .iter()
        .flat_map(|(account, offers)| {
            offers
                .missing()
                .into_iter()
                .filter(|m| *m != Missing::Categories)
                .map(|m| (account.email.clone(), reason(account, m)))
        })
        .collect();
    lines.dedup();
    lines
}

/// What a screen reader calls a Not Available row. One account can have
/// several rows under the same address, so the name carries the reason.
pub fn missing_name(address: &str, reason: &str) -> String {
    fill(
        &gettext("{address}: {reason}"),
        &[("address", address), ("reason", reason)],
    )
}

/// Whether the app asks the server which addresses `account` sends as.
/// Gmail keeps send-as addresses with their names. An IMAP server keeps
/// neither, and asking it would replace the name the person typed when
/// adding the account with none.
pub fn reads_send_as(account: &Account) -> bool {
    account.provider == Provider::Gmail
}

#[cfg(test)]
mod tests {
    use mailrs_domain::{Account, AccountState, Provider};
    use mailrs_sync::{Missing, Offers};

    use super::{offers_for, reason, withheld_for};

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
    fn each_missing_service_says_why_and_names_the_provider() {
        for missing in [
            Missing::Calendar,
            Missing::Contacts,
            Missing::Rules,
            Missing::AutoReply,
            Missing::DeleteForever,
            Missing::Categories,
        ] {
            let said = reason(&gmail(), missing);
            assert!(said.starts_with("Gmail "), "{said}");
            assert!(said.ends_with('.'), "{said}");
        }
    }

    #[test]
    fn an_imap_account_says_its_calendar_and_contacts_come_later() {
        assert_eq!(
            reason(&fastmail(), Missing::Calendar),
            "Fastmail's calendar comes in a later version."
        );
        assert_eq!(
            reason(&fastmail(), Missing::Contacts),
            "Fastmail's contacts come in a later version."
        );
    }

    #[test]
    fn an_imap_account_says_rules_need_a_server_that_runs_them() {
        for missing in [Missing::Rules, Missing::AutoReply] {
            assert_eq!(
                reason(&fastmail(), missing),
                "Rules and automatic replies need a server that runs them."
            );
        }
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
        let inbox = Mailbox::Standard { account_id: 1, which: Standard::Inbox };
        assert!(shows_categories(true, &inbox, &[1], |_| Offers::EVERYTHING));
        assert!(!shows_categories(false, &inbox, &[1], |_| Offers::EVERYTHING));
    }

    #[test]
    fn an_inbox_whose_server_has_no_categories_hides_the_bar() {
        let inbox = Mailbox::Standard { account_id: 2, which: Standard::Inbox };
        assert!(!shows_categories(true, &inbox, &[2], |_| without_categories()));
    }

    #[test]
    fn the_unified_inbox_shows_the_bar_when_one_account_sorts() {
        let all = Mailbox::Unified(Standard::Inbox);
        let offers = |id| if id == 1 { Offers::EVERYTHING } else { without_categories() };
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
        let offers = |id| if id == 2 { folders() } else { Offers::EVERYTHING };
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
            ..Offers::EVERYTHING
        };
        assert_eq!(
            account_menu_actions(imap),
            [("vacation", false), ("rules", false), ("hide-my-email", false)]
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
            [("vacation", true), ("rules", false), ("hide-my-email", false)],
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
            Filing::Labels.delete_body(),
            "Its mail stays in Gmail, without the label. Nested labels stay too."
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
            Filing::Folders.delete_body(),
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
            ..Offers::EVERYTHING
        };
        let lines = missing_lines(&[(gmail(), Offers::EVERYTHING), (bare.clone(), lacking)]);
        assert_eq!(
            lines,
            [
                ("me@example.com".to_string(), reason(&bare, Missing::Rules)),
                ("me@example.com".to_string(), reason(&bare, Missing::AutoReply)),
            ]
        );
    }

    #[test]
    fn preferences_lists_what_an_imap_account_lacks_once_each() {
        let imap = Offers {
            labels: false,
            categories: false,
            calendar: false,
            contacts: false,
            rules: false,
            auto_reply: false,
            ..Offers::EVERYTHING
        };
        let lines = missing_lines(&[(fastmail(), imap)]);
        let said: Vec<&str> = lines.iter().map(|(_, line)| line.as_str()).collect();
        assert_eq!(
            said,
            [
                "Fastmail's calendar comes in a later version.",
                "Fastmail's contacts come in a later version.",
                "Rules and automatic replies need a server that runs them.",
            ]
        );
    }

    #[test]
    fn each_not_available_row_is_named_for_its_account_and_its_reason() {
        assert_eq!(
            super::missing_name(
                "dana@fastmail.example",
                "Fastmail's calendar comes in a later version."
            ),
            "dana@fastmail.example: Fastmail's calendar comes in a later version."
        );
    }

    use super::reads_send_as;

    #[test]
    fn only_gmail_is_asked_which_addresses_it_sends_as() {
        assert!(reads_send_as(&gmail()));
        assert!(!reads_send_as(&fastmail()));
    }

    #[test]
    fn a_mailbox_that_is_not_an_inbox_never_shows_the_bar() {
        let sent = Mailbox::Unified(Standard::Sent);
        assert!(!shows_categories(true, &sent, &[1], |_| Offers::EVERYTHING));
    }
}
