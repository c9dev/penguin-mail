//! The translation card: what language a message arrived in, where its
//! words would go to be translated, and the button that sends them.
//!
//! It takes the event card's shape and sits directly above the message,
//! under the protection card. Nothing is sent until the button is
//! pressed, so the card says beforehand which model reads the message and
//! whether that model is on this computer.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;

use crate::translation::Language;
use mailrs_domain::translate::{fill, gettext};

pub struct TranslationCard {
    pub widget: adw::BreakpointBin,
    title: gtk::Label,
    detail: gtk::Label,
    button: gtk::Button,
    /// Never Translate, for the language on the card.
    never: gtk::Button,
    /// The language the card offers to translate from, while it names one.
    from: Cell<Option<&'static str>>,
}

impl TranslationCard {
    /// `on_press` is the button: it translates the message, or turns the
    /// translation over once there is one. The window knows which.
    /// `on_never` hears the code of a language the person never wants
    /// translated.
    pub fn new(
        on_press: impl Fn() + 'static,
        on_never: impl Fn(&'static str) + 'static,
    ) -> Rc<TranslationCard> {
        let icon = gtk::Image::builder()
            .icon_name("preferences-desktop-locale-symbolic")
            .valign(gtk::Align::Start)
            .build();
        let line = |classes: &[&str]| {
            gtk::Label::builder()
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .css_classes(classes.to_vec())
                .build()
        };
        let title = line(&["heading"]);
        let detail = line(&["dim-label", "caption"]);
        let lines = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .build();
        lines.append(&title);
        lines.append(&detail);

        let button = gtk::Button::builder()
            .label(gettext("Translate"))
            .valign(gtk::Align::Center)
            .build();
        button.connect_clicked(move |_| on_press());
        let never = gtk::Button::builder()
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .visible(false)
            .build();

        let inside = gtk::Box::builder()
            .spacing(12)
            .css_classes(["card", "translation-card"])
            .build();
        inside.append(&icon);
        inside.append(&lines);
        inside.append(&never);
        inside.append(&button);

        let widget = super::page_column(&inside, "translation-area");

        let card = Rc::new(TranslationCard {
            widget,
            title,
            detail,
            button,
            never,
            from: Cell::new(None),
        });
        let weak = Rc::downgrade(&card);
        card.never.connect_clicked(move |_| {
            if let Some(code) = weak.upgrade().and_then(|card| card.from.get()) {
                on_never(code);
            }
        });
        card
    }

    /// Offers Never Translate for `from`, or takes it away.
    fn offer_never(&self, from: Option<Language>) {
        self.from.set(from.map(|language| language.code));
        if let Some(language) = from {
            self.never.set_label(&fill(
                &gettext("Never Translate {language}"),
                &[("language", &language.name())],
            ));
        }
        self.never.set_visible(from.is_some());
    }

    /// Offers to translate a message in `from`. `goes` says where its
    /// words would go, or, as an `Err`, why there is nowhere to send
    /// them. Nothing has been sent at this point.
    pub fn offer(&self, from: Option<Language>, goes: Result<&str, &str>) {
        self.title.set_text(&match from {
            Some(language) => fill(
                &gettext("This message is in {language}"),
                &[("language", &language.name())],
            ),
            None => gettext("This message is in another language"),
        });
        self.say(goes.unwrap_or_else(|problem| problem), goes.is_err());
        self.offer_never(from);
        self.button.set_label(&gettext("Translate"));
        self.button.set_sensitive(true);
        self.widget.set_visible(true);
    }

    /// While the model is reading the message.
    pub fn working(&self) {
        self.say(&gettext("Translating…"), false);
        self.offer_never(None);
        self.button.set_sensitive(false);
    }

    /// The translation is here. `shown` says which of the two the message
    /// below is showing, and `cut` that the message was too long for one
    /// request.
    pub fn done(&self, from: Option<Language>, cut: bool, shown: bool) {
        self.title.set_text(&match (shown, from) {
            (false, _) => gettext("Showing the message as it arrived"),
            (true, Some(language)) => fill(
                &gettext("Translated from {language}"),
                &[("language", &language.name())],
            ),
            (true, None) => gettext("Translated"),
        });
        let note = match cut && shown {
            true => gettext("The message was long, so only the start of it was translated."),
            false => String::new(),
        };
        self.say(&note, false);
        self.offer_never(None);
        self.button.set_label(&match shown {
            true => gettext("Show Original"),
            false => gettext("Show Translation"),
        });
        self.button.set_sensitive(true);
        self.widget.set_visible(true);
    }

    /// Nothing was sent, and why. The button stays, so the reader can try
    /// again once Preferences has a model in it.
    pub fn problem(&self, reason: &str) {
        self.say(reason, true);
        self.button.set_label(&gettext("Translate"));
        self.button.set_sensitive(true);
    }

    pub fn hide(&self) {
        self.widget.set_visible(false);
    }

    /// The line under the title. An empty one takes its own room back.
    fn say(&self, text: &str, problem: bool) {
        self.detail.set_text(text);
        self.detail.set_visible(!text.is_empty());
        match problem {
            true => self.detail.add_css_class("error"),
            false => self.detail.remove_css_class("error"),
        }
    }
}
