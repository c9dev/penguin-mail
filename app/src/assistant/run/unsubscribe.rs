//! The newsletters an account gets, and leaving several of them at once.
//!
//! `list_newsletters` answers from the store, so asking who keeps
//! writing costs nothing but the few metadata fetches
//! [`mailrs_sync::Newsletters`] makes for mail stored before the header
//! was kept. `unsubscribe` takes up to twenty conversations, reads every
//! page out of sight, asks once, and then leaves each list in turn.
//!
//! The question is the dialog, not the pane's approval card. A page
//! submission waits for the person whatever Ask Before Acting says, and
//! the dialog is the only question that can name the button and the
//! address it is about to use, so this tool asks through
//! [`crate::ui::unsubscribe::confirm`] and treats its answer as the
//! approval. A card as well would ask the same thing twice, the second
//! time in vaguer words.

use futures::future::{Either, select};
use mailrs_store::bodies;
use mailrs_store::newsletters::Sender;
use mailrs_sync::Newsletters;

use super::*;
use crate::ui::unsubscribe::{ListLine, Way, sent_to};
use crate::unsubscribe::{Unsubscribe, choose_with_body};
use crate::unsubscribe_page::{Browser, Outcome as PageOutcome, Prepared, finish, prepare};

/// The most lists one call may leave. Twenty pages already take minutes,
/// and a dialog longer than that is one nobody reads before pressing.
const MOST_LISTS: usize = 20;

/// The most senders a listing hands the model.
const MOST_SENDERS: usize = 50;

/// One list an `unsubscribe` call named, as far as it could be worked
/// out before the question.
struct Leaving {
    account: Account,
    thread_id: String,
    /// The sender as a person reads it, which is what the dialog and the
    /// answer both call this list.
    name: String,
    state: State,
}

/// Whether a list can be left at all.
enum State {
    /// Nothing to leave by. The reason goes back as this list's outcome
    /// and the dialog never hears about it.
    Failed(String),
    Ready {
        how: Unsubscribe,
        /// The address the newsletter was sent to, for a page to type.
        address: String,
    },
}

/// How one list ended: the word the model reads, and the sentence under
/// it when there is one.
type Ended = (&'static str, Option<String>);

/// What the model hears about the way a sender lets go.
fn way_out_key(way: Option<&Unsubscribe>) -> &'static str {
    match way {
        Some(Unsubscribe::OneClick(_)) => "one_click",
        Some(Unsubscribe::Page(_)) => "page",
        Some(Unsubscribe::Email { .. }) => "email",
        Some(Unsubscribe::BodyLink(_)) => "body_link",
        None => "none",
    }
}

/// Whether a sender matches what the call asked for: every word of the
/// query, in the name or the address.
fn matches(sender: &Sender, words: &[String]) -> bool {
    let text = format!("{} {}", sender.name, sender.email).to_lowercase();
    words.iter().all(|word| text.contains(word))
}

impl<A: Accounts> Tools<A> {
    pub(super) async fn list_newsletters(&self, input: &Value) -> ToolResult {
        let words: Vec<String> = text(input, "query")
            .map(|query| query.split_whitespace().map(str::to_lowercase).collect())
            .unwrap_or_default();
        let accounts = match text(input, "account") {
            Some(email) => vec![self.account_named(&email)?],
            None => self.desk.accounts(),
        };
        let mut found: Vec<(Account, Sender)> = Vec::new();
        let mut problem: Option<String> = None;
        for account in accounts {
            let newsletters =
                Newsletters::new(Arc::clone(&self.modules.accounts), self.modules.db.clone());
            let id = account.id;
            // One account being away is not the call failing: the others
            // still answer, and the reason goes back only when nothing
            // did.
            match self.call(async move { newsletters.list(id).await }).await {
                Ok(senders) => found.extend(
                    senders
                        .into_iter()
                        .filter(|sender| matches(sender, &words))
                        .map(|sender| (account.clone(), sender)),
                ),
                Err(err) => {
                    if problem.is_none() {
                        problem = Some(err);
                    }
                }
            }
        }
        if let (true, Some(problem)) = (found.is_empty(), problem) {
            return Err(problem);
        }
        found.sort_by_key(|(_, sender)| std::cmp::Reverse(sender.last));
        let cut = found.len().saturating_sub(MOST_SENDERS);
        let mut newsletters = Vec::with_capacity(found.len().min(MOST_SENDERS));
        for (account, sender) in found.into_iter().take(MOST_SENDERS) {
            let way = self.way_out(account.id, &sender).await;
            newsletters.push(json!({
                "name": sender.name,
                "email": sender.email,
                "messages": sender.messages,
                "last": crate::format::local(sender.last)
                    .map(|at| at.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default(),
                "way_out": way_out_key(way.as_ref()),
                "account": account.email,
                "thread_id": sender.thread_id,
            }));
        }
        let mut result = json!({"newsletters": newsletters});
        if cut > 0 {
            result["more"] = json!(format!(
                "{cut} more senders were left out. Narrow the list with query."
            ));
        }
        Ok(result)
    }

    /// The way out of one sender's list, from the header the store kept
    /// with their newest message. A sender with no header may still have
    /// a link in the body, which is worth reading only where the body is
    /// stored already: a listing must not cost one fetch a sender.
    async fn way_out(&self, account_id: AccountId, sender: &Sender) -> Option<Unsubscribe> {
        if let Some(way) = choose_with_body(sender.header.as_deref(), sender.one_click, None) {
            return Some(way);
        }
        let id = sender.message_id.clone();
        let body = self
            .read(move |c| bodies::peek_body(c, account_id, &id))
            .await
            .ok()??;
        choose_with_body(None, false, body.html.as_deref())
    }

    pub(super) async fn unsubscribe(&self, input: &Value) -> ToolResult {
        let named = self.named_conversations(input)?;
        let mut lists = Vec::with_capacity(named.len());
        for (account, thread_id) in named {
            lists.push(self.leaving(account, thread_id).await);
        }

        // The dialog gets a line for every list there is a way out of,
        // and `belongs` keeps which list each of its lines came from.
        let mut lines = Vec::new();
        let mut belongs = Vec::new();
        let mut pages = Vec::new();
        for (at, list) in lists.iter().enumerate() {
            let State::Ready { how, address } = &list.state else {
                continue;
            };
            let way = match how {
                Unsubscribe::OneClick(_) => Way::OneClick,
                Unsubscribe::Email { .. } => Way::Mail {
                    from: list.account.email.clone(),
                },
                Unsubscribe::Page(url) | Unsubscribe::BodyLink(url) => {
                    pages.push((lines.len(), url.clone(), address.clone()));
                    Way::Reading
                }
            };
            lines.push(ListLine {
                name: list.name.clone(),
                way,
            });
            belongs.push(at);
        }
        if lines.is_empty() {
            return Ok(self.outcomes(&lists, HashMap::new()));
        }

        // One hidden view for the whole run and one model beside it,
        // both held until the last page has been submitted. The pages go
        // through it one after another: a second view would be a second
        // page loading, which is a second sender told that the person
        // acted before they have said yes to anything.
        let browser = (!pages.is_empty()).then(|| self.effects.page_browser());
        let adviser = self.effects.page_adviser();
        let (tell, hear) = async_channel::bounded(1);
        let reading = async {
            if let Some(browser) = &browser {
                let adviser = adviser.as_deref();
                for (line, url, address) in pages {
                    let prepared = prepare(&**browser, adviser, &url, &address).await;
                    if tell.send((line, Way::Page(prepared))).await.is_err() {
                        break;
                    }
                }
            }
            // Whatever happens, the dialog has to hear that no line is
            // still being read.
            drop(tell);
        };
        let asking = self.effects.confirm_unsubscribe(lines, hear);
        futures::pin_mut!(reading, asking);
        let ticked = match select(asking, reading).await {
            // The person answered while a page was still loading. The
            // read goes no further: the view is dropped when this call
            // returns.
            Either::Left((answer, _)) => answer,
            Either::Right((_, asking)) => asking.await,
        };

        let mut ended: HashMap<usize, Ended> = HashMap::new();
        let Some(ticked) = ticked else {
            for at in belongs {
                ended.insert(at, ("declined", Some("The user said no.".into())));
            }
            return Ok(self.outcomes(&lists, ended));
        };
        for (line, way) in ticked {
            let Some(&at) = belongs.get(line) else {
                continue;
            };
            let list = &lists[at];
            let outcome = match way {
                // Unsubscribe stays insensitive while a line is still
                // being read, so a ticked line has settled. A page that
                // arrives here unread is one nothing knows what to press
                // on, and pressing blind is the one thing this must not
                // do.
                Way::Reading => ("failed", Some("That page had not finished loading.".into())),
                Way::OneClick | Way::Mail { .. } => self.leave(list).await,
                Way::Page(prepared) => match &browser {
                    Some(browser) => self.finish_page(list, &**browser, &prepared).await,
                    None => continue,
                },
            };
            ended.insert(at, outcome);
        }
        for at in belongs {
            ended
                .entry(at)
                .or_insert(("declined", Some("The user unticked this list.".into())));
        }
        Ok(self.outcomes(&lists, ended))
    }

    /// The conversations an `unsubscribe` call named: an account and a
    /// thread each. The count is checked before a page is loaded,
    /// because loading one tells that sender the person acted.
    fn named_conversations(&self, input: &Value) -> Result<Vec<(Account, String)>, String> {
        let items = input
            .get("conversations")
            .and_then(Value::as_array)
            .ok_or("`conversations` is missing")?;
        if items.is_empty() {
            return Err("`conversations` is empty".into());
        }
        if items.len() > MOST_LISTS {
            return Err(format!(
                "unsubscribe leaves at most {MOST_LISTS} lists at a time, and that call named {}. Ask about the rest afterwards.",
                items.len()
            ));
        }
        items
            .iter()
            .map(|item| {
                let account = self.account_named(&required(item, "account")?)?;
                Ok((account, required(item, "thread_id")?))
            })
            .collect()
    }

    /// One conversation as the run needs it: who the list is, and the
    /// way out of it.
    async fn leaving(&self, account: Account, thread_id: String) -> Leaving {
        let (name, state) = match self.sender_and_way(&account, &thread_id).await {
            Ok((name, Some((how, address)))) => (name, State::Ready { how, address }),
            Ok((name, None)) => (
                name,
                State::Failed(
                    "That conversation has no unsubscribe link Penguin Mail can use.".into(),
                ),
            ),
            Err(why) => (thread_id.clone(), State::Failed(why)),
        };
        Leaving {
            account,
            thread_id,
            name,
            state,
        }
    }

    /// Who wrote the conversation, and how their list lets go. The
    /// stored headers answer for mail synced since Penguin Mail started
    /// keeping them, so only a conversation they say nothing about costs
    /// a body, which is also the one place a link in the body can be
    /// found.
    async fn sender_and_way(
        &self,
        account: &Account,
        thread_id: &str,
    ) -> Result<(String, Option<(Unsubscribe, String)>), String> {
        let sync = self
            .modules
            .accounts
            .account(account.id)
            .ok_or_else(|| format!("{} is not connected.", account.email))?;
        let (s, t) = (Arc::clone(&sync), thread_id.to_string());
        if let Err(err) = self.call(async move { s.ensure_thread(&t).await }).await {
            tracing::info!(error = %err, "reading the stored copy of the thread");
        }
        let (id, key) = (account.id, thread_id.to_string());
        let metas = self
            .read(move |c| messages::thread_messages(c, id, &key))
            .await?;
        let newest = metas.last().ok_or("That conversation was not found.")?;
        let name = newest
            .from
            .as_ref()
            .map(|a| a.display().to_string())
            .unwrap_or_else(|| gettext("this list"));
        let to_and_cc: Vec<String> = newest
            .to
            .iter()
            .chain(newest.cc.iter())
            .map(|a| a.email.clone())
            .collect();
        let settings = self.desk.settings();
        let mine: Vec<String> = settings
            .senders(&account.email)
            .into_iter()
            .map(|a| a.email)
            .collect();
        let address = sent_to(&to_and_cc, &mine, &account.email);
        let stored = metas.iter().rev().find_map(|meta| {
            choose_with_body(meta.list_unsubscribe.as_deref(), meta.one_click, None)
        });
        if let Some(how) = stored {
            return Ok((name, Some((how, address))));
        }
        for meta in metas.iter().rev() {
            let (s, id) = (Arc::clone(&sync), meta.id.clone());
            let Ok(body) = self.call(async move { s.body(&id).await }).await else {
                continue;
            };
            let found = choose_with_body(
                body.list_unsubscribe.as_deref(),
                body.one_click_unsubscribe,
                body.html.as_deref(),
            );
            if let Some(how) = found {
                return Ok((name, Some((how, address))));
            }
        }
        Ok((name, None))
    }

    /// Leaves a list the way that needs no page: the one-click request,
    /// or the mail the window sends from the account.
    async fn leave(&self, list: &Leaving) -> Ended {
        let State::Ready { how, .. } = &list.state else {
            return ("failed", Some("That list has no way out.".into()));
        };
        match self.effects.unsubscribe(list.account.id, how.clone()).await {
            Ok(()) => ("done", None),
            Err(why) => ("failed", Some(why)),
        }
    }

    /// Submits the page the person approved, and says how it ended. A
    /// page nobody could read opens in their browser, the way it did
    /// before any of this: the window is asked to open it through the
    /// same effect the other two ways go through.
    async fn finish_page(
        &self,
        list: &Leaving,
        browser: &dyn Browser,
        prepared: &Prepared,
    ) -> Ended {
        match finish(browser, prepared).await {
            PageOutcome::Done => ("done", None),
            PageOutcome::Unclear => (
                "unclear",
                Some(format!(
                    "The form went in and {} did not say whether it worked.",
                    prepared.url
                )),
            ),
            PageOutcome::Failed(why) => ("failed", Some(why)),
            PageOutcome::OpenInBrowser(url) => {
                let opening = self
                    .effects
                    .unsubscribe(list.account.id, Unsubscribe::Page(url.clone()))
                    .await;
                match opening {
                    Ok(()) => (
                        "opened",
                        Some(format!(
                            "Penguin Mail could not read {url}, so it opened in the user's browser for them to finish."
                        )),
                    ),
                    Err(why) => ("failed", Some(why)),
                }
            }
        }
    }

    /// One line per list, in the order the call named them, so the model
    /// can match an outcome to what it asked for.
    fn outcomes(&self, lists: &[Leaving], mut ended: HashMap<usize, Ended>) -> Value {
        let rows: Vec<Value> = lists
            .iter()
            .enumerate()
            .map(|(at, list)| {
                let (outcome, reason) = match (ended.remove(&at), &list.state) {
                    (Some(ended), _) => ended,
                    (None, State::Failed(why)) => ("failed", Some(why.clone())),
                    (None, State::Ready { .. }) => ("declined", None),
                };
                let mut row = json!({
                    "name": list.name,
                    "account": list.account.email,
                    "thread_id": list.thread_id,
                    "outcome": outcome,
                });
                if let Some(reason) = reason {
                    row["reason"] = json!(reason);
                }
                row
            })
            .collect();
        json!({"lists": rows})
    }
}
