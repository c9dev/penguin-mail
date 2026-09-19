//! The automatic reply dialog: what Gmail answers new mail with for one
//! account, and the days it runs between. `mailrs_sync::AccountSettings`
//! reads and stores it.

use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::Account;
use mailrs_sync::{AutomaticReply, Permitted};

use crate::core::Core;

/// Shows the dialog for `account`. `grant` runs when Gmail says Penguin Mail lacks
/// the settings permission, to send the user through consent again. `saved`
/// receives a confirmation to show once the reply is stored.
pub fn present(
    core: &Rc<Core>,
    account: &Account,
    parent: &impl IsA<gtk::Widget>,
    grant: impl Fn() + 'static,
    saved: impl Fn(&str) + 'static,
) {
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    stack.add_named(
        &adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build(),
        Some("loading"),
    );
    let save = gtk::Button::builder()
        .label("Save")
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&save);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toasts));
    let dialog = adw::Dialog::builder()
        .title("Automatic Reply")
        .content_width(480)
        .content_height(640)
        .child(&toolbar)
        .build();
    let closer = dialog.clone();
    cancel.connect_clicked(move |_| {
        closer.close();
    });
    dialog.present(Some(parent));

    if core.account(account.id).is_none() {
        stack.add_named(&problem("This account is not syncing yet."), Some("error"));
        stack.set_visible_child_name("error");
        return;
    }
    let (core, email, account_id) = (Rc::clone(core), account.email.clone(), account.id);
    let settings = core.gmail_settings();
    glib::spawn_future_local(async move {
        let loaded = {
            let settings = Arc::clone(&settings);
            core.call(async move { settings.automatic_reply(account_id).await })
                .await
        };
        let reply = match loaded {
            Ok(Permitted::Done(reply)) => reply,
            Ok(Permitted::NeedsPermission) => {
                let page = adw::StatusPage::builder()
                    .icon_name("mail-send-symbolic")
                    .title("Allow Automatic Replies")
                    .description(format!(
                        "Penguin Mail needs permission to change Gmail settings for {email}. \
                         Google will ask you to confirm in your browser."
                    ))
                    .build();
                let button = gtk::Button::builder()
                    .label("Grant Access")
                    .halign(gtk::Align::Center)
                    .css_classes(["pill", "suggested-action"])
                    .build();
                let closer = dialog.clone();
                button.connect_clicked(move |_| {
                    closer.close();
                    grant();
                });
                page.set_child(Some(&button));
                stack.add_named(&page, Some("error"));
                stack.set_visible_child_name("error");
                return;
            }
            Err(err) => {
                stack.add_named(&problem(&err.to_string()), Some("error"));
                stack.set_visible_child_name("error");
                return;
            }
        };
        let form = Form::new(&reply);
        stack.add_named(&form.page, Some("form"));
        stack.set_visible_child_name("form");
        save.set_sensitive(true);
        let saved = Rc::new(saved);
        save.connect_clicked(move |button| {
            let saved = Rc::clone(&saved);
            let wanted = form.reply(&reply);
            button.set_sensitive(false);
            let (core, settings, dialog, toasts, button) = (
                Rc::clone(&core),
                Arc::clone(&settings),
                dialog.clone(),
                toasts.clone(),
                button.clone(),
            );
            glib::spawn_future_local(async move {
                let enabled = wanted.enabled;
                let stored = core
                    .call(async move { settings.set_automatic_reply(account_id, &wanted).await })
                    .await;
                match stored {
                    Ok(Permitted::Done(())) => {
                        dialog.close();
                        saved(if enabled {
                            "Automatic reply is on"
                        } else {
                            "Automatic reply is off"
                        });
                    }
                    Ok(Permitted::NeedsPermission) => {
                        button.set_sensitive(true);
                        toasts.add_toast(adw::Toast::new(
                            "Penguin Mail needs permission to change Gmail settings",
                        ));
                    }
                    Err(err) => {
                        button.set_sensitive(true);
                        toasts.add_toast(adw::Toast::new(&format!("Could not save: {err}")));
                    }
                }
            });
        });
    });
}

fn problem(message: &str) -> adw::StatusPage {
    adw::StatusPage::builder()
        .icon_name("dialog-warning-symbolic")
        .title("Could Not Load the Automatic Reply")
        .description(glib::markup_escape_text(message).as_str())
        .build()
}

struct Form {
    page: adw::PreferencesPage,
    enabled: adw::SwitchRow,
    dated: adw::SwitchRow,
    first: DateButton,
    last: DateButton,
    subject: adw::EntryRow,
    body: gtk::TextView,
    contacts_only: adw::SwitchRow,
}

impl Form {
    fn new(reply: &AutomaticReply) -> Rc<Form> {
        let page = adw::PreferencesPage::new();

        let enabled = adw::SwitchRow::builder()
            .title("Send Automatic Replies")
            .subtitle("Gmail answers new mail while you are away, even when this computer is off")
            .active(reply.enabled)
            .build();
        let top = adw::PreferencesGroup::new();
        top.add(&enabled);
        page.add(&top);

        let today = glib::DateTime::now_local().expect("the clock reads");
        let first_day = reply.first_day.map_or_else(|| today.clone(), local_day);
        let last_day = reply.last_day.map_or_else(
            || today.add_days(6).expect("a week from now exists"),
            local_day,
        );
        let dated = adw::SwitchRow::builder()
            .title("Only Between These Dates")
            .active(reply.first_day.is_some() || reply.last_day.is_some())
            .build();
        let first = DateButton::new("First Day", &first_day);
        let last = DateButton::new("Last Day", &last_day);
        for row in [&first.row, &last.row] {
            dated
                .bind_property("active", row, "sensitive")
                .sync_create()
                .build();
        }
        let dates = adw::PreferencesGroup::builder().title("Dates").build();
        dates.add(&dated);
        dates.add(&first.row);
        dates.add(&last.row);
        page.add(&dates);

        let subject = adw::EntryRow::builder().title("Subject").build();
        subject.set_text(&reply.subject);
        let body = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(12)
            .bottom_margin(12)
            .left_margin(12)
            .right_margin(12)
            .accepts_tab(false)
            .build();
        body.buffer().set_text(&reply.body);
        let frame = gtk::ScrolledWindow::builder()
            .child(&body)
            .min_content_height(160)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .css_classes(["card"])
            .margin_top(12)
            .build();
        let message = adw::PreferencesGroup::builder().title("Message").build();
        message.add(&subject);
        message.add(&frame);
        page.add(&message);

        let contacts_only = adw::SwitchRow::builder()
            .title("Only Reply to My Contacts")
            .active(reply.contacts_only)
            .build();
        let who = adw::PreferencesGroup::new();
        who.add(&contacts_only);
        page.add(&who);

        for widget in [
            dates.upcast_ref::<gtk::Widget>(),
            message.upcast_ref(),
            who.upcast_ref(),
        ] {
            enabled
                .bind_property("active", widget, "sensitive")
                .sync_create()
                .build();
        }
        Rc::new(Form {
            page,
            enabled,
            dated,
            first,
            last,
            subject,
            body,
            contacts_only,
        })
    }

    /// `base` with the form's values. Keeps settings the form does not show.
    fn reply(&self, base: &AutomaticReply) -> AutomaticReply {
        let buffer = self.body.buffer();
        let dated = self.dated.is_active();
        let first = midnight(&self.first.date());
        let last = midnight(&self.last.date()).max(first);
        AutomaticReply {
            enabled: self.enabled.is_active(),
            subject: self.subject.text().trim().to_string(),
            body: buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .trim_end()
                .to_string(),
            contacts_only: self.contacts_only.is_active(),
            first_day: dated.then_some(first),
            last_day: dated.then_some(last),
            ..base.clone()
        }
    }
}

/// A row whose button opens a calendar.
struct DateButton {
    row: adw::ActionRow,
    calendar: gtk::Calendar,
}

impl DateButton {
    fn new(title: &str, day: &glib::DateTime) -> DateButton {
        let calendar = gtk::Calendar::new();
        calendar.set_date(day);
        let button = gtk::MenuButton::builder()
            .label(day_label(day))
            .valign(gtk::Align::Center)
            .popover(&gtk::Popover::builder().child(&calendar).build())
            .build();
        let shown = button.clone();
        calendar.connect_day_selected(move |calendar| {
            shown.set_label(&day_label(&calendar.date()));
            if let Some(popover) = shown.popover() {
                popover.popdown();
            }
        });
        let row = adw::ActionRow::builder().title(title).build();
        row.add_suffix(&button);
        row.set_activatable_widget(Some(&button));
        DateButton { row, calendar }
    }

    fn date(&self) -> glib::DateTime {
        self.calendar.date()
    }
}

fn day_label(day: &glib::DateTime) -> String {
    day.format("%a, %-d %b %Y")
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn local_day(millis: i64) -> glib::DateTime {
    glib::DateTime::from_unix_local(millis / 1000)
        .or_else(|_| glib::DateTime::now_local())
        .expect("the clock reads")
}

/// Local midnight at the start of `day`, in epoch milliseconds.
fn midnight(day: &glib::DateTime) -> i64 {
    let (year, month, date) = day.ymd();
    glib::DateTime::from_local(year, month, date, 0, 0, 0.0)
        .map(|d| d.to_unix() * 1000)
        .unwrap_or_default()
}
