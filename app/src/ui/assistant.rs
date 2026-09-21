//! The assistant pane on the right of the window: a chat with a model that
//! can read and organize mail through the window's tools.
//!
//! Under each question the pane shows the turn as it arrives: a row for
//! what the model thought, a row per tool call, and the reply, then a
//! status line saying what the model is doing now. `assistant::turn`
//! decides what each event changes; this file draws it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use adw::prelude::*;
use gtk::{glib, pango};
use mailrs_ai::{AgentEvent, Conversation, ProviderConfig};

use crate::assistant::sources::{self, ApprovalRequest, Toolbox, Verdict};
use crate::assistant::turn::{
    self, Phase, Step, ToolState, Turn, Update, input_summary, thinking_title, tool_label,
};
use crate::assistant::{self, Host, ToolRequest, to_pango};
use crate::core::Core;
use crate::settings::{AiProvider, Settings};
use mailrs_domain::translate::gettext;

/// Starting points shown in an empty chat. The reader types over them, so
/// they come out in the reader's language and not the model's.
fn suggestions() -> [String; 5] {
    [
        gettext("Summarize this conversation"),
        gettext("What needs a reply today?"),
        gettext("Archive newsletters older than a week"),
        gettext("Set an out-of-office reply for next week"),
        gettext("Draft a reply to the open conversation"),
    ]
}

/// How long the transcript takes to glide to its end, in milliseconds.
const SCROLL_MS: u32 = 220;

/// How close to the end, in pixels, still counts as reading the end. New
/// words follow the reader only from there, so someone reading an earlier
/// answer keeps their place.
const NEAR_END: f64 = 48.0;

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
    /// Questions from outside tool sources, answered on this thread.
    approvals: async_channel::Sender<ApprovalRequest>,
    /// Saves an Always Allow answer, keyed `source/tool`.
    on_allow: Box<dyn Fn(String)>,
    settings: SettingsSource,
    chat: RefCell<Option<(ProviderConfig, Arc<tokio::sync::Mutex<Conversation>>)>>,
    running: Cell<bool>,
    stop: RefCell<Option<async_channel::Sender<()>>>,
    /// The turn in progress.
    turn: RefCell<Option<TurnView>>,
    /// Whether the transcript follows new content to its end.
    follow: Cell<bool>,
    /// The glide to the end, retargeted as content keeps arriving.
    glide: adw::TimedAnimation,
    /// Where the view last stood, to tell which way it moved.
    scrolled_to: Cell<f64>,
}

impl AssistantPane {
    /// `requests` carries tool calls to the window. `on_setup` opens the
    /// assistant settings.
    pub fn new(
        core: Rc<Core>,
        requests: async_channel::Sender<ToolRequest>,
        settings: impl Fn() -> Settings + 'static,
        on_setup: impl Fn() + 'static,
        on_allow: impl Fn(String) + 'static,
    ) -> Rc<AssistantPane> {
        let (approvals, asked) = async_channel::unbounded::<ApprovalRequest>();
        let title = adw::WindowTitle::new(&gettext("Assistant"), "");
        let new_chat = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text(gettext("New Chat"))
            .build();
        crate::ui::name(&new_chat, &gettext("New Chat"));
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
        let suggestion_list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .valign(gtk::Align::End)
            .vexpand(true)
            .build();
        let intro = gtk::Label::builder()
            .label(gettext(
                "Ask about your mail, or tell me what to tidy. I can search, summarize, \
                 sort, draft, and change settings.",
            ))
            .wrap(true)
            .xalign(0.0)
            .css_classes(["dim-label"])
            .margin_bottom(6)
            .build();
        suggestion_list.append(&intro);
        transcript.append(&suggestion_list);
        // A viewport hands its child the child's minimum height by default,
        // which squeezes the scrolling boxes inside an opened tool row down
        // to nothing. Natural height lets each grow to its limit.
        let viewport = gtk::Viewport::builder()
            .child(&transcript)
            .vscroll_policy(gtk::ScrollablePolicy::Natural)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .child(&viewport)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();

        let entry = gtk::Entry::builder()
            .placeholder_text(gettext("Ask Penguin Mail…"))
            .hexpand(true)
            .build();
        let send = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text(gettext("Send"))
            .css_classes(["circular", "suggested-action"])
            .build();
        crate::ui::name(&entry, &gettext("Ask Penguin Mail"));
        crate::ui::name(&send, &gettext("Send"));
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
            .label(gettext("Choose a Model"))
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        let setup = adw::StatusPage::builder()
            .icon_name("penguin-mail-sparkle-symbolic")
            .title(gettext("Set Up the Assistant"))
            .description(gettext(
                "Use a local model from LM Studio, Ollama, or Unsloth, an Anthropic \
                 API key, or your Claude subscription.",
            ))
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

        // libadwaita skips the animation to its end when the desktop has
        // animations turned off, so reduced motion jumps with no check here.
        let moving = scroller.clone();
        let glide = adw::TimedAnimation::builder()
            .widget(&scroller)
            .duration(SCROLL_MS)
            .easing(adw::Easing::EaseOutCubic)
            .target(&adw::CallbackAnimationTarget::new(move |value| {
                moving.vadjustment().set_value(value)
            }))
            .build();

        let pane = Rc::new(AssistantPane {
            page,
            title,
            stack,
            transcript,
            scroller,
            suggestions: suggestion_list.clone(),
            entry,
            send,
            core,
            requests,
            approvals,
            on_allow: Box::new(on_allow),
            settings: Box::new(settings),
            chat: RefCell::new(None),
            running: Cell::new(false),
            stop: RefCell::new(None),
            turn: RefCell::new(None),
            follow: Cell::new(true),
            glide,
            scrolled_to: Cell::new(0.0),
        });
        let weak = Rc::downgrade(&pane);
        glib::spawn_future_local(async move {
            while let Ok(request) = asked.recv().await {
                let Some(pane) = weak.upgrade() else { break };
                let verdict = pane.decide(&request.question, true).await;
                if verdict == Verdict::Always {
                    (pane.on_allow)(request.key.clone());
                }
                let _ = request.reply.send(verdict).await;
            }
        });
        for text in suggestions() {
            let button = gtk::Button::builder()
                .label(&text)
                .css_classes(["flat", "assistant-suggestion"])
                .halign(gtk::Align::Start)
                .build();
            let weak = Rc::downgrade(&pane);
            button.connect_clicked(move |_| {
                if let Some(pane) = weak.upgrade() {
                    pane.ask(text.clone());
                }
            });
            suggestion_list.append(&button);
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
        pane.watch_scrolling();
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
                    "Claude".to_string()
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
        let config = match assistant::model_for(
            &(self.settings)().ai,
            crate::settings::Feature::Assistant,
        ) {
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
                    let conversation = Arc::new(tokio::sync::Mutex::new(
                        Conversation::new(config.clone(), assistant::SYSTEM_PROMPT.to_string())
                            .with_thinking(),
                    ));
                    *chat = Some((config, Arc::clone(&conversation)));
                    conversation
                }
            }
        };
        self.suggestions.set_visible(false);
        self.set_running(true);
        // Asking always goes to the end, wherever the reader had scrolled.
        self.follow.set(true);
        self.user_bubble(&text);
        self.start_turn();
        let (events, received) = async_channel::unbounded::<AgentEvent>();
        let (stop, stopped) = async_channel::bounded::<()>(1);
        *self.stop.borrow_mut() = Some(stop);
        let settings = (self.settings)();
        let web = settings.ai.web_search != crate::settings::WebSearch::Off;
        let host = Arc::new(Toolbox::new(
            Host::new(assistant::tools::specs(), self.requests.clone()),
            sources::for_settings(&settings),
            self.approvals.clone(),
            settings.assistant_allowed_tools,
        ));
        let this = Rc::clone(self);
        let late = received.clone();
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
                    conversation.set_web(web);
                    tokio::select! {
                        reply = conversation.send(text, host, events) => reply.map_err(anyhow::Error::from),
                        _ = stopped.recv() => Err(anyhow::anyhow!("Stopped.")),
                    }
                })
                .await;
            // The reply can land before the loop above has drawn every
            // event, and those belong above the end of the turn.
            while let Ok(event) = late.try_recv() {
                this.show_event(event);
            }
            if let Ok(reply) = &result {
                let wrote = this.turn.borrow().as_ref().is_some_and(|view| {
                    view.turn
                        .steps()
                        .iter()
                        .any(|step| matches!(step, Step::Reply(_)))
                });
                if !wrote && !reply.trim().is_empty() {
                    this.show_event(AgentEvent::Text(reply.clone()));
                }
            }
            this.end_turn();
            if let Err(err) = result {
                this.note(&err.to_string(), true);
            }
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
        let said = match running {
            true => gettext("Stop"),
            false => gettext("Send"),
        };
        self.send.set_tooltip_text(Some(&said));
        crate::ui::name(&self.send, &said);
        if !running {
            self.entry.grab_focus();
        }
    }

    /// Puts an empty turn under the question, with its status line below.
    fn start_turn(&self) {
        let steps = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .css_classes(["assistant-turn"])
            .build();
        let status = Status::new();
        self.transcript.append(&steps);
        self.transcript.append(&status.row);
        let mut view = TurnView {
            turn: Turn::new(Instant::now()),
            steps,
            rows: Vec::new(),
            status,
            open: (self.settings)().assistant_details_expanded,
        };
        view.show_phase();
        *self.turn.borrow_mut() = Some(view);
    }

    fn show_event(&self, event: AgentEvent) {
        if let Some(view) = self.turn.borrow_mut().as_mut() {
            let updates = view.turn.apply(event, Instant::now());
            view.draw(&updates);
            view.show_phase();
        }
    }

    /// Settles the turn and takes its status line away.
    fn end_turn(&self) {
        if let Some(mut view) = self.turn.borrow_mut().take() {
            let updates = view.turn.finish(Instant::now());
            view.draw(&updates);
            self.transcript.remove(&view.status.row);
        }
    }

    fn user_bubble(&self, text: &str) {
        let label = bubble_label("assistant-question");
        label.set_text(text);
        label.set_halign(gtk::Align::End);
        self.transcript.append(&label);
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
        if problem {
            label.announce(text, gtk::AccessibleAnnouncementPriority::High);
        }
    }

    /// Asks the user to approve an action. Resolves to their answer.
    pub async fn confirm(&self, question: &str) -> bool {
        self.decide(question, false).await != Verdict::Deny
    }

    /// Puts an approval card under the running call. With `always`, the
    /// card also offers Always Allow, which outside tools take and the mail
    /// tools do not: those follow the Ask Before Acting switch instead.
    async fn decide(&self, question: &str, always: bool) -> Verdict {
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
        let deny = gtk::Button::with_label(&gettext("Don't Allow"));
        let allow = gtk::Button::builder()
            .label(gettext("Allow"))
            .css_classes(["suggested-action"])
            .build();
        let forever = gtk::Button::builder()
            .label(gettext("Always Allow"))
            .visible(always)
            .build();
        buttons.append(&deny);
        buttons.append(&forever);
        buttons.append(&allow);
        card.append(&buttons);
        // The card joins the turn, under the call that asked for it.
        match self.turn.borrow_mut().as_mut() {
            Some(view) => {
                view.steps.append(&card);
                view.turn.set_awaiting_approval(true);
                view.show_phase();
            }
            None => self.transcript.append(&card),
        }
        let (answer, answered) = async_channel::bounded::<Verdict>(1);
        for (button, value) in [
            (&allow, Verdict::Once),
            (&forever, Verdict::Always),
            (&deny, Verdict::Deny),
        ] {
            let answer = answer.clone();
            button.connect_clicked(move |_| {
                let _ = answer.try_send(value);
            });
        }
        let verdict = answered.recv().await.unwrap_or(Verdict::Deny);
        if let Some(view) = self.turn.borrow_mut().as_mut() {
            view.turn.set_awaiting_approval(false);
            view.show_phase();
        }
        buttons.set_visible(false);
        card.append(
            &gtk::Label::builder()
                .label(match verdict {
                    Verdict::Once => gettext("Allowed"),
                    Verdict::Always => gettext("Always allowed"),
                    Verdict::Deny => gettext("Not allowed"),
                })
                .xalign(0.0)
                .css_classes(["dim-label", "caption"])
                .margin_start(12)
                .margin_bottom(10)
                .build(),
        );
        verdict
    }

    /// Follows new content down while a turn runs and the reader is at the
    /// end. Scrolling up stops the following; coming back down, or asking
    /// something new, starts it again.
    fn watch_scrolling(self: &Rc<Self>) {
        let adjustment = self.scroller.vadjustment();
        let weak = Rc::downgrade(self);
        adjustment.connect_value_changed(move |adjustment| {
            let Some(pane) = weak.upgrade() else { return };
            let value = adjustment.value();
            if pane.glide.state() == adw::AnimationState::Playing {
                // The glide only moves down, through the middle of the text,
                // and that is not the reader leaving the end. A move up while
                // it runs is the reader, and they win.
                if value < pane.scrolled_to.get() - 0.5 {
                    pane.glide.pause();
                    pane.follow.set(false);
                }
            } else {
                let end = adjustment.upper() - adjustment.page_size();
                pane.follow.set(value >= end - NEAR_END);
            }
            pane.scrolled_to.set(value);
        });
        let weak = Rc::downgrade(self);
        adjustment.connect_changed(move |_| {
            let Some(pane) = weak.upgrade() else { return };
            if pane.follow.get() {
                pane.glide_to_end();
            }
        });
    }

    /// Glides to the end of the transcript. While a glide runs, it moves
    /// its target rather than starting over, so a stream of words reads as
    /// one smooth movement.
    fn glide_to_end(&self) {
        let adjustment = self.scroller.vadjustment();
        let end = adjustment.upper() - adjustment.page_size();
        if self.glide.state() == adw::AnimationState::Playing {
            self.glide.set_value_to(end);
            return;
        }
        if (end - adjustment.value()).abs() < 0.5 {
            return;
        }
        self.glide.set_value_from(adjustment.value());
        self.glide.set_value_to(end);
        self.glide.play();
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

/// The widgets of one turn, kept in step with its [`Turn`].
struct TurnView {
    turn: Turn,
    steps: gtk::Box,
    /// One per step, in the same order.
    rows: Vec<StepRow>,
    status: Status,
    /// Whether new detail rows open as they appear.
    open: bool,
}

impl TurnView {
    fn draw(&mut self, updates: &[Update]) {
        for update in updates {
            match *update {
                Update::Added(index) => {
                    let row = StepRow::new(&self.turn.steps()[index], self.open);
                    self.steps.append(row.widget());
                    self.rows.push(row);
                }
                Update::Changed(index) => {
                    if let Some(row) = self.rows.get(index) {
                        row.show(&self.turn.steps()[index]);
                    }
                }
            }
        }
    }

    fn show_phase(&mut self) {
        self.status.show(&self.turn.phase());
    }
}

/// What a detail row says about its own state on the left.
enum Mark {
    Working,
    Thought,
    Done,
    Failed,
}

/// A row that folds to one line: an icon, a title, and a dim summary.
struct Detail {
    expander: gtk::Expander,
    mark: gtk::Box,
    title: gtk::Label,
    summary: gtk::Label,
}

impl Detail {
    fn new(open: bool) -> Detail {
        let mark = gtk::Box::builder()
            .width_request(16)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let title = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["assistant-step-title"])
            .build();
        let summary = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .css_classes(["dim-label"])
            .build();
        let header = gtk::Box::builder().spacing(8).build();
        header.append(&mark);
        header.append(&title);
        header.append(&summary);
        let expander = gtk::Expander::builder()
            .label_widget(&header)
            .expanded(open)
            .css_classes(["assistant-step"])
            .build();
        Detail {
            expander,
            mark,
            title,
            summary,
        }
    }

    fn set_mark(&self, mark: Mark) {
        while let Some(child) = self.mark.first_child() {
            self.mark.remove(&child);
        }
        let icon = match mark {
            Mark::Working => {
                self.mark.append(
                    &adw::Spinner::builder()
                        .width_request(14)
                        .height_request(14)
                        .build(),
                );
                return;
            }
            Mark::Thought => "penguin-mail-sparkle-symbolic",
            Mark::Done => "object-select-symbolic",
            Mark::Failed => "dialog-warning-symbolic",
        };
        let image = gtk::Image::from_icon_name(icon);
        image.add_css_class(match mark {
            Mark::Failed => "warning",
            _ => "dim-label",
        });
        self.mark.append(&image);
    }
}

enum StepRow {
    Thinking {
        detail: Detail,
        text: gtk::Label,
    },
    Tool {
        detail: Detail,
        input: gtk::Label,
        result: gtk::Box,
        output: gtk::Label,
    },
    Reply(gtk::Label),
}

impl StepRow {
    fn new(step: &Step, open: bool) -> StepRow {
        let row = match step {
            Step::Thinking { .. } => {
                let detail = Detail::new(open);
                let text = detail_text(&["assistant-thinking", "dim-label"]);
                text.set_margin_start(24);
                detail.expander.set_child(Some(&text));
                StepRow::Thinking { detail, text }
            }
            Step::Tool { .. } => {
                let detail = Detail::new(open);
                let input = detail_text(&["monospace"]);
                let output = detail_text(&["monospace"]);
                let result = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(4)
                    .build();
                result.append(&heading(&gettext("Result")));
                result.append(&clipped(&output, 280));
                let body = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(4)
                    .margin_start(24)
                    .css_classes(["assistant-step-body"])
                    .build();
                body.append(&heading(&gettext("Input")));
                body.append(&clipped(&input, 160));
                body.append(&result);
                detail.expander.set_child(Some(&body));
                StepRow::Tool {
                    detail,
                    input,
                    result,
                    output,
                }
            }
            Step::Reply(_) => {
                let label = bubble_label("assistant-reply");
                label.set_halign(gtk::Align::Fill);
                StepRow::Reply(label)
            }
        };
        row.show(step);
        row
    }

    fn widget(&self) -> &gtk::Widget {
        match self {
            StepRow::Thinking { detail, .. } | StepRow::Tool { detail, .. } => {
                detail.expander.upcast_ref()
            }
            StepRow::Reply(label) => label.upcast_ref(),
        }
    }

    /// Redraws the row from its step.
    fn show(&self, step: &Step) {
        match (self, step) {
            (
                StepRow::Thinking { detail, text },
                Step::Thinking {
                    text: thought,
                    took,
                },
            ) => {
                let title = thinking_title(*took);
                detail.title.set_label(&title);
                detail.set_mark(if took.is_some() {
                    Mark::Thought
                } else {
                    Mark::Working
                });
                text.set_label(thought.trim());
                crate::ui::name(&detail.expander, &title);
            }
            (
                StepRow::Tool {
                    detail,
                    input: input_label,
                    result,
                    output: output_label,
                },
                Step::Tool {
                    name,
                    input,
                    state,
                    output,
                    ..
                },
            ) => {
                let label = tool_label(name);
                let summary = input_summary(input);
                detail.title.set_label(&label);
                detail.summary.set_label(&summary);
                detail.set_mark(match state {
                    ToolState::Running => Mark::Working,
                    ToolState::Done => Mark::Done,
                    ToolState::Failed => Mark::Failed,
                });
                input_label.set_label(&turn::readable_input(input));
                result.set_visible(*state != ToolState::Running);
                output_label.set_label(&turn::readable(output));
                let said = match state {
                    ToolState::Running => gettext("Running"),
                    ToolState::Done => gettext("Done"),
                    ToolState::Failed => gettext("Failed"),
                };
                let name: Vec<&str> = [label.as_str(), summary.as_str(), said.as_str()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect();
                crate::ui::name(&detail.expander, &name.join(", "));
            }
            (StepRow::Reply(label), Step::Reply(text)) => label.set_markup(&to_pango(text)),
            _ => {}
        }
    }
}

/// Selectable text inside an expanded row.
fn detail_text(classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::builder()
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .xalign(0.0)
        .selectable(true)
        .build();
    label.add_css_class("caption");
    for class in classes {
        label.add_css_class(class);
    }
    label
}

fn heading(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["caption-heading", "dim-label"])
        .build()
}

/// `label` in a box that grows to `max` pixels and scrolls past that, so
/// one long result cannot push the rest of the chat away.
fn clipped(label: &gtk::Label, max: i32) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .child(label)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .max_content_height(max)
        .propagate_natural_height(true)
        .css_classes(["assistant-detail"])
        .build()
}

/// The line under a running turn saying what the model is doing.
struct Status {
    row: gtk::Box,
    label: gtk::Label,
    shown: Option<Phase>,
}

impl Status {
    fn new() -> Status {
        let row = gtk::Box::builder()
            .spacing(8)
            .css_classes(["assistant-status"])
            .build();
        row.append(
            &adw::Spinner::builder()
                .width_request(14)
                .height_request(14)
                .build(),
        );
        let label = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["dim-label", "caption"])
            .build();
        row.append(&label);
        Status {
            row,
            label,
            shown: None,
        }
    }

    /// Shows the phase, and says it to a screen reader when it changes. The
    /// words that stream in are not announced one by one, and the focus
    /// stays where the reader left it.
    fn show(&mut self, phase: &Phase) {
        if self.shown.as_ref() == Some(phase) {
            return;
        }
        let text = phase.label();
        self.label.set_label(&text);
        self.label
            .announce(&text, gtk::AccessibleAnnouncementPriority::Medium);
        self.shown = Some(phase.clone());
    }
}
