//! One turn of the assistant as the pane shows it: what the model thought,
//! each tool it ran, and its reply, in the order they arrived, plus what it
//! is doing right now. The pane draws a widget per step and redraws the one
//! an event changed; everything that decides which step that is lives here,
//! away from GTK, so tests can feed events in and read the steps back.

use std::time::{Duration, Instant};

use mailrs_ai::AgentEvent;
use mailrs_domain::translate::{fill, fill_plural, gettext};
use serde_json::Value;

/// How a tool call stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    Running,
    Done,
    Failed,
}

/// One thing the model did in a turn.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Thinking {
        text: String,
        /// How long the model thought, once it has moved on.
        took: Option<Duration>,
    },
    Tool {
        id: String,
        name: String,
        input: Value,
        state: ToolState,
        /// The result the model read, empty until the call finishes.
        output: String,
    },
    Reply(String),
}

/// What the turn is doing now, for the status line under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// The request went out and nothing has come back since.
    Waiting,
    Thinking,
    /// A tool runs, by its name.
    Tool(String),
    /// A tool waits for the person to allow it.
    Approval(String),
    Writing,
}

impl Phase {
    /// The status line's words.
    pub fn label(&self) -> String {
        match self {
            Phase::Waiting => gettext("Waiting for the model…"),
            Phase::Thinking => gettext("Thinking…"),
            Phase::Tool(name) => fill(&gettext("{activity}…"), &[("activity", &tool_label(name))]),
            Phase::Approval(_) => gettext("Waiting for your answer…"),
            Phase::Writing => gettext("Writing…"),
        }
    }
}

/// Which step an event touched, by its place in [`Turn::steps`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Update {
    Added(usize),
    Changed(usize),
}

#[derive(Debug)]
pub struct Turn {
    steps: Vec<Step>,
    /// When the last event arrived, or the turn began. A model thinks from
    /// the moment it has what it needs, and some providers hand the
    /// thinking over whole once it is done, so its clock starts here.
    since: Instant,
    thinking_since: Option<Instant>,
    approval: bool,
}

impl Turn {
    pub fn new(now: Instant) -> Turn {
        Turn {
            steps: Vec::new(),
            since: now,
            thinking_since: None,
            approval: false,
        }
    }

    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Takes one event from the model and says which steps it changed.
    pub fn apply(&mut self, event: AgentEvent, now: Instant) -> Vec<Update> {
        let mut updates = Vec::new();
        match event {
            AgentEvent::Thinking(text) => {
                if let Some(Step::Thinking {
                    text: so_far,
                    took: None,
                }) = self.steps.last_mut()
                {
                    so_far.push_str(&text);
                    updates.push(Update::Changed(self.steps.len() - 1));
                } else {
                    self.thinking_since = Some(self.since);
                    self.steps.push(Step::Thinking { text, took: None });
                    updates.push(Update::Added(self.steps.len() - 1));
                }
            }
            AgentEvent::Text(text) => {
                updates.extend(self.stop_thinking(now));
                if let Some(Step::Reply(so_far)) = self.steps.last_mut() {
                    so_far.push_str(&text);
                    updates.push(Update::Changed(self.steps.len() - 1));
                } else {
                    // A reply after a tool starts fresh, and the blank lines
                    // a provider puts between rounds would open it.
                    let text = text.trim_start();
                    if !text.is_empty() {
                        self.steps.push(Step::Reply(text.to_string()));
                        updates.push(Update::Added(self.steps.len() - 1));
                    }
                }
            }
            AgentEvent::ToolStarted { id, name, input } => {
                updates.extend(self.stop_thinking(now));
                self.steps.push(Step::Tool {
                    id,
                    name,
                    input,
                    state: ToolState::Running,
                    output: String::new(),
                });
                updates.push(Update::Added(self.steps.len() - 1));
            }
            AgentEvent::ToolFinished {
                id,
                name,
                ok,
                output,
                ..
            } => {
                updates.extend(self.stop_thinking(now));
                let state = if ok {
                    ToolState::Done
                } else {
                    ToolState::Failed
                };
                match self.running(&id, &name) {
                    Some(index) => {
                        if let Step::Tool {
                            state: now_state,
                            output: now_output,
                            ..
                        } = &mut self.steps[index]
                        {
                            *now_state = state;
                            *now_output = output;
                        }
                        updates.push(Update::Changed(index));
                    }
                    // A result whose start never came through still shows.
                    None => {
                        self.steps.push(Step::Tool {
                            id,
                            name,
                            input: Value::Null,
                            state,
                            output,
                        });
                        updates.push(Update::Added(self.steps.len() - 1));
                    }
                }
            }
        }
        self.since = now;
        updates
    }

    /// Ends the turn. A tool still marked as running never answered, as
    /// when the person pressed Stop, so it shows as failed.
    pub fn finish(&mut self, now: Instant) -> Vec<Update> {
        let mut updates = self.stop_thinking(now);
        for (index, step) in self.steps.iter_mut().enumerate() {
            if let Step::Tool { state, .. } = step
                && *state == ToolState::Running
            {
                *state = ToolState::Failed;
                updates.push(Update::Changed(index));
            }
        }
        self.approval = false;
        updates
    }

    /// Marks the turn as waiting for the person to allow a tool, or done
    /// waiting.
    pub fn set_awaiting_approval(&mut self, waiting: bool) {
        self.approval = waiting;
    }

    pub fn phase(&self) -> Phase {
        let running = self.steps.iter().rev().find_map(|step| match step {
            Step::Tool {
                name,
                state: ToolState::Running,
                ..
            } => Some(name.clone()),
            _ => None,
        });
        if self.approval {
            return Phase::Approval(running.unwrap_or_default());
        }
        match self.steps.last() {
            Some(Step::Thinking { took: None, .. }) => Phase::Thinking,
            Some(Step::Reply(_)) => Phase::Writing,
            _ => running.map_or(Phase::Waiting, Phase::Tool),
        }
    }

    /// The running call a result belongs to: the one with its id, or, for
    /// a provider that gave no id, the oldest running call to that tool.
    fn running(&self, id: &str, name: &str) -> Option<usize> {
        let running = |step: &Step, matches: &dyn Fn(&str, &str) -> bool| matches!(step, Step::Tool { id: i, name: n, state: ToolState::Running, .. } if matches(i, n));
        if !id.is_empty()
            && let Some(index) = self.steps.iter().position(|s| running(s, &|i, _| i == id))
        {
            return Some(index);
        }
        self.steps
            .iter()
            .position(|s| running(s, &|_, n| n == name))
    }

    fn stop_thinking(&mut self, now: Instant) -> Vec<Update> {
        let index = self.steps.len().saturating_sub(1);
        match self.steps.last_mut() {
            Some(Step::Thinking {
                took: took @ None, ..
            }) => {
                let began = self.thinking_since.take().unwrap_or(now);
                *took = Some(now.saturating_duration_since(began));
                vec![Update::Changed(index)]
            }
            _ => Vec::new(),
        }
    }
}

/// The title of a thinking row.
pub fn thinking_title(took: Option<Duration>) -> String {
    match took {
        None => gettext("Thinking…"),
        Some(took) => {
            // Rounded up, so a quick thought never reads as zero seconds.
            let seconds = took.as_millis().div_ceil(1000).max(1) as usize;
            fill_plural(
                "Thought for {count} second",
                "Thought for {count} seconds",
                seconds,
                &[("count", &seconds.to_string())],
            )
        }
    }
}

/// What the pane calls a tool. The name on the left is the tool's own,
/// which the model knows and nobody reads.
pub fn tool_label(name: &str) -> String {
    match name {
        "get_context" => gettext("Looking at the screen"),
        "list_mail" => gettext("Reading a mailbox"),
        "search_mail" => gettext("Searching mail"),
        "read_conversation" => gettext("Reading a conversation"),
        "organize" => gettext("Organizing mail"),
        "label" => gettext("Changing labels"),
        "remind_me" => gettext("Setting reminders"),
        "draft_email" => gettext("Writing a draft"),
        "send_email" => gettext("Sending mail"),
        "block_sender" => gettext("Blocking a sender"),
        "get_automatic_reply" => gettext("Checking the automatic reply"),
        "set_automatic_reply" => gettext("Setting the automatic reply"),
        "list_rules" => gettext("Reading rules"),
        "create_rule" => gettext("Creating a rule"),
        "delete_rule" => gettext("Deleting a rule"),
        "create_label" => gettext("Creating a label"),
        "get_settings" => gettext("Reading settings"),
        "change_setting" => gettext("Changing a setting"),
        "set_signature" => gettext("Setting a signature"),
        "vip" => gettext("Updating VIPs"),
        "create_smart_mailbox" => gettext("Creating a smart mailbox"),
        "open_conversation" => gettext("Opening a conversation"),
        "categorize_sender" => gettext("Sorting a sender"),
        "dismiss_follow_up" => gettext("Dismissing a follow-up"),
        "list_hidden_addresses" => gettext("Reading hidden addresses"),
        "create_hidden_address" => gettext("Making a hidden address"),
        "set_hidden_address" => gettext("Changing a hidden address"),
        // The app's own web tools, Anthropic's, and Claude Code's.
        "web_search" | "WebSearch" => gettext("Searching the web"),
        "fetch_page" | "web_fetch" | "WebFetch" => gettext("Reading a web page"),
        _ => gettext("Working"),
    }
}

/// Longest input summary on a tool row's title line, in characters.
const SUMMARY_CHARS: usize = 60;

/// Input fields that say most about a call, first in its summary. The rest
/// follow by name, since a JSON object keeps no order of its own.
const TELLING: [&str; 8] = [
    "query", "mailbox", "category", "label", "name", "email", "to", "subject",
];

/// One line saying what a call was asked to do: the input's plain values,
/// such as "follow_up" or "invoice, 20". Lists count as their first entry,
/// and nested objects are left for the expanded row.
pub fn input_summary(input: &Value) -> String {
    let words: Vec<String> = match input {
        Value::Object(fields) => {
            let mut fields: Vec<(&String, &Value)> = fields.iter().collect();
            fields.sort_by_key(|(key, _)| {
                TELLING
                    .iter()
                    .position(|t| t == key)
                    .unwrap_or(TELLING.len())
            });
            fields.into_iter().filter_map(|(_, v)| plain(v)).collect()
        }
        Value::String(raw) => vec![raw.clone()],
        _ => Vec::new(),
    };
    let line = words.join(", ").replace('\n', " ");
    match line.char_indices().nth(SUMMARY_CHARS) {
        Some((end, _)) => format!("{}…", line[..end].trim_end()),
        None => line,
    }
}

fn plain(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.trim().to_string()).filter(|t| !t.is_empty()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(_) | Value::Null | Value::Object(_) => None,
        Value::Array(items) => items.first().and_then(plain),
    }
}

/// Most of a tool's input or result the expanded row shows, in characters.
/// A tool can hand back a whole conversation, and a label that long makes
/// the pane slow to draw.
pub const SHOWN_CHARS: usize = 20_000;

/// Text for the expanded row: JSON laid out on lines, anything else as it
/// came, cut at [`SHOWN_CHARS`] with an ellipsis.
pub fn readable(text: &str) -> String {
    let text = match serde_json::from_str::<Value>(text) {
        Ok(value @ (Value::Object(_) | Value::Array(_))) => {
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| text.to_string())
        }
        _ => text.to_string(),
    };
    match text.char_indices().nth(SHOWN_CHARS) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text,
    }
}

/// A tool's input, laid out for the expanded row.
pub fn readable_input(input: &Value) -> String {
    match input {
        Value::String(raw) => readable(raw),
        Value::Null => String::new(),
        value => readable(&value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn started(id: &str, name: &str) -> AgentEvent {
        AgentEvent::ToolStarted {
            id: id.into(),
            name: name.into(),
            input: json!({"mailbox": id}),
        }
    }

    fn finished(id: &str, name: &str, ok: bool) -> AgentEvent {
        AgentEvent::ToolFinished {
            id: id.into(),
            name: name.into(),
            ok,
            preview: String::new(),
            output: format!("result {id}"),
        }
    }

    fn states(turn: &Turn) -> Vec<(String, ToolState, String)> {
        turn.steps()
            .iter()
            .filter_map(|s| match s {
                Step::Tool {
                    id, state, output, ..
                } => Some((id.clone(), *state, output.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn results_find_their_call_by_id_even_for_the_same_tool() {
        let start = Instant::now();
        let mut turn = Turn::new(start);
        turn.apply(started("a", "list_mail"), start);
        turn.apply(started("b", "list_mail"), start);
        assert_eq!(
            turn.apply(finished("b", "list_mail", false), start),
            [Update::Changed(1)]
        );
        turn.apply(finished("a", "list_mail", true), start);
        assert_eq!(
            states(&turn),
            [
                ("a".into(), ToolState::Done, "result a".into()),
                ("b".into(), ToolState::Failed, "result b".into()),
            ]
        );
    }

    #[test]
    fn a_result_without_an_id_goes_to_the_oldest_call_by_name() {
        let start = Instant::now();
        let mut turn = Turn::new(start);
        turn.apply(started("", "search_mail"), start);
        turn.apply(started("", "search_mail"), start);
        turn.apply(finished("", "search_mail", true), start);
        let running: Vec<ToolState> = states(&turn).into_iter().map(|s| s.1).collect();
        assert_eq!(running, [ToolState::Done, ToolState::Running]);
    }

    #[test]
    fn steps_come_in_the_order_they_arrived() {
        let start = Instant::now();
        let later = start + Duration::from_millis(3200);
        let mut turn = Turn::new(start);
        assert_eq!(turn.phase(), Phase::Waiting);
        turn.apply(AgentEvent::Thinking("Look at ".into()), start);
        assert_eq!(
            turn.apply(AgentEvent::Thinking("the inbox.".into()), start),
            [Update::Changed(0)]
        );
        assert_eq!(turn.phase(), Phase::Thinking);
        // A tool call ends the thought and times it.
        assert_eq!(
            turn.apply(started("a", "list_mail"), later),
            [Update::Changed(0), Update::Added(1)]
        );
        assert_eq!(turn.phase(), Phase::Tool("list_mail".into()));
        turn.set_awaiting_approval(true);
        assert_eq!(turn.phase(), Phase::Approval("list_mail".into()));
        turn.set_awaiting_approval(false);
        turn.apply(finished("a", "list_mail", true), later);
        assert_eq!(turn.phase(), Phase::Waiting);
        // The blank lines a provider sends between rounds open nothing.
        assert!(
            turn.apply(AgentEvent::Text("\n\n".into()), later)
                .is_empty()
        );
        turn.apply(AgentEvent::Text("Two ".into()), later);
        turn.apply(AgentEvent::Text("threads.".into()), later);
        assert_eq!(turn.phase(), Phase::Writing);
        assert_eq!(
            turn.steps()[0],
            Step::Thinking {
                text: "Look at the inbox.".into(),
                took: Some(Duration::from_millis(3200)),
            }
        );
        assert_eq!(turn.steps()[2], Step::Reply("Two threads.".into()));
        assert_eq!(turn.steps().len(), 3);
    }

    #[test]
    fn thinking_after_a_tool_is_a_step_of_its_own() {
        let start = Instant::now();
        let mut turn = Turn::new(start);
        turn.apply(AgentEvent::Thinking("One.".into()), start);
        turn.apply(started("a", "list_mail"), start);
        turn.apply(finished("a", "list_mail", true), start);
        let updates = turn.apply(AgentEvent::Thinking("Two.".into()), start);
        assert_eq!(updates, [Update::Added(2)]);
    }

    #[test]
    fn stopping_fails_what_still_runs_and_times_the_thought() {
        let start = Instant::now();
        let mut turn = Turn::new(start);
        turn.apply(started("a", "send_email"), start);
        turn.apply(AgentEvent::Thinking("Hm.".into()), start);
        let updates = turn.finish(start + Duration::from_secs(2));
        assert_eq!(updates, [Update::Changed(1), Update::Changed(0)]);
        assert_eq!(states(&turn)[0].1, ToolState::Failed);
        assert!(matches!(
            turn.steps()[1],
            Step::Thinking { took: Some(_), .. }
        ));
    }

    #[test]
    fn a_thought_reads_in_whole_seconds_rounded_up() {
        assert_eq!(thinking_title(None), "Thinking…");
        assert_eq!(
            thinking_title(Some(Duration::from_millis(10))),
            "Thought for 1 second"
        );
        assert_eq!(
            thinking_title(Some(Duration::from_millis(4100))),
            "Thought for 5 seconds"
        );
    }

    #[test]
    fn a_call_sums_up_in_one_line() {
        assert_eq!(
            input_summary(&json!({"mailbox": "inbox", "category": "promotions", "limit": 20})),
            "inbox, promotions, 20"
        );
        assert_eq!(
            input_summary(&json!({"threads": ["t-1", "t-2"], "archive": true})),
            "t-1"
        );
        assert_eq!(input_summary(&json!({})), "");
        let long = input_summary(&json!({"query": "word ".repeat(40)}));
        assert!(long.ends_with('…') && long.chars().count() <= SUMMARY_CHARS + 1);
    }

    #[test]
    fn results_read_as_laid_out_json_and_stop_at_the_limit() {
        assert_eq!(readable(r#"{"hits":2}"#), "{\n  \"hits\": 2\n}");
        assert_eq!(readable("plain words"), "plain words");
        let huge = readable(&"x".repeat(SHOWN_CHARS + 50));
        assert_eq!(huge.chars().count(), SHOWN_CHARS + 1);
        assert!(huge.ends_with('…'));
        assert_eq!(readable_input(&json!("{\"query\": \"x")), "{\"query\": \"x");
    }
}
