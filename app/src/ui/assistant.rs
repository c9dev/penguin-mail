//! The assistant pane on the right of the window: a chat with a model that
//! can read and organize mail through the window's tools.
//!
//! Under each question the pane groups thoughts and tool calls in one
//! disclosure, then shows the reply and the current activity. `assistant::turn`
//! decides what each event changes; this file draws it.

mod controls;
mod files;
mod model_catalog;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use adw::prelude::*;
use gtk::{gdk, glib, pango};
use mailrs_ai::AgentEvent;

use crate::assistant::Analysis;
use crate::assistant::chat::{Chat, Question, Tools};
use crate::assistant::sources::{ApprovalRequest, Verdict};
use crate::assistant::turn::{
    self, Phase, Step, ToolState, Turn, Update, thinking_title, tool_label,
};
use crate::assistant::{self, ToolRequest, to_pango};
use crate::core::Core;
use crate::settings::{AiProvider, Settings};
use mailrs_domain::translate::gettext;

/// Starting points shown in an empty chat. The reader types over them, so
/// they come out in the reader's language and not the model's.
fn suggestions() -> [String; 6] {
    [
        gettext("Summarize this conversation"),
        gettext("What needs a reply today?"),
        gettext("Archive newsletters older than a week"),
        gettext("Set an out-of-office reply for next week"),
        gettext("Draft a reply to the open conversation"),
        gettext("Explain this conversation"),
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
    pub header: adw::HeaderBar,
    pub detach: gtk::Button,
    pub close: gtk::Button,
    model: gtk::MenuButton,
    model_name: gtk::Label,
    known_model: RefCell<Option<(AiProvider, String, String)>>,
    model_catalog: RefCell<model_catalog::ModelCache>,
    files: Rc<files::Files>,
    mcp: Cell<bool>,
    skills: Cell<bool>,
    pending: RefCell<Vec<async_channel::Sender<Verdict>>>,
    stack: gtk::Stack,
    transcript: gtk::Box,
    scroller: gtk::ScrolledWindow,
    suggestions: gtk::Box,
    /// The question box: several lines that wrap, sent with Enter.
    input: gtk::TextView,
    send: gtk::Button,
    core: Rc<Core>,
    /// Saves an Always Allow answer, keyed `source/tool`.
    on_allow: Box<dyn Fn(String)>,
    on_analyze: Box<dyn Fn(Analysis)>,
    settings: SettingsSource,
    chat: RefCell<Chat>,
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
        on_change: impl Fn(crate::settings::Change) + 'static,
        on_analyze: impl Fn(Analysis) + 'static,
    ) -> Rc<AssistantPane> {
        let (approvals, asked) = async_channel::unbounded::<ApprovalRequest>();
        let model = gtk::MenuButton::builder()
            .css_classes(["flat", "assistant-model"])
            .tooltip_text(gettext("Choose a Model"))
            .valign(gtk::Align::Center)
            .build();
        crate::ui::name(&model, &gettext("Choose a Model"));
        let model_name = gtk::Label::builder()
            .ellipsize(pango::EllipsizeMode::End)
            .max_width_chars(24)
            .build();
        let model_title = gtk::Box::builder().spacing(4).build();
        model_title.append(&model_name);
        model_title.append(&gtk::Image::from_icon_name(
            "penguin-mail-chevron-down-symbolic",
        ));
        model.set_child(Some(&model_title));
        let model_capsule = gtk::Box::builder()
            .css_classes(["toolbar-capsule"])
            .valign(gtk::Align::Center)
            .build();
        model_capsule.append(&model);
        let detach = controls::icon_button("window-new-symbolic", &gettext("Detach"));
        let close = controls::icon_button("window-close-symbolic", &gettext("Close"));
        close.set_visible(false);
        let new_chat = controls::icon_button("list-add-symbolic", &gettext("New Chat"));
        let header = adw::HeaderBar::builder()
            .show_title(false)
            .show_start_title_buttons(false)
            .show_end_title_buttons(true)
            .build();
        header.pack_start(&new_chat);
        header.pack_start(&close);
        header.pack_end(&detach);

        let transcript = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(16)
            .margin_end(16)
            .build();
        let suggestion_list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .valign(gtk::Align::End)
            .vexpand(true)
            .build();
        suggestion_list.add_css_class("assistant-starters");
        let intro = gtk::Label::builder()
            .label(gettext(
                "Ask about your mail, or tell me what to tidy. I can search, summarize, \
                 sort, draft, and change settings.",
            ))
            .wrap(true)
            .xalign(0.0)
            .css_classes(["dim-label", "assistant-intro"])
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

        let (field, input) = question_box();
        let send = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text(gettext("Send"))
            .css_classes(["circular", "suggested-action"])
            // Stays by the last line as the question grows.
            .valign(gtk::Align::End)
            .build();
        crate::ui::name(&send, &gettext("Send"));
        let files = files::Files::new();
        let attach = gtk::Button::builder()
            .icon_name("mail-attachment-symbolic")
            .css_classes(["flat", "circular"])
            .tooltip_text(gettext("Attach"))
            .build();
        crate::ui::name(&attach, &gettext("Attach"));
        let actions = gtk::Box::builder().spacing(6).build();
        actions.append(&model_capsule);
        actions.append(&gtk::Box::builder().hexpand(true).build());
        actions.append(&attach);
        actions.append(&send);
        let input_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(6)
            .margin_bottom(10)
            .margin_start(10)
            .margin_end(10)
            .build();
        input_row.add_css_class("assistant-input-card");
        input_row.append(&files.row);
        input_row.append(&field);
        input_row.append(&actions);
        let chat = gtk::Box::new(gtk::Orientation::Vertical, 0);
        chat.append(&scroller);
        chat.append(&input_row);

        let setup_button = gtk::Button::builder()
            .label(gettext("Choose a Model"))
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        let setup = adw::StatusPage::builder()
            .icon_name("penguin-mail-sparkle-symbolic")
            .title(gettext("Set Up the Assistant"))
            .description(gettext(
                "Use a local model from LM Studio, Ollama, or any server with OpenAI's \
                 API. You can also use an Anthropic API key or your Claude subscription.",
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
        page.add_css_class("assistant-docked");
        page.set_top_bar_style(adw::ToolbarStyle::Flat);

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
            header: header.clone(),
            detach,
            close,
            model,
            model_name,
            known_model: RefCell::new(None),
            model_catalog: RefCell::new(model_catalog::ModelCache::default()),
            files: Rc::clone(&files),
            mcp: Cell::new(true),
            skills: Cell::new(true),
            pending: RefCell::new(Vec::new()),
            stack,
            transcript,
            scroller,
            suggestions: suggestion_list.clone(),
            input,
            send,
            core,
            on_allow: Box::new(on_allow),
            on_analyze: Box::new(on_analyze),
            settings: Box::new(settings),
            chat: RefCell::new(Chat::new(requests, approvals)),
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
        let on_setup = Rc::new(on_setup);
        controls::install(&pane, &header, on_setup.clone(), Rc::new(on_change));
        let weak = Rc::downgrade(&pane);
        attach.connect_clicked(move |_| {
            if let Some(pane) = weak.upgrade()
                && let Some(window) = pane.page.root().and_downcast::<gtk::Window>()
            {
                pane.files.choose(&window);
            }
        });
        let options = suggestions();
        for (position, index) in [0, 5, 4, 1, 2, 3].into_iter().enumerate() {
            if position == 0 || position == 3 {
                let heading = gtk::Label::builder()
                    .label(if position == 0 {
                        gettext("This conversation")
                    } else {
                        gettext("Your mail")
                    })
                    .xalign(0.0)
                    .margin_top(20)
                    .css_classes(["caption-heading"])
                    .build();
                suggestion_list.append(&heading);
            }
            let text = options[index].clone();
            // A suggestion wraps rather than setting the pane's minimum
            // width: the longest one, in Portuguese, is wider than the
            // narrowest window the app allows.
            let label = gtk::Label::builder()
                .label(&text)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .xalign(0.0)
                .build();
            let button = gtk::Button::builder()
                .child(&label)
                .css_classes(["flat", "assistant-suggestion"])
                .halign(gtk::Align::Start)
                .build();
            let weak = Rc::downgrade(&pane);
            button.connect_clicked(move |_| {
                if let Some(pane) = weak.upgrade() {
                    match index {
                        0 => (pane.on_analyze)(Analysis::Summarize),
                        5 => (pane.on_analyze)(Analysis::Explain),
                        _ => pane.ask(text.clone()),
                    }
                }
            });
            suggestion_list.append(&button);
        }
        setup_button.connect_clicked(move |_| on_setup());
        // Enter sends only once the input method has passed it on: an
        // Enter that finishes a word still being composed, in Chinese or
        // after a dead key, must not send. So the press is noted here,
        // ahead of the text view, and the new line the view then inserts
        // for it becomes the send. Shift+Enter keeps its new line.
        let enter = Rc::new(Cell::new(false));
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let noted = Rc::clone(&enter);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            if enter_sends(key, modifiers) {
                noted.set(true);
                let noted = Rc::clone(&noted);
                glib::idle_add_local_once(move || noted.set(false));
            }
            glib::Propagation::Proceed
        });
        pane.input.add_controller(keys);
        let weak = Rc::downgrade(&pane);
        pane.input
            .buffer()
            .connect_insert_text(move |buffer, _, text| {
                if text != "\n" || !enter.replace(false) {
                    return;
                }
                buffer.stop_signal_emission_by_name("insert-text");
                // The buffer is mid-insert, so it empties after.
                let weak = weak.clone();
                glib::idle_add_local_once(move || {
                    if let Some(pane) = weak.upgrade() {
                        pane.submit();
                    }
                });
            });
        let weak = Rc::downgrade(&pane);
        pane.send.connect_clicked(move |_| {
            let Some(pane) = weak.upgrade() else { return };
            if pane.running.get() {
                for answer in pane.pending.take() {
                    let _ = answer.try_send(Verdict::Deny);
                }
                if let Some(stop) = pane.stop.borrow_mut().take() {
                    let _ = stop.try_send(());
                }
            } else {
                pane.submit();
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

    /// Shows setup or the chat, and the model in the title. It reads the
    /// connection and its model and nothing else, which is what
    /// `Effect::Assistant` watches for.
    pub fn refresh(&self) {
        let ai = (self.settings)().ai;
        let model = ai.model_on(ai.provider);
        let subtitle = self
            .known_model
            .borrow()
            .as_ref()
            .filter(|(provider, id, _)| {
                *provider == ai.provider && id == model && ai.provider != AiProvider::ClaudeCode
            })
            .map(|(_, _, name)| name.clone())
            .unwrap_or_else(|| controls::model_label(ai.provider, model));
        self.model_name.set_label(&subtitle);
        self.model.set_tooltip_text(Some(&subtitle));
        crate::ui::describe(&self.model, &gettext("Choose a Model"), &subtitle);
        self.model.set_visible(assistant::offers_summary(&ai));
        self.stack
            .set_visible_child_name(if assistant::offers_summary(&ai) {
                "chat"
            } else {
                "setup"
            });
    }

    pub fn focus(&self) {
        self.input.grab_focus();
    }

    /// Asks the question in the box. While a turn runs the question stays
    /// in the box, to send once it ends.
    fn submit(self: &Rc<Self>) {
        let buffer = self.input.buffer();
        let (start, end) = buffer.bounds();
        let text = buffer.text(&start, &end, false);
        if let Some(question) = question_to_send(&text, self.running.get()) {
            buffer.set_text("");
            let (text, shown, picked) = self.files.take(&question);
            self.ask_shown(text, shown, picked);
        }
    }

    /// Starts over with an empty chat.
    fn reset(&self) {
        if self.running.get() {
            return;
        }
        self.files.clear();
        self.clear_history();
    }

    fn clear_history(&self) {
        self.chat.borrow_mut().clear();
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
        self.ask_shown(text.clone(), text, Arc::default());
    }

    pub fn analyze(self: &Rc<Self>, action: Analysis, context: String) {
        if self.running.get() {
            return;
        }
        self.reset();
        let settings = (self.settings)();
        if !settings.ai.analysis_tools {
            self.note(&gettext("Tools are off for this conversation and its follow-up questions. Choose New Chat to use tools again."), false);
        }
        let question = self.chat.borrow_mut().analyze(
            settings,
            Tools {
                mcp: self.mcp.get(),
                skills: self.skills.get(),
            },
            action,
            context,
        );
        self.show_question(question, &action.question());
    }

    fn ask_shown(self: &Rc<Self>, text: String, shown: String, picked: Arc<[std::path::PathBuf]>) {
        if self.running.get() {
            return;
        }
        let question = self.chat.borrow_mut().ask(
            (self.settings)(),
            Tools {
                mcp: self.mcp.get(),
                skills: self.skills.get(),
            },
            text,
            picked,
        );
        self.show_question(question, &shown);
    }

    fn show_question(self: &Rc<Self>, question: Result<Question, String>, shown: &str) {
        let question = match question {
            Ok(question) => question,
            Err(problem) => {
                self.refresh();
                return self.note(&problem, true);
            }
        };
        self.suggestions.set_visible(false);
        self.set_running(true);
        // Asking always goes to the end, wherever the reader had scrolled.
        self.follow.set(true);
        self.user_bubble(shown);
        self.start_turn();
        let received = question.events;
        *self.stop.borrow_mut() = Some(question.stop);
        let this = Rc::clone(self);
        let late = received.clone();
        glib::spawn_future_local(async move {
            while let Ok(event) = received.recv().await {
                this.show_event(event);
            }
        });
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let result = this.core.call(question.answer).await;
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
            this.end_turn(result.is_err());
            if let Err(err) = result {
                this.note(&err.to_string(), true);
            }
            this.set_running(false);
        });
    }

    fn set_running(&self, running: bool) {
        // The box stays open during a turn, so the next question can be
        // written while the model answers; only sending waits.
        self.running.set(running);
        self.model.set_sensitive(!running);
        if running {
            self.send.remove_css_class("suggested-action");
            self.send.add_css_class("assistant-stop");
        } else {
            self.send.add_css_class("suggested-action");
            self.send.remove_css_class("assistant-stop");
        }
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
        // The turn's end takes the focus only from nowhere, such as a
        // suggestion that hid when it was picked, never from where the
        // reader went meanwhile.
        if !running && self.input.root().is_some_and(|root| root.focus().is_none()) {
            self.input.grab_focus();
        }
    }

    /// Puts an empty turn under the question, with its status line below.
    fn start_turn(&self) {
        let steps = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .css_classes(["assistant-turn"])
            .build();
        let work = Detail::new((self.settings)().assistant_details_expanded);
        work.title.set_label(&gettext("What it did"));
        crate::ui::name(&work.expander, &gettext("What it did"));
        let details = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_start(18)
            .css_classes(["assistant-work-body"])
            .build();
        work.expander.set_child(Some(&details));
        work.expander.set_visible(false);
        steps.append(&work.expander);
        let status = Status::new();
        self.transcript.append(&steps);
        self.transcript.append(&status.row);
        let mut view = TurnView {
            turn: Turn::new(Instant::now()),
            steps,
            work,
            details,
            rows: Vec::new(),
            status,
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
    fn end_turn(&self, failed: bool) {
        for answer in self.pending.take() {
            let _ = answer.try_send(Verdict::Deny);
        }
        if let Some(mut view) = self.turn.borrow_mut().take() {
            let updates = view.turn.finish(Instant::now());
            view.draw(&updates);
            view.work
                .summary
                .set_label(&turn::work_summary(view.turn.steps()));
            view.work.set_mark(
                if failed
                    || view.turn.steps().iter().any(|s| {
                        matches!(
                            s,
                            Step::Tool {
                                state: ToolState::Failed,
                                ..
                            }
                        )
                    })
                {
                    Mark::Failed
                } else {
                    Mark::Done
                },
            );
            self.transcript.remove(&view.status.row);
        }
    }

    fn user_bubble(&self, text: &str) {
        let question = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(8)
            .margin_bottom(6)
            .build();
        question.append(&heading(&gettext("You")));
        let label = bubble_label("assistant-question");
        label.set_text(text);
        question.append(&label);
        self.transcript.append(&question);
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
                // A web address or a command line has no spaces to break
                // at, and the person has to read all of it.
                .wrap_mode(gtk::pango::WrapMode::WordChar)
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
        self.pending.borrow_mut().push(answer.clone());
        let verdict = answered.recv().await.unwrap_or(Verdict::Deny);
        self.pending
            .borrow_mut()
            .retain(|pending| !pending.same_channel(&answer));
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

/// How many lines the question box grows to before it scrolls.
const QUESTION_LINES: i32 = 4;

/// Whether a key press in the question box sends it: Enter, alone or with
/// Control. Shift+Enter starts a new line instead.
fn enter_sends(key: gdk::Key, modifiers: gdk::ModifierType) -> bool {
    matches!(
        key,
        gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter
    ) && !modifiers.intersects(gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::ALT_MASK)
}

/// What a send asks: the box's text, trimmed, unless it is blank or a turn
/// is still running, when the text stays in the box.
fn question_to_send(text: &str, running: bool) -> Option<String> {
    let question = text.trim();
    (!running && !question.is_empty()).then(|| question.to_string())
}

/// The question box and the text view inside it. The box grows with the
/// question up to [`QUESTION_LINES`] and then scrolls. A text view has no
/// placeholder of its own, so a dim label sits over it while it is empty.
fn question_box() -> (gtk::Overlay, gtk::TextView) {
    let placeholder = gettext("Ask Penguin Mail…");
    let input = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .accepts_tab(false)
        .top_margin(3)
        .bottom_margin(3)
        .left_margin(4)
        .right_margin(4)
        .build();
    crate::ui::name(&input, &gettext("Ask Penguin Mail"));
    input.update_property(&[gtk::accessible::Property::Placeholder(&placeholder)]);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&input)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .hexpand(true)
        .build();
    // Until the question passes the limit the box takes its whole height,
    // with no scrollbar. A scrollbar would set the box's least height to
    // its own, about two lines. Both closures hold the scroller weakly:
    // it owns them, and a strong hold would keep every pane alive.
    let bar = scroller.downgrade();
    scroller.vadjustment().connect_changed(move |adjustment| {
        let Some(bar) = bar.upgrade() else { return };
        let limit = f64::from(bar.max_content_height());
        bar.set_vscrollbar_policy(match limit >= 0.0 && adjustment.upper() > limit {
            true => gtk::PolicyType::Automatic,
            false => gtk::PolicyType::Never,
        });
    });
    // The view's font is known once it is on screen, so the limit waits
    // for that.
    let limit = scroller.downgrade();
    input.connect_realize(move |view| {
        let Some(limit) = limit.upgrade() else { return };
        let line = view.pango_context().metrics(None, None).height() / pango::SCALE;
        let edges = view.top_margin() + view.bottom_margin();
        limit.set_max_content_height(line * QUESTION_LINES + edges);
    });
    let hint = gtk::Label::builder()
        .label(&placeholder)
        .xalign(0.0)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .margin_top(3)
        .margin_start(4)
        .margin_end(4)
        .ellipsize(pango::EllipsizeMode::End)
        .can_target(false)
        .css_classes(["dim-label"])
        .build();
    // The text view says the placeholder to a screen reader already.
    hint.update_state(&[gtk::accessible::State::Hidden(true)]);
    let field = gtk::Overlay::builder()
        .child(&scroller)
        .hexpand(true)
        .overflow(gtk::Overflow::Hidden)
        .css_classes(["assistant-input"])
        .build();
    field.add_overlay(&hint);
    input
        .buffer()
        .connect_changed(move |buffer| hint.set_visible(buffer.char_count() == 0));
    // A new line glides the view to the cursor while the box grows to fit
    // it, and the glide's last frames leave the scroll range at the old,
    // shorter height, with the first line cut off. Laying the view out
    // again sets the range from its real height.
    let view = input.downgrade();
    scroller
        .vadjustment()
        .connect_value_changed(move |adjustment| {
            if let Some(view) = view.upgrade()
                && adjustment.page_size() < f64::from(view.height())
            {
                view.queue_allocate();
            }
        });
    (field, input)
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
    work: Detail,
    details: gtk::Box,
    /// One per step, in the same order.
    rows: Vec<StepRow>,
    status: Status,
}

impl TurnView {
    fn draw(&mut self, updates: &[Update]) {
        for update in updates {
            match *update {
                Update::Added(index) => {
                    let row = StepRow::new(&self.turn.steps()[index], false);
                    if matches!(self.turn.steps()[index], Step::Reply(_)) {
                        self.steps.append(row.widget());
                    } else {
                        self.details.append(row.widget());
                        self.work.expander.set_visible(true);
                    }
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
        self.work
            .summary
            .set_label(&turn::work_summary(self.turn.steps()));
        self.work.set_mark(
            if self.turn.steps().iter().any(|step| {
                matches!(
                    step,
                    Step::Tool {
                        state: ToolState::Running,
                        ..
                    } | Step::Thinking { took: None, .. }
                )
            }) {
                Mark::Working
            } else {
                Mark::Done
            },
        );
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

/// The arrow a detail row shows: pointing at the title while folded, down
/// while open.
fn arrow_icon(open: bool) -> &'static str {
    if open {
        "penguin-mail-chevron-down-symbolic"
    } else {
        "penguin-mail-chevron-right-symbolic"
    }
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
            .width_request(14)
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
        // GtkExpander draws its own arrow before the whole label, which put
        // it left of the status icon. Its arrow is hidden in the style
        // sheet, and this one sits between the icon and the title.
        let arrow = gtk::Image::builder()
            .icon_name(arrow_icon(open))
            .pixel_size(10)
            .css_classes(["assistant-step-arrow"])
            .build();
        arrow.set_accessible_role(gtk::AccessibleRole::Presentation);
        let header = gtk::Box::builder().spacing(6).build();
        header.append(&mark);
        header.append(&arrow);
        header.append(&title);
        header.append(&summary);
        let expander = gtk::Expander::builder()
            .label_widget(&header)
            .expanded(open)
            .css_classes(["assistant-step"])
            .build();
        expander.connect_expanded_notify(move |expander| {
            arrow.set_icon_name(Some(arrow_icon(expander.is_expanded())));
        });
        Detail {
            expander,
            mark,
            title,
            summary,
        }
    }

    fn set_mark(&self, mark: Mark) {
        let state = match mark {
            Mark::Working => gettext("Running"),
            Mark::Thought | Mark::Done => gettext("Done"),
            Mark::Failed => gettext("Failed"),
        };
        let title = self.title.text();
        let summary = self.summary.text();
        let parts = [title.as_str(), summary.as_str(), state.as_str()];
        let said: Vec<_> = parts.into_iter().filter(|part| !part.is_empty()).collect();
        crate::ui::name(&self.expander, &said.join(", "));
        while let Some(child) = self.mark.first_child() {
            self.mark.remove(&child);
        }
        let icon = match mark {
            Mark::Working => {
                self.mark.append(
                    &adw::Spinner::builder()
                        .width_request(12)
                        .height_request(12)
                        .build(),
                );
                return;
            }
            Mark::Thought => "penguin-mail-sparkle-symbolic",
            Mark::Done => "object-select-symbolic",
            Mark::Failed => "dialog-warning-symbolic",
        };
        let image = gtk::Image::from_icon_name(icon);
        // Sized to the row's smaller text rather than the usual 16 px.
        image.set_pixel_size(12);
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
        identity: gtk::Label,
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
                let identity = detail_text(&["monospace", "dim-label"]);
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
                body.append(&identity);
                body.append(&heading(&gettext("Input")));
                body.append(&clipped(&input, 160));
                body.append(&result);
                detail.expander.set_child(Some(&body));
                StepRow::Tool {
                    detail,
                    input,
                    identity,
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
                    identity,
                    result,
                    output: output_label,
                },
                Step::Tool {
                    id,
                    name,
                    input,
                    state,
                    output,
                    ..
                },
            ) => {
                let label = tool_label(name);
                let summary = turn::work_summary(std::slice::from_ref(step));
                let summary = summary.strip_prefix(&format!("{label}: ")).unwrap_or("");
                identity.set_label(&format!("{name}\n{id}"));
                detail.title.set_label(&label);
                detail.summary.set_label(summary);
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
                let name: Vec<&str> = [label.as_str(), summary, said.as_str()]
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

#[cfg(test)]
mod tests {
    use gtk::gdk::{Key, ModifierType};

    use super::{enter_sends, question_to_send};

    #[test]
    fn enter_sends_the_question_and_shift_enter_starts_a_line() {
        let none = ModifierType::empty();
        assert!(enter_sends(Key::Return, none));
        assert!(enter_sends(Key::KP_Enter, none));
        assert!(enter_sends(Key::ISO_Enter, none));
        assert!(enter_sends(Key::Return, ModifierType::CONTROL_MASK));
        assert!(!enter_sends(Key::Return, ModifierType::SHIFT_MASK));
        assert!(!enter_sends(Key::KP_Enter, ModifierType::SHIFT_MASK));
    }

    #[test]
    fn enter_during_a_run_keeps_the_question_in_the_box() {
        assert_eq!(question_to_send("Archive them, (all) please", true), None);
        assert_eq!(
            question_to_send("  Archive them, (all) please\n", false).as_deref(),
            Some("Archive them, (all) please")
        );
        assert_eq!(question_to_send(" \n ", false), None);
    }

    #[test]
    fn other_keys_type_into_the_question() {
        let none = ModifierType::empty();
        for key in [
            Key::a,
            Key::comma,
            Key::parenleft,
            Key::parenright,
            Key::dead_acute,
            Key::space,
        ] {
            assert!(!enter_sends(key, none), "{key:?}");
        }
        assert!(!enter_sends(Key::parenleft, ModifierType::SHIFT_MASK));
    }
}

#[cfg(test)]
pub(crate) mod checks {
    use super::*;
    pub fn run() {
        let steps = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let work = Detail::new(true);
        let details = gtk::Box::new(gtk::Orientation::Vertical, 0);
        work.expander.set_child(Some(&details));
        steps.append(&work.expander);
        let mut view = TurnView {
            turn: Turn::new(Instant::now()),
            steps,
            work,
            details,
            rows: Vec::new(),
            status: Status::new(),
        };
        for event in [
            AgentEvent::Thinking("Look at the mailbox".into()),
            AgentEvent::ToolStarted {
                id: "call-42".into(),
                name: "list_mail".into(),
                input: serde_json::json!({"mailbox": "follow_up"}),
            },
            AgentEvent::ToolFinished {
                id: "call-42".into(),
                name: "list_mail".into(),
                ok: true,
                preview: String::new(),
                output: "One message".into(),
            },
            AgentEvent::Text("One message needs a reply.".into()),
        ] {
            let changes = view.turn.apply(event, Instant::now());
            view.draw(&changes);
            view.show_phase();
        }
        assert_eq!(
            view.steps.first_child(),
            Some(view.work.expander.clone().upcast())
        );
        assert_eq!(
            view.details.first_child(),
            Some(view.rows[0].widget().clone())
        );
        assert_eq!(view.steps.last_child(), Some(view.rows[2].widget().clone()));
        assert!(view.work.expander.is_expanded());
        assert!(view.work.summary.text().contains("Follow Up"));
        if let StepRow::Tool {
            detail, identity, ..
        } = &view.rows[1]
        {
            assert!(!detail.expander.is_expanded());
            assert!(identity.text().contains("call-42"));
            assert!(!detail.summary.text().contains("call-42"));
        } else {
            panic!("a nested tool row");
        }
        let detail = Detail::new(false);
        detail.title.set_label("What it did");
        for mark in [Mark::Working, Mark::Done, Mark::Failed] {
            detail.set_mark(mark);
            assert!(gtk::test_accessible_has_property(
                &detail.expander,
                gtk::AccessibleProperty::Label
            ));
        }
        files::checks::run();
        assert_eq!(QUESTION_LINES, 4);
    }
}
