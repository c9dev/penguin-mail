//! The setup that lets a person use their own Google project.
#![allow(dead_code, reason = "The setup pages use these")]

/// A page of the setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Intro,
    Project,
    Apis,
    Screen,
    Publish,
    Key,
    File,
    Warning,
}

impl Step {
    /// The number the person sees, 1 to 6, for the six steps.
    pub fn number(self) -> Option<u8> {
        match self {
            Step::Project => Some(1),
            Step::Apis => Some(2),
            Step::Screen => Some(3),
            Step::Publish => Some(4),
            Step::Key => Some(5),
            Step::File => Some(6),
            Step::Intro | Step::Warning => None,
        }
    }

    /// The page tag in the navigation view.
    pub fn tag(self) -> &'static str {
        match self {
            Step::Intro => "own-intro",
            Step::Project => "own-1",
            Step::Apis => "own-2",
            Step::Screen => "own-3",
            Step::Publish => "own-4",
            Step::Key => "own-5",
            Step::File => "own-file",
            Step::Warning => "own-warning",
        }
    }

    /// The step with this number, 1 to 6.
    pub fn of_number(n: u8) -> Option<Step> {
        match n {
            1 => Some(Step::Project),
            2 => Some(Step::Apis),
            3 => Some(Step::Screen),
            4 => Some(Step::Publish),
            5 => Some(Step::Key),
            6 => Some(Step::File),
            _ => None,
        }
    }
}

/// Where the setup opens for the step saved in `config.toml`. Nothing
/// saved, or 0 from a hand edit, opens the intro; a number past 6 opens
/// the last step.
pub fn resume_at(saved: Option<u8>) -> Step {
    match saved {
        None | Some(0) => Step::Intro,
        Some(n) => Step::of_number(n).unwrap_or(Step::File),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_saved_opens_the_intro() {
        assert_eq!(resume_at(None), Step::Intro);
    }

    #[test]
    fn a_saved_step_opens_that_step() {
        assert_eq!(resume_at(Some(4)), Step::Publish);
    }

    #[test]
    fn step_zero_opens_the_intro() {
        assert_eq!(resume_at(Some(0)), Step::Intro);
    }

    #[test]
    fn a_step_past_six_opens_the_file_step() {
        assert_eq!(resume_at(Some(9)), Step::File);
    }

    #[test]
    fn every_step_has_its_own_tag() {
        let all = [
            Step::Intro,
            Step::Project,
            Step::Apis,
            Step::Screen,
            Step::Publish,
            Step::Key,
            Step::File,
            Step::Warning,
        ];
        let tags: std::collections::HashSet<_> = all.iter().map(|s| s.tag()).collect();
        assert_eq!(tags.len(), all.len());
        assert_eq!(Step::Intro.tag(), "own-intro");
        assert_eq!(Step::Key.tag(), "own-5");
        assert_eq!(Step::Warning.tag(), "own-warning");
    }
}
