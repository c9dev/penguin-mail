//! The inbox rules Outlook runs on the server, and the sender overrides
//! behind Focused Inbox. A rule reads as a `Filter` when the filter can
//! say all of it; any other rule reads as a read-only filter, so the
//! dialog lists it and never rewrites it. An override lists as a filter
//! whose id is `override:<Graph id>` and whose action adds the Other
//! category (or removes it, for Focused).

use mailrs_domain::translate::gettext;
use mailrs_domain::{Filter, FilterAction, FilterCriteria, MailSet, Role, category};
use mailrs_graph::{EmailAddress, MessageRule, Override, Recipient, RuleActions, RulePredicates};

use super::{GraphApi, Microsoft, Service, tag_id, tag_name};
use crate::BackendError;
use crate::services::{MailBackend, RulesService};

const OVERRIDE: &str = "override:";

fn address_of(recipient: &Recipient) -> Option<String> {
    recipient.email_address.address.clone()
}

fn recipient(address: &str) -> Recipient {
    Recipient { email_address: EmailAddress { name: None, address: Some(address.to_string()) } }
}

fn is_empty(p: &RulePredicates) -> bool {
    p.from_addresses.is_empty()
        && p.sent_to_addresses.is_empty()
        && p.subject_contains.is_empty()
        && p.body_or_subject_contains.is_empty()
        && p.has_attachments.is_none()
        && p.other.is_empty()
}

/// Whether a condition holds more than a filter can keep: several values
/// for one field, or a key the app has no word for.
fn too_much(p: &RulePredicates) -> bool {
    p.from_addresses.len() > 1
        || p.sent_to_addresses.len() > 1
        || p.subject_contains.len() > 1
        || p.body_or_subject_contains.len() > 1
        || p.has_attachments == Some(false)
        || !p.other.is_empty()
}

/// The sender and whether it files under Other, when the filter names
/// only a sender and only adds or removes the Other category.
fn focus_rule(filter: &Filter) -> Option<(String, bool)> {
    let sender = filter.criteria.from.clone()?;
    let bare = FilterCriteria { from: Some(sender.clone()), ..FilterCriteria::default() };
    if filter.criteria != bare || filter.action.forward.is_some() {
        return None;
    }
    let other = [MailSet::Category(category::OTHER.into())];
    match (filter.action.add.as_slice(), filter.action.remove.as_slice()) {
        (add, []) if add == other => Some((sender, true)),
        ([], remove) if remove == other => Some((sender, false)),
        _ => None,
    }
}

fn filter_of_override(o: &Override) -> Option<Filter> {
    let sender = o.sender_email_address.address.clone()?;
    let set = vec![MailSet::Category(category::OTHER.into())];
    let (add, remove) = if o.classify_as == "other" { (set, vec![]) } else { (vec![], set) };
    Some(Filter {
        id: Some(format!("{OVERRIDE}{}", o.id)),
        criteria: FilterCriteria { from: Some(sender), ..FilterCriteria::default() },
        action: FilterAction { add, remove, forward: None },
        read_only: false,
    })
}

/// What the rule matches, in English: Outlook shows it as a name.
fn describe(criteria: &FilterCriteria) -> String {
    let mut parts = Vec::new();
    if let Some(from) = &criteria.from {
        parts.push(format!("from {from}"));
    }
    if let Some(to) = &criteria.to {
        parts.push(format!("to {to}"));
    }
    if let Some(subject) = &criteria.subject {
        parts.push(format!("subject {subject}"));
    }
    if let Some(query) = &criteria.query {
        parts.push(format!("words {query}"));
    }
    if criteria.has_attachment {
        parts.push("with attachments".into());
    }
    format!("Penguin Mail: {}", parts.join(", "))
}

fn cannot() -> BackendError {
    BackendError::Refused(gettext("Outlook's rules cannot do that."))
}

impl<G: GraphApi> Microsoft<G> {
    fn filter_of(&self, rule: &MessageRule) -> Filter {
        let conditions = rule.conditions.clone().unwrap_or_default();
        let actions = rule.actions.clone().unwrap_or_default();
        let forward = actions.forward_to.first().and_then(address_of);
        let read_only = rule.is_read_only
            || rule.has_error
            || !rule.is_enabled
            || rule.exceptions.as_ref().is_some_and(|e| !is_empty(e))
            || too_much(&conditions)
            || !actions.other.is_empty()
            || actions.forward_to.len() > 1
            // A forward whose address Graph left out would be lost on a rewrite.
            || (actions.forward_to.len() == 1 && forward.is_none());
        let criteria = FilterCriteria {
            from: conditions.from_addresses.first().and_then(address_of),
            to: conditions.sent_to_addresses.first().and_then(address_of),
            subject: conditions.subject_contains.first().cloned(),
            query: conditions.body_or_subject_contains.first().cloned(),
            has_attachment: conditions.has_attachments == Some(true),
            ..FilterCriteria::default()
        };
        let mut action = FilterAction { forward, ..FilterAction::default() };
        if let Some(folder) = &actions.move_to_folder {
            action.add.push(self.set_of(folder));
        }
        if actions.mark_as_read == Some(true) {
            action.remove.push(MailSet::Unseen);
        }
        if actions.delete == Some(true) {
            action.add.push(MailSet::Role(Role::Trash));
        }
        action.add.extend(actions.assign_categories.iter().map(|name| MailSet::Mailbox(tag_id(name))));
        Filter { id: Some(rule.id.clone()), criteria, action, read_only }
    }

    /// The folder a rule moves to for a set, when Outlook can move there.
    fn folder_for(&self, set: &MailSet) -> Result<String, BackendError> {
        match set {
            MailSet::Mailbox(id) if tag_name(id).is_none() => Ok(id.clone()),
            MailSet::Role(role) => self.known().roles.get(role).cloned().ok_or_else(cannot),
            _ => Err(cannot()),
        }
    }

    fn rule_of(&self, filter: &Filter, sequence: u32) -> Result<MessageRule, BackendError> {
        let c = &filter.criteria;
        if c.negated_query.is_some() || c.exclude_chats || c.size.is_some() || c.size_comparison.is_some() {
            return Err(cannot());
        }
        let conditions = RulePredicates {
            from_addresses: c.from.iter().map(|a| recipient(a)).collect(),
            sent_to_addresses: c.to.iter().map(|a| recipient(a)).collect(),
            subject_contains: c.subject.iter().cloned().collect(),
            body_or_subject_contains: c.query.iter().cloned().collect(),
            has_attachments: c.has_attachment.then_some(true),
            ..RulePredicates::default()
        };
        if is_empty(&conditions) {
            return Err(cannot());
        }
        let mut actions = RuleActions { stop_processing_rules: Some(false), ..RuleActions::default() };
        let mut moves: Vec<String> = Vec::new();
        for set in &filter.action.add {
            match set {
                MailSet::Role(Role::Trash) => actions.delete = Some(true),
                MailSet::Mailbox(id) if tag_name(id).is_some() => {
                    actions.assign_categories.extend(tag_name(id).map(str::to_string));
                }
                other => moves.push(self.folder_for(other)?),
            }
        }
        let mut skips_inbox = false;
        for set in &filter.action.remove {
            match set {
                MailSet::Unseen => actions.mark_as_read = Some(true),
                MailSet::Role(Role::Inbox) => skips_inbox = true,
                _ => return Err(cannot()),
            }
        }
        if skips_inbox && actions.delete != Some(true) {
            // Gmail's "skip the inbox": the Archive is where such mail goes.
            moves.push(self.folder_for(&MailSet::Role(Role::Archive))?);
        }
        if moves.len() > 1 || (actions.delete == Some(true) && !moves.is_empty()) {
            return Err(cannot());
        }
        actions.move_to_folder = moves.pop();
        actions.forward_to = filter.action.forward.iter().map(|a| recipient(a)).collect();
        let nothing = actions.delete.is_none()
            && actions.move_to_folder.is_none()
            && actions.mark_as_read.is_none()
            && actions.assign_categories.is_empty()
            && actions.forward_to.is_empty();
        if nothing {
            return Err(cannot());
        }
        Ok(MessageRule {
            display_name: describe(c),
            sequence,
            is_enabled: true,
            conditions: Some(conditions),
            actions: Some(actions),
            ..MessageRule::default()
        })
    }
}

impl<G: GraphApi> RulesService for Microsoft<G> {
    async fn filters(&self) -> Result<Vec<Filter>, BackendError> {
        self.synced().await?;
        let rules = self.graph().rules().await.map_err(|e| self.service_error(Service::Rules, e))?;
        let overrides = self.graph().overrides().await.map_err(|e| self.service_error(Service::Rules, e))?;
        let mut filters: Vec<Filter> = rules.iter().map(|r| self.filter_of(r)).collect();
        filters.extend(overrides.iter().filter_map(filter_of_override));
        Ok(filters)
    }

    async fn create_filter(&self, filter: &Filter) -> Result<Filter, BackendError> {
        if let Some((sender, other)) = focus_rule(filter) {
            let made = self
                .graph()
                .set_override(&sender, other)
                .await
                .map_err(|e| self.service_error(Service::Rules, e))?;
            return filter_of_override(&made).ok_or_else(cannot);
        }
        self.synced().await?;
        let held = self.graph().rules().await.map_err(|e| self.service_error(Service::Rules, e))?;
        let sequence = held.iter().map(|r| r.sequence).max().unwrap_or(0) + 1;
        let rule = self.rule_of(filter, sequence)?;
        let made = self
            .graph()
            .create_rule(&rule)
            .await
            .map_err(|e| self.service_error(Service::Rules, e))?;
        Ok(self.filter_of(&made))
    }

    async fn delete_filter(&self, id: &str) -> Result<(), BackendError> {
        let gone = match id.strip_prefix(OVERRIDE) {
            Some(override_id) => self.graph().delete_override(override_id).await,
            None => self.graph().delete_rule(id).await,
        };
        gone.map_err(|e| self.service_error(Service::Rules, e))
    }
}
