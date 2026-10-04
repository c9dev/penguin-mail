//! The fake's rules, automatic reply and Focused Inbox overrides.

use mailrs_graph::{AutomaticReplies, EmailAddress, GraphError, MessageRule, Override};

use super::{Answer, Area, GraphState};

pub(super) fn rules(s: &mut GraphState) -> Answer<Vec<MessageRule>> {
    s.refuses(Area::Rules)?;
    Ok(s.rules.clone())
}

pub(super) fn create_rule(s: &mut GraphState, rule: &MessageRule) -> Answer<MessageRule> {
    s.refuses(Area::Rules)?;
    let made = MessageRule { id: s.new_id("rule"), ..rule.clone() };
    s.rules.push(made.clone());
    Ok(made)
}

pub(super) fn delete_rule(s: &mut GraphState, id: &str) -> Answer<()> {
    s.refuses(Area::Rules)?;
    let at = s.rules.iter().position(|r| r.id == id).ok_or(GraphError::NotFound)?;
    s.rules.remove(at);
    Ok(())
}

pub(super) fn automatic_replies(s: &mut GraphState) -> Answer<AutomaticReplies> {
    s.refuses(Area::Replies)?;
    Ok(s.replies.clone())
}

pub(super) fn set_automatic_replies(s: &mut GraphState, replies: &AutomaticReplies) -> Answer<()> {
    s.refuses(Area::Replies)?;
    s.replies = replies.clone();
    Ok(())
}

pub(super) fn overrides(s: &mut GraphState) -> Answer<Vec<Override>> {
    s.refuses(Area::Rules)?;
    Ok(s.overrides.clone())
}

/// A sender has one override: a second for the same address replaces it.
pub(super) fn set_override(s: &mut GraphState, address: &str, other: bool) -> Answer<Override> {
    s.refuses(Area::Rules)?;
    s.overrides.retain(|o| o.sender_email_address.address.as_deref() != Some(address));
    let made = Override {
        id: s.new_id("override"),
        classify_as: if other { "other" } else { "focused" }.into(),
        sender_email_address: EmailAddress { name: None, address: Some(address.to_string()) },
    };
    s.overrides.push(made.clone());
    Ok(made)
}

pub(super) fn delete_override(s: &mut GraphState, id: &str) -> Answer<()> {
    s.refuses(Area::Rules)?;
    let at = s.overrides.iter().position(|o| o.id == id).ok_or(GraphError::NotFound)?;
    s.overrides.remove(at);
    Ok(())
}

#[cfg(test)]
mod tests {
    use mailrs_graph::GraphError;

    use crate::fake::FakeGraph;
    use crate::services::microsoft::GraphApi;

    #[tokio::test]
    async fn an_override_replaces_the_one_for_the_same_address() {
        let fake = FakeGraph::new();
        fake.set_override("ann@example.com", true).await.unwrap();
        fake.set_override("ann@example.com", false).await.unwrap();
        let all = fake.overrides().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].classify_as, "focused");
    }

    #[tokio::test]
    async fn deleting_an_unknown_rule_is_not_found() {
        let fake = FakeGraph::new();
        assert!(matches!(fake.delete_rule("nope").await, Err(GraphError::NotFound)));
    }

    #[tokio::test]
    async fn the_automatic_reply_is_kept_as_set() {
        let fake = FakeGraph::new();
        let mut replies = fake.automatic_replies().await.unwrap();
        assert_eq!(replies.status, "disabled");
        replies.status = "alwaysEnabled".into();
        fake.set_automatic_replies(&replies).await.unwrap();
        assert_eq!(fake.automatic_replies().await.unwrap().status, "alwaysEnabled");
    }
}
