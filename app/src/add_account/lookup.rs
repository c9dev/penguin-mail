//! The lookup page's list of checks: what discovery asks and how far each
//! question has got. Discovery reports nothing itself, so the core wraps
//! its network in a watcher that says each time a question goes out and
//! comes back, and these rows follow what it says.

use mailrs_domain::translate::{fill, gettext};

/// One row of the lookup page, in the order discovery asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// The built-in list, which answers without the network.
    Table,
    /// The domain's MX records, which every step below waits for.
    Mx,
    /// The domain's own autoconfig file, or its MX host's.
    Own,
    /// Mozilla's provider list, the ISPDB.
    Mozilla,
    /// SRV records and a probe of the usual host names.
    Common,
}

impl Check {
    pub const ALL: [Check; 5] = [Check::Table, Check::Mx, Check::Own, Check::Mozilla, Check::Common];

    fn index(self) -> usize {
        self as usize
    }

    /// The row's title for an address at `domain`.
    pub fn title(self, domain: &str) -> String {
        match self {
            Check::Table => gettext("Penguin Mail's own list"),
            Check::Mx => gettext("Mail servers (MX)"),
            Check::Own => fill(&gettext("{domain}'s own settings"), &[("domain", domain)]),
            Check::Mozilla => gettext("Mozilla's provider list"),
            Check::Common => gettext("Common server names"),
        }
    }
}

/// Where Mozilla's provider list lives; discovery asks it by URL.
const ISPDB: &str = "https://autoconfig.thunderbird.net/";

/// The check an HTTPS request of discovery's belongs to.
pub fn check_of_url(url: &str) -> Check {
    if url.starts_with(ISPDB) {
        Check::Mozilla
    } else {
        Check::Own
    }
}

/// What the network watcher heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    /// A question for this check went out.
    Asked(Check),
    /// It came back, with something in it or not.
    Answered(Check, bool),
}

/// How far a check has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The built-in list does not know the domain.
    NotListed,
    /// Waiting for the MX answer before it may ask.
    Waiting,
    Asking,
    /// Something came back.
    Answered,
    /// The question came back empty.
    Nothing,
}

impl State {
    pub fn text(self) -> String {
        match self {
            State::NotListed => gettext("Not listed"),
            State::Waiting => gettext("Waits for MX"),
            State::Asking => gettext("Asking…"),
            State::Answered => gettext("Answered"),
            State::Nothing => gettext("Nothing there"),
        }
    }
}

/// Every check's progress for one lookup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Checks {
    in_flight: [u32; 5],
    answered: [Option<bool>; 5],
}

impl Checks {
    pub fn heard(&mut self, heard: Heard) {
        match heard {
            Heard::Asked(check) => self.in_flight[check.index()] += 1,
            Heard::Answered(check, found) => {
                let i = check.index();
                self.in_flight[i] = self.in_flight[i].saturating_sub(1);
                // One answer with something in it is enough for the row.
                self.answered[i] = Some(self.answered[i].unwrap_or(false) || found);
            }
        }
    }

    pub fn state(&self, check: Check) -> State {
        let i = check.index();
        if check == Check::Table {
            // The lookup page shows only for a domain the list does not
            // know: a listed one answers before anything is asked.
            return State::NotListed;
        }
        if self.in_flight[i] > 0 {
            return State::Asking;
        }
        match self.answered[i] {
            Some(true) => State::Answered,
            Some(false) => State::Nothing,
            // MX is asked first; every other step waits for its answer
            // and asks as soon as it comes.
            None if check == Check::Mx || self.answered[Check::Mx.index()].is_some() => {
                State::Asking
            }
            None => State::Waiting,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn states(checks: &Checks) -> Vec<State> {
        Check::ALL.into_iter().map(|c| checks.state(c)).collect()
    }

    #[test]
    fn a_lookup_starts_with_mx_asking_and_the_rest_waiting_for_it() {
        use State::*;
        assert_eq!(
            states(&Checks::default()),
            [NotListed, Asking, Waiting, Waiting, Waiting]
        );
    }

    #[test]
    fn once_mx_answers_the_other_checks_ask() {
        use State::*;
        let mut checks = Checks::default();
        checks.heard(Heard::Asked(Check::Mx));
        checks.heard(Heard::Answered(Check::Mx, true));
        assert_eq!(
            states(&checks),
            [NotListed, Answered, Asking, Asking, Asking]
        );
    }

    #[test]
    fn a_check_asking_twice_is_asking_until_both_come_back() {
        let mut checks = Checks::default();
        checks.heard(Heard::Answered(Check::Mx, false));
        checks.heard(Heard::Asked(Check::Own));
        checks.heard(Heard::Asked(Check::Own));
        checks.heard(Heard::Answered(Check::Own, true));
        assert_eq!(checks.state(Check::Own), State::Asking);
        checks.heard(Heard::Answered(Check::Own, false));
        assert_eq!(checks.state(Check::Own), State::Answered);
    }

    #[test]
    fn a_check_that_found_nothing_says_so() {
        let mut checks = Checks::default();
        checks.heard(Heard::Answered(Check::Mx, false));
        checks.heard(Heard::Asked(Check::Common));
        checks.heard(Heard::Answered(Check::Common, false));
        assert_eq!(checks.state(Check::Mx), State::Nothing);
        assert_eq!(checks.state(Check::Common), State::Nothing);
    }

    #[test]
    fn mozillas_list_is_told_apart_from_the_domains_own_file_by_url() {
        assert_eq!(
            check_of_url("https://autoconfig.thunderbird.net/v1.1/reyes.studio"),
            Check::Mozilla
        );
        assert_eq!(
            check_of_url("https://autoconfig.reyes.studio/mail/config-v1.1.xml"),
            Check::Own
        );
        assert_eq!(
            check_of_url("https://reyes.studio/.well-known/autoconfig/mail/config-v1.1.xml"),
            Check::Own
        );
    }

    #[test]
    fn the_rows_read_as_the_mockup_writes_them() {
        let titles: Vec<String> = Check::ALL.into_iter().map(|c| c.title("reyes.studio")).collect();
        assert_eq!(
            titles,
            [
                "Penguin Mail's own list",
                "Mail servers (MX)",
                "reyes.studio's own settings",
                "Mozilla's provider list",
                "Common server names",
            ]
        );
        assert_eq!(State::NotListed.text(), "Not listed");
        assert_eq!(State::Waiting.text(), "Waits for MX");
        assert_eq!(State::Asking.text(), "Asking…");
    }
}
