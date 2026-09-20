//! Ids of the labels Gmail itself defines. Gmail spells them the same in
//! every account, so code can name them directly. User labels have ids
//! like `Label_12` that only the account knows.

pub const INBOX: &str = "INBOX";
pub const SENT: &str = "SENT";
pub const DRAFT: &str = "DRAFT";
pub const STARRED: &str = "STARRED";
pub const UNREAD: &str = "UNREAD";
pub const TRASH: &str = "TRASH";
pub const SPAM: &str = "SPAM";
pub const IMPORTANT: &str = "IMPORTANT";
/// Marks a thread muted. Gmail's own filters archive whatever arrives on a
/// thread that carries it, so the reply never reaches the inbox.
pub const MUTE: &str = "MUTE";

pub const CATEGORY_PERSONAL: &str = "CATEGORY_PERSONAL";
pub const CATEGORY_UPDATES: &str = "CATEGORY_UPDATES";
pub const CATEGORY_PROMOTIONS: &str = "CATEGORY_PROMOTIONS";
pub const CATEGORY_SOCIAL: &str = "CATEGORY_SOCIAL";
pub const CATEGORY_FORUMS: &str = "CATEGORY_FORUMS";

/// Every category label Gmail puts on inbox mail, Personal first.
pub const CATEGORIES: [&str; 5] = [
    CATEGORY_PERSONAL,
    CATEGORY_UPDATES,
    CATEGORY_PROMOTIONS,
    CATEGORY_SOCIAL,
    CATEGORY_FORUMS,
];

/// Whether `label` names one of Gmail's inbox categories, including any
/// Gmail adds later.
pub fn is_category(label: &str) -> bool {
    label.starts_with("CATEGORY_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_category_label_counts_as_a_category() {
        assert!(CATEGORIES.into_iter().all(is_category));
        assert!(!is_category(INBOX));
        assert!(!is_category("Label_12"));
    }
}
