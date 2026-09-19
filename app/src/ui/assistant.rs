//! The assistant pane on the right of the window: a chat with a model that
//! can read and organize mail through the window's tools.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{glib, pango};
use mailrs_ai::{AgentEvent, Conversation, ProviderConfig};

use crate::assistant::{self, Host, ToolRequest, to_pango};
use crate::core::Core;
use crate::settings::{AiProvider, Settings};

/// Starting points shown in an empty chat.
const SUGGESTIONS: [&str; 5] = [
    "Summarize this conversation",
    "What needs a reply today?",
    "Archive newsletters older than a week",
    "Set an out-of-office reply for next week",
    "Draft a reply to the open conversation",
];

/// What the pane shows while a tool runs.
fn activity(name: &str) -> &'static str {
    match name {
        "get_context" => "Looking at the screen",
        "list_mail" => "Reading a mailbox",
        "search_mail" => "Searching mail",
        "read_conversation" => "Reading a conversation",
        "organize" => "Organizing mail",
        "label" => "Changing labels",
        "remind_me" => "Setting reminders",
        "draft_email" => "Writing a draft",
        "send_email" => "Sending mail",
        "block_sender" => "Blocking a sender",
        "get_automatic_reply" => "Checking the automatic reply",
        "set_automatic_reply" => "Setting the automatic reply",
        "list_rules" => "Reading rules",
        "create_rule" => "Creating a rule",
        "delete_rule" => "Deleting a rule",
        "create_label" => "Creating a label",
        "get_settings" => "Reading settings",
        "change_setting" => "Changing a setting",
        "set_signature" => "Setting a signature",
        "vip" => "Updating VIPs",
        "create_smart_mailbox" => "Creating a smart mailbox",
        "open_conversation" => "Opening a conversation",
        "categorize_sender" => "Sorting a sender",
        "dismiss_follow_up" => "Dismissing a follow-up",
        "list_hidden_addresses" => "Reading hidden addresses",
        "create_hidden_address" => "Making a hidden address",
        "set_hidden_address" => "Changing a hidden address",
        _ => "Working",
    }
}

type SettingsSource = Box<dyn Fn() -> Settings>;

pub struct AssistantPane {
    pub page: adw::ToolbarView,
    title: adw::WindowTitle,
    stack: gtk::Stack,
    transcript: gtk::Box,
    scroller: gtk::ScrolledWindow,
    suggestions: gtk::Box,
    entry: gtk::Entry,
    send: gtk::Button,
    core: Rc<Core>,
    requests: async_channel::Sender<ToolRequest>,
    settings: SettingsSource,
    chat: RefCell<Option<(ProviderConfig, Arc<tokio::sync::Mutex<Conversation>>)>>,
    running: Cell<bool>,
    stop: RefCell<Option<async_channel::Sender<()>>>,
    /// The reply being written, and its text so far.
    bubble: RefCell<Option<(gtk::Label, String)>>,
    /// Activity rows for tools in progress, by tool name.
    working: RefCell<Vec<(String, gtk::Box)>>,
}

impl AssistantPane {
    /// `requests` carries tool calls to the window. `on_setup` opens the
    /// assistant settings.
    pub fn new(
        core: Rc<Core>,
        requests: async_channel::Sender<ToolRequest>,
        settings: impl Fn() -> Settings + 'static,
        on_setup: impl Fn() + 'static,
    ) -> Rc<AssistantPane> {
        let title = adw::WindowTitle::new("Assistant", "");
        let new_chat = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("New Chat")
            .build();
        let header = adw::HeaderBar::builder()
            .title_widget(&title)
            .show_start_title_buttons(false)
            .build();
        header.pack_start(&new_chat);

        let transcript = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let suggestions = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .valign(gtk::Align::End)
            .vexpand(true)
            .build();
        let intro = gtk::Label::builder()
            .label("Ask about your mail, or tell me what to tidy. I can search, summarize, sort, draft, and change settings.")
            .wrap(true)
            .xalign(0.0)
            .css_classes(["dim-label"])
            .margin_bottom(6)
            .build();
        suggestions.append(&intro);
        transcript.append(&suggestions);
        let scroller = gtk::ScrolledWindow::builder()
            .child(&transcript)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();

        let entry = gtk::Entry::builder()
            .placeholder_text("Ask Penguin Mail…")
            .hexpand(true)
            .build();
        let send = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text("Send")
            .css_classes(["circular", "suggested-action"])
            .build();
        let input = gtk::Box::builder()
            .spacing(6)
            .margin_top(6)
            .margin_bottom(10)
            .margin_start(10)
            .margin_end(10)
            .build();
        input.append(&entry);
        input.append(&send);
        let chat = gtk::Box::new(gtk::Orientation::Vertical, 0);
        chat.append(&scroller);
        chat.append(&input);

        let setup_button = gtk::Button::builder()
            .label("Choose a Model")
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        let setup = adw::StatusPage::builder()
            .icon_name("penguin-mail-sparkle-symbolic")
            .title("Set Up the Assistant")
            .description("Use a local model from LM Studio, Ollama, or Unsloth, an Anthropic API key, or your Claude subscription.")
            .child(&setup_button)
            .build();
        setup.add_css_class("compact");

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        stack.add_named(&chat, Some("chat"));
        stack.add_named(&setup, Some("setup"));
        let page = adw::ToolbarView::new();
        page.add_top_bar(&header);
        page.set_content(Some(&stack));
        page.add_css_class("assistant-pane");

        let pane = Rc::new(AssistantPane {
            page,
            title,
            stack,
            transcript,
            scroller,
            suggestions: suggestions.clone(),
            entry,
            send,
            core,
            requests,
            settings: Box::new(settings),
            chat: RefCell::new(None),
            running: Cell::new(false),
            stop: RefCell::new(None),
            bubble: RefCell::new(None),
            working: RefCell::new(Vec::new()),
        });
        for text in SUGGESTIONS {
            let button = gtk::Button::builder()
                .label(text)
                .css_classes(["flat", "assistant-suggestion"])
                .halign(gtk::Align::Start)
                .build();
            let weak = Rc::downgrade(&pane);
            button.connect_clicked(move |_| {
                if let Some(pane) = weak.upgrade() {
                    pane.ask(text.to_string());
                }
            });
            suggestions.append(&button);
        }
        setup_button.connect_clicked(move |_| on_setup());
        let weak = Rc::downgrade(&pane);
        pane.entry.connect_activate(move |entry| {
            let Some(pane) = weak.upgrade() else { return };
            let text = entry.text().trim().to_string();
            if !text.is_empty() && !pane.running.get() {
                entry.set_text("");
                pane.ask(text);
            }
        });
        let weak = Rc::downgrade(&pane);
        pane.send.connect_clicked(move |_| {
            let Some(pane) = weak.upgrade() else { return };
            if pane.running.get() {
                if let Some(stop) = pane.stop.borrow_mut().take() {
                    let _ = stop.try_send(());
                }
            } else {
                pane.entry.emit_activate();
            }
        });
        let weak = Rc::downgrade(&pane);
        new_chat.connect_clicked(move |_| {
            if let Some(pane) = weak.upgrade() {
                pane.reset();
            }
        });
        pane.refresh();
        pane
    }

    /// Shows setup or the chat, and the model in the title.
    pub fn refresh(&self) {
        let ai = (self.settings)().ai;
        let subtitle = match ai.provider {
            AiProvider::Off => String::new(),
            AiProvider::Local => ai.local_model.clone(),
            AiProvider::Anthropic => ai.anthropic_model.clone(),
            AiProvider::ClaudeCode => {
                if ai.claude_model.is_empty() {
                    "Claude".into()
                } else {
                    format!("Claude {}", ai.claude_model)
                }
            }
        };
        self.title.set_subtitle(&subtitle);
        self.stack
            .set_visible_child_name(if ai.provider == AiProvider::Off {
                "setup"
            } else {
                "chat"
            });
    }

    pub fn focus(&self) {
        self.entry.grab_focus();
    }

    /// Starts over with an empty chat.
    fn reset(&self) {
        if self.running.get() {
            return;
        }
        *self.chat.borrow_mut() = None;
        while let Some(child) = self.transcript.last_child() {
            if child == self.suggestions.clone().upcast::<gtk::Widget>() {
                break;
            }
            self.transcript.remove(&child);
        }
        self.suggestions.set_visible(true);
    }

    /// Asks the assistant something, as if typed.
    pub fn ask(self: &Rc<Self>, text: String) {
        if self.running.get() {
            return;
        }
        let config = match assistant::provider_config(&(self.settings)().ai) {
            Ok(config) => config,
            Err(problem) => {
                self.refresh();
                return self.note(&problem, true);
            }
        };
        let conversation = {
            let mut chat = self.chat.borrow_mut();
            match chat.as_ref() {
                Some((current, conversation)) if *current == config => Arc::clone(conversation),
                _ => {
                    let conversation = Arc::new(tokio::sync::Mutex::new(Conversation::new(
                        config.clone(),
                        assistant::SYSTEM_PROMPT.to_string(),
                    )));
                    *chat = Some((config, Arc::clone(&conversation)));
                    conversation
                }
            }
        };
        self.suggestions.set_visible(false);
        self.user_bubble(&text);
        self.set_running(true);
        let (events, received) = async_channel::unbounded::<AgentEvent>();
        let (stop, stopped) = async_channel::bounded::<()>(1);
        *self.stop.borrow_mut() = Some(stop);
        let host = Arc::new(Host::new(assistant::tools::specs(), self.requests.clone()));
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            while let Ok(event) = received.recv().await {
                this.show_event(event);
            }
        });
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let result = this
                .core
                .call(async move {
                    let mut conversation = conversation.lock().await;
                    tokio::select! {
                        reply = conversation.send(text, host, events) => reply.map_err(anyhow::Error::from),
                        _ = stopped.recv() => Err(anyhow::anyhow!("Stopped.")),
                    }
                })
                .await;
            this.finish_working(false);
            match result {
                Ok(reply) => {
                    let wrote = this
                        .bubble
                        .borrow()
                        .as_ref()
                        .is_some_and(|(_, t)| !t.is_empty());
                    if !wrote && !reply.trim().is_empty() {
                        this.append_text(&reply);
                    }
                }
                Err(err) => this.note(&err.to_string(), true),
            }
            *this.bubble.borrow_mut() = None;
            this.set_running(false);
        });
    }

    fn set_running(&self, running: bool) {
        self.running.set(running);
        self.entry.set_sensitive(!running);
        self.send.set_icon_name(if running {
            "media-playback-stop-symbolic"
        } else {
            "go-up-symbolic"
        });
        self.send
            .set_tooltip_text(Some(if running { "Stop" } else { "Send" }));
        if !running {
            self.entry.grab_focus();
        }
    }

    fn show_event(&self, event: AgentEvent) {
        match event {
            AgentEvent::Text(text) => self.append_text(&text),
            AgentEvent::ToolStarted { name, .. } => {
                *self.bubble.borrow_mut() = None;
                let row = gtk::Box::builder()
                    .spacing(8)
                    .css_classes(["assistant-activity"])
                    .build();
                row.append(
                    &adw::Spinner::builder()
                        .width_request(14)
                        .height_request(14)
                        .build(),
                );
                row.append(
                    &gtk::Label::builder()
                        .label(activity(&name))
                        .xalign(0.0)
                        .css_classes(["dim-label", "caption"])
                        .build(),
                );
                self.transcript.append(&row);
                self.working.borrow_mut().push((name, row));
                self.scroll_down();
            }
            AgentEvent::ToolFinished { name, ok, preview } => {
                let row = {
                    let mut working = self.working.borrow_mut();
                    working
                        .iter()
                        .position(|(n, _)| *n == name)
                        .map(|i| working.remove(i).1)
                };
                if let Some(row) = row {
                    settle(&row, ok, (!ok).then_some(preview.as_str()));
                }
            }
        }
    }

    /// Marks tools still shown as running as finished.
    fn finish_working(&self, ok: bool) {
        for (_, row) in self.working.borrow_mut().drain(..) {
            settle(&row, ok, None);
        }
    }

    fn append_text(&self, text: &str) {
        let mut bubble = self.bubble.borrow_mut();
        if bubble.is_none() {
            let label = bubble_label("assistant-reply");
            label.set_halign(gtk::Align::Fill);
            self.transcript.append(&label);
            *bubble = Some((label, String::new()));
        }
        if let Some((label, so_far)) = bubble.as_mut() {
            so_far.push_str(text);
            label.set_markup(&to_pango(so_far));
        }
        drop(bubble);
        self.scroll_down();
    }

    fn user_bubble(&self, text: &str) {
        let label = bubble_label("assistant-question");
        label.set_text(text);
        label.set_halign(gtk::Align::End);
        self.transcript.append(&label);
        self.scroll_down();
    }

    /// A line from the app itself, such as an error.
    fn note(&self, text: &str, problem: bool) {
        let label = gtk::Label::builder()
            .label(text)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .xalign(0.0)
            .selectable(true)
            .css_classes(if problem {
                ["assistant-note", "error"]
            } else {
                ["assistant-note", "dim-label"]
            })
            .build();
        self.transcript.append(&label);
        self.scroll_down();
    }

    /// Asks the user to approve an action. Resolves to their answer.
    pub async fn confirm(&self, question: &str) -> bool {
        let card = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .css_classes(["card", "assistant-confirm"])
            .build();
        card.append(
            &gtk::Label::builder()
                .label(question)
                .wrap(true)
                .xalign(0.0)
                .margin_top(12)
                .margin_start(12)
                .margin_end(12)
                .build(),
        );
        let buttons = gtk::Box::builder()
            .spacing(8)
            .halign(gtk::Align::End)
            .margin_bottom(12)
            .margin_end(12)
            .build();
        let deny = gtk::Button::with_label("Don't Allow");
        let allow = gtk::Button::builder()
            .label("Allow")
            .css_classes(["suggested-action"])
            .build();
        buttons.append(&deny);
        buttons.append(&allow);
        card.append(&buttons);
        self.transcript.append(&card);
        self.scroll_down();
        let (answer, answered) = async_channel::bounded::<bool>(1);
        for (button, value) in [(&allow, true), (&deny, false)] {
            let answer = answer.clone();
            button.connect_clicked(move |_| {
                let _ = answer.try_send(value);
            });
        }
        let approved = answered.recv().await.unwrap_or(false);
        buttons.set_visible(false);
        card.append(
            &gtk::Label::builder()
                .label(if approved { "Allowed" } else { "Not allowed" })
                .xalign(0.0)
                .css_classes(["dim-label", "caption"])
                .margin_start(12)
                .margin_bottom(10)
                .build(),
        );
        approved
    }

    fn scroll_down(&self) {
        let adjustment = self.scroller.vadjustment();
        glib::idle_add_local_once(move || {
            adjustment.set_value(adjustment.upper() - adjustment.page_size());
        });
    }
}

fn bubble_label(class: &str) -> gtk::Label {
    gtk::Label::builder()
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .xalign(0.0)
        .selectable(true)
        .css_classes(["assistant-bubble", class])
        .build()
}

/// Replaces a running tool's spinner with a result mark.
fn settle(row: &gtk::Box, ok: bool, problem: Option<&str>) {
    if let Some(spinner) = row.first_child() {
        row.remove(&spinner);
    }
    let mark = gtk::Image::from_icon_name(if ok {
        "object-select-symbolic"
    } else {
        "dialog-warning-symbolic"
    });
    mark.add_css_class("dim-label");
    row.prepend(&mark);
    if let Some(problem) = problem.filter(|p| !p.is_empty()) {
        row.set_tooltip_text(Some(problem));
    }
}
