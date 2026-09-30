//! Moving mail by drag and drop, and creating, renaming, and deleting labels.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::{AccountId, Label, LabelKind};
use mailrs_sync::{History, MailAction, NewLabels};

use super::{MainWindow, Target};
use crate::offered::Filing;
use crate::ui::Mailbox;
use crate::ui::sidebar::LabelDrop;
use crate::ui::sidebar::tree::{self, Placement, Refusal, label_rows};
use crate::ui::confirm::{Tone, confirm};
use super::press::{Press, Scope};
use super::reach::Reach;
use mailrs_domain::translate::{fill, gettext};

/// What to do with a label once it exists, given it.
pub(super) type AfterCreate = Box<dyn Fn(&Rc<MainWindow>, Label)>;

impl MainWindow {
    /// Moves the dragged rows into `mailbox`. True when the drop was taken.
    pub(super) fn drop_on(self: &Rc<Self>, mailbox: Mailbox) -> bool {
        let rows = self.list.dragged();
        if rows.is_empty() {
            return false;
        }
        let targets: Vec<Target> = rows.iter().map(Target::from_row).collect();
        let open = self.conversation.read(|o| o.among(&targets)) == Some(true);
        let reach = Reach {
            targets,
            marks: Default::default(),
            muted: false,
            mailbox: self.shown(),
        };
        let view = Rc::clone(&self.conversation);
        self.press_on(&view, reach, Scope::Carried { open }, Press::Drop(mailbox))
    }

    /// Asks for a name and creates a label in `account_id`, or a folder on
    /// an account that files in folders. With `then`, runs it on the new
    /// label, as the label menu does to apply it.
    pub(super) fn new_label(self: &Rc<Self>, account_id: AccountId, then: Option<AfterCreate>) {
        self.new_label_from(account_id, then, "");
    }

    /// Asks for a new label's name with "Parent/" typed already, so the
    /// new one nests under `parent_id`.
    pub(super) fn new_label_inside(self: &Rc<Self>, account_id: AccountId, parent_id: String) {
        if let Some(parent) = self.label_name(account_id, &parent_id) {
            self.new_label_from(account_id, None, &format!("{parent}/"));
        }
    }

    /// The New Label dialog with `start` in its entry and the cursor
    /// after it.
    fn new_label_from(self: &Rc<Self>, account_id: AccountId, then: Option<AfterCreate>, start: &str) {
        let filing = Filing::of([self.offers(account_id)]);
        let entry = gtk::Entry::builder()
            .text(start)
            .placeholder_text(gettext("Name, or Parent/Name to nest it"))
            .activates_default(true)
            .build();
        focus_at_end(&entry);
        let dialog = adw::AlertDialog::builder()
            .heading(filing.new_heading())
            .extra_child(&entry)
            .build();
        dialog.add_responses(&[
            ("cancel", &gettext("Cancel")),
            ("create", &gettext("Create")),
        ]);
        dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("create"));
        dialog.set_close_response("cancel");
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&this.window)).await != "create" {
                return;
            }
            let name = entry.text().trim().to_string();
            if name.is_empty() {
                return;
            }
            let Some(sync) = this.core.account(account_id) else {
                return this.toast(&gettext("That account is not connected"));
            };
            let wanted = name.clone();
            match this
                .core
                .call(async move { sync.create_label(&wanted).await })
                .await
            {
                Ok(label) => match then {
                    Some(then) => then(&this, label),
                    None => this.toast(&fill(&gettext("Created “{name}”"), &[("name", &name)])),
                },
                Err(err) => this.failed(&filing.create_failed(), &err),
            }
        });
    }

    pub(super) fn rename_label(self: &Rc<Self>, account_id: AccountId, label_id: String) {
        let Some(current) = self.label_name(account_id, &label_id) else {
            return;
        };
        let filing = Filing::of([self.offers(account_id)]);
        let entry = gtk::Entry::builder()
            .text(&current)
            .activates_default(true)
            .build();
        focus_when_shown(&entry);
        let dialog = adw::AlertDialog::builder()
            .heading(filing.rename_heading())
            .body(filing.rename_body())
            .extra_child(&entry)
            .build();
        dialog.add_responses(&[
            ("cancel", &gettext("Cancel")),
            ("rename", &gettext("Rename")),
        ]);
        dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("rename"));
        dialog.set_close_response("cancel");
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&this.window)).await != "rename" {
                return;
            }
            let name = entry.text().trim().to_string();
            if name.is_empty() || name == current {
                return;
            }
            let Some(sync) = this.core.account(account_id) else {
                return this.toast(&gettext("That account is not connected"));
            };
            let wanted = name.clone();
            if let Err(err) = this
                .core
                .call(async move { sync.rename_label(&label_id, &wanted).await })
                .await
            {
                this.failed(&filing.rename_failed(), &err);
            }
        });
    }

    /// Carries out a label dropped on another row of its account: moves
    /// it among its siblings, or renames it into or out of a parent, or
    /// says why it stays.
    pub(super) fn drop_label(self: &Rc<Self>, drop: LabelDrop) {
        let labels = self.labels_of(drop.account_id);
        let order = self.label_order(drop.account_id);
        let rows = label_rows(&labels, &order);
        let filing = Filing::of([self.offers(drop.account_id)]);
        match tree::place(&rows, &drop.dragged, &drop.target, drop.zone) {
            Ok(Some(placement)) => self.place_label(drop.account_id, drop.dragged, placement),
            Ok(None) => {}
            Err(Refusal::OwnSubtree) => self.toast(&filing.inside_itself()),
            Err(Refusal::Taken(name)) => self.toast(&filing.name_taken(&name)),
        }
    }

    /// Moves a label one place up (`-1`) or down (`1`) among its
    /// siblings, from its menu.
    pub(super) fn move_label(self: &Rc<Self>, account_id: AccountId, label_id: String, by: i32) {
        let labels = self.labels_of(account_id);
        let order = self.label_order(account_id);
        let rows = label_rows(&labels, &order);
        if let Some(placement) = tree::step(&rows, &label_id, by) {
            self.place_label(account_id, label_id, placement);
        }
    }

    fn place_label(self: &Rc<Self>, account_id: AccountId, label_id: String, placement: Placement) {
        let Some(sync) = self.core.account(account_id) else {
            return self.toast(&gettext("That account is not connected"));
        };
        let filing = Filing::of([self.offers(account_id)]);
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let Placement { name, order } = placement;
            if let Err(err) = this
                .core
                .call(async move { sync.place_label(&label_id, &name, &order).await })
                .await
            {
                this.failed(&filing.rename_failed(), &err);
            }
        });
    }

    pub(super) fn delete_label(self: &Rc<Self>, account_id: AccountId, label_id: String) {
        let Some(name) = self.label_name(account_id, &label_id) else {
            return;
        };
        let filing = Filing::of([self.offers(account_id)]);
        let question = confirm(
            &fill(&gettext("Delete “{name}”?"), &[("name", &name)]),
            &filing.delete_body(),
            &gettext("Delete"),
            Tone::Destructive,
        );
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if !question.ask(&this.window).await {
                return;
            }
            let Some(sync) = this.core.account(account_id) else {
                return this.toast(&gettext("That account is not connected"));
            };
            let deleted = label_id.clone();
            match this
                .core
                .call(async move { sync.delete_label(&deleted).await })
                .await
            {
                Ok(()) => {
                    this.change_screen(|screen| screen.label_deleted(account_id, &label_id));
                    this.toast(&fill(&gettext("Deleted “{name}”"), &[("name", &name)]));
                }
                Err(err) => this.failed(&filing.delete_failed(), &err),
            }
        });
    }

    /// Gives a label colour `index` of Gmail's palette.
    pub(super) fn color_label(
        self: &Rc<Self>,
        account_id: AccountId,
        label_id: String,
        index: usize,
    ) {
        let Some((background, text)) = crate::ui::LABEL_COLORS.get(index) else {
            return;
        };
        let Some(sync) = self.core.account(account_id) else {
            return self.toast(&gettext("That account is not connected"));
        };
        let color = mailrs_gmail::LabelColor {
            background_color: background.to_string(),
            text_color: text.to_string(),
        };
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if let Err(err) = this
                .core
                .call(async move { sync.set_label_color(&label_id, color).await })
                .await
            {
                this.failed(&gettext("Could not change the color: {reason}"), &err);
            }
        });
    }

    /// The label menu for mail from several accounts: every user label
    /// name any of them holds, once each. `None` when none holds a label.
    pub(super) fn labels_by_name(
        self: &Rc<Self>,
        accounts: &HashSet<AccountId>,
        popover: &gtk::Popover,
    ) -> Option<gtk::Widget> {
        let mut names: Vec<String> = Vec::new();
        for account_id in accounts {
            for label in self.labels_of(*account_id) {
                if label.kind == LabelKind::User
                    && !names.iter().any(|n| n.eq_ignore_ascii_case(&label.name))
                {
                    names.push(label.name);
                }
            }
        }
        if names.is_empty() {
            return None;
        }
        names.sort_by_key(|n| n.to_lowercase());
        let list = gtk::ListBox::builder()
            .css_classes(["navigation-sidebar"])
            .selection_mode(gtk::SelectionMode::None)
            .build();
        for name in &names {
            let shown = gtk::Label::builder()
                .label(name.replace('/', " › "))
                .xalign(0.0)
                .build();
            let row = gtk::ListBoxRow::builder()
                .child(&shown)
                .activatable(true)
                .build();
            crate::ui::name(&row, &super::label_row_name(name, false));
            list.append(&row);
        }
        let (weak, pop) = (Rc::downgrade(self), popover.clone());
        list.connect_row_activated(move |_, row| {
            let (Some(win), Some(name)) = (weak.upgrade(), names.get(row.index() as usize)) else {
                return;
            };
            pop.popdown();
            win.label_by_name(name.clone());
        });
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(360)
            .min_content_width(220)
            .build();
        Some(scroller.upcast())
    }

    /// Adds the label called `name` to the targets. An account without a
    /// label by that name gets one only if the person says so; otherwise
    /// only the mail in accounts that hold the name gets it.
    fn label_by_name(self: &Rc<Self>, name: String) {
        let reach = self.reach(&self.conversation);
        // On a folder account the label is a folder the mail moves into,
        // out of the list it was picked from.
        let (targets, from) = (reach.targets, reach.mailbox.moved_from());
        if targets.is_empty() {
            return;
        }
        let add = vec![name];
        let plan = NewLabels::plan(&targets, &add, &[], |account_id| {
            self.labels_of(account_id)
                .into_iter()
                .map(|l| l.name)
                .collect()
        });
        let label = move |create: bool| MailAction::Label {
            add: add.clone(),
            remove: vec![],
            create,
        };
        if plan.is_empty() {
            return self.perform_from(targets, label(true), History::Record, None, from);
        }
        let kept = plan.kept(&targets);
        let emails: HashMap<AccountId, String> = self
            .accounts()
            .into_iter()
            .map(|a| (a.id, a.email))
            .collect();
        let who = plan.who(|id| emails.get(&id).cloned().unwrap_or_default());
        let mut question = confirm(&plan.heading(), &who, &gettext("Create"), Tone::Suggested);
        if !kept.is_empty() {
            question = question.declining(&gettext("Only Where It Exists"));
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let create = question.ask(&this.window).await;
            let targets = if create { targets } else { kept };
            let action = label(create);
            this.perform_from(targets, action, History::Record, None, from);
        });
    }

    fn label_name(&self, account_id: AccountId, label_id: &str) -> Option<String> {
        self.labels_of(account_id)
            .into_iter()
            .find(|l| l.id == label_id && l.kind == LabelKind::User)
            .map(|l| l.name)
    }
}

/// Puts the cursor in `entry` once its dialog is on screen; dialogs
/// otherwise focus their default button.
/// Focuses `entry` once it shows, with the cursor after its text rather
/// than the text selected, so typing adds to it.
fn focus_at_end(entry: &gtk::Entry) {
    entry.connect_map(|entry| {
        let entry = entry.clone();
        glib::idle_add_local_once(move || {
            entry.grab_focus_without_selecting();
            entry.set_position(-1);
        });
    });
}

fn focus_when_shown(entry: &gtk::Entry) {
    entry.connect_map(|entry| {
        let entry = entry.clone();
        glib::idle_add_local_once(move || {
            entry.grab_focus();
        });
    });
}
