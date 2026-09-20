//! The Templates list in Preferences, and the editor behind Add and Edit.
//!
//! Templates belong to the person rather than to one account, so the list
//! is the same whichever addresses are signed in.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_store::templates::Template;

use crate::app::App;
use mailrs_domain::translate::{fill, gettext};

/// The rows on show, so a change can take them off again.
type Rows = Rc<RefCell<Vec<adw::ActionRow>>>;

/// One row per saved template, with a way to add, edit, and delete them.
pub fn group(app: &Rc<App>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Templates"))
        .description(gettext(
            "Saved bodies the composer's Templates menu drops into a message. \
             Markdown works, and {{first_name}}, {{name}}, {{email}}, {{subject}} \
             and {{date}} fill in when you use one.",
        ))
        .build();
    let add = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name("list-add-symbolic")
                .label(gettext("Add Template"))
                .build(),
        )
        .css_classes(["flat"])
        .build();
    group.set_header_suffix(Some(&add));
    let rows: Rows = Rc::new(RefCell::new(Vec::new()));
    let (weak, list, shown) = (Rc::downgrade(app), group.clone(), Rc::clone(&rows));
    add.connect_clicked(move |button| {
        if let Some(app) = weak.upgrade() {
            edit(&app, button, None, &list, &shown);
        }
    });
    refresh(app, &group, &rows);
    group
}

/// Reads the templates in again and lays the rows out afresh.
fn refresh(app: &Rc<App>, group: &adw::PreferencesGroup, rows: &Rows) {
    let (this, group, rows) = (Rc::clone(app), group.clone(), Rc::clone(rows));
    glib::spawn_future_local(async move {
        let saved = match this.core.read(mailrs_store::templates::list).await {
            Ok(saved) => saved,
            Err(err) => return tracing::warn!(error = %err, "could not read the templates"),
        };
        for row in rows.borrow_mut().drain(..) {
            group.remove(&row);
        }
        for template in saved {
            let row = row(&this, &group, &rows, template);
            group.add(&row);
            rows.borrow_mut().push(row);
        }
    });
}

fn row(
    app: &Rc<App>,
    group: &adw::PreferencesGroup,
    rows: &Rows,
    template: Template,
) -> adw::ActionRow {
    let name = template.name.clone();
    let row = adw::ActionRow::builder()
        .title(&template.name)
        .subtitle(summary(&template))
        .build();
    let button = |icon: &str, tip: String, spoken: String| {
        let button = gtk::Button::builder()
            .icon_name(icon)
            .tooltip_text(tip)
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        crate::ui::name(&button, &spoken);
        button
    };
    let open = button(
        "document-edit-symbolic",
        gettext("Edit Template"),
        fill(&gettext("Edit {template}"), &[("template", &name)]),
    );
    let (weak, saved, list, shown) = (
        Rc::downgrade(app),
        template.clone(),
        group.clone(),
        Rc::clone(rows),
    );
    open.connect_clicked(move |button| {
        if let Some(app) = weak.upgrade() {
            edit(&app, button, Some(saved.clone()), &list, &shown);
        }
    });
    let delete = button(
        "user-trash-symbolic",
        gettext("Delete Template"),
        fill(&gettext("Delete {template}"), &[("template", &name)]),
    );
    let (weak, saved, list, shown) = (Rc::downgrade(app), template, group.clone(), Rc::clone(rows));
    delete.connect_clicked(move |button| {
        if let Some(app) = weak.upgrade() {
            confirm_delete(&app, button, saved.clone(), &list, &shown);
        }
    });
    row.set_activatable_widget(Some(&open));
    row.add_suffix(&open);
    row.add_suffix(&delete);
    row
}

/// What the row says under the name: the subject, or the first line of the
/// body when the template leaves the subject alone.
fn summary(template: &Template) -> String {
    let subject = template.subject.trim();
    if !subject.is_empty() {
        return subject.to_string();
    }
    template
        .markdown
        .lines()
        .find(|line| !line.trim().is_empty())
        .map_or_else(|| gettext("Empty"), |line| line.trim().to_string())
}

/// Asks before deleting, since nothing here brings a template back.
fn confirm_delete(
    app: &Rc<App>,
    parent: &impl IsA<gtk::Widget>,
    template: Template,
    group: &adw::PreferencesGroup,
    rows: &Rows,
) {
    let dialog = adw::AlertDialog::new(
        Some(&fill(
            &gettext("Delete {name}?"),
            &[("name", &template.name)],
        )),
        Some(&gettext("This computer keeps the only copy.")),
    );
    dialog.add_responses(&[
        ("cancel", &gettext("Cancel")),
        ("delete", &gettext("Delete")),
    ]);
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_close_response("cancel");
    let (this, parent, group, rows) = (
        Rc::clone(app),
        parent.as_ref().clone(),
        group.clone(),
        Rc::clone(rows),
    );
    glib::spawn_future_local(async move {
        if dialog.choose_future(Some(&parent)).await != "delete" {
            return;
        }
        let id = template.id;
        match this
            .core
            .write(move |c| mailrs_store::templates::remove(c, id))
            .await
        {
            Ok(()) => refresh(&this, &group, &rows),
            Err(err) => tracing::warn!(error = %err, "could not delete the template"),
        }
    });
}

/// The editor: a new template with no `existing`, otherwise that one, name
/// and all, which is how a template is renamed.
fn edit(
    app: &Rc<App>,
    parent: &impl IsA<gtk::Widget>,
    existing: Option<Template>,
    group: &adw::PreferencesGroup,
    rows: &Rows,
) {
    let editing = existing.is_some();
    let template = existing.unwrap_or(Template {
        id: 0,
        name: String::new(),
        subject: String::new(),
        markdown: String::new(),
    });
    let name = adw::EntryRow::builder().title(gettext("Name")).build();
    name.set_text(&template.name);
    let subject = adw::EntryRow::builder().title(gettext("Subject")).build();
    subject.set_text(&template.subject);
    let about = adw::PreferencesGroup::builder()
        .description(gettext(
            "The subject is optional. A template with one gives it to a message that \
             has none.",
        ))
        .build();
    about.add(&name);
    about.add(&subject);

    let view = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(10)
        .bottom_margin(10)
        .left_margin(12)
        .right_margin(12)
        .accepts_tab(false)
        .build();
    view.buffer().set_text(&template.markdown);
    crate::ui::name(&view, &gettext("Body"));
    let body = adw::PreferencesGroup::builder()
        .title(gettext("Body"))
        .build();
    body.add(
        &gtk::ScrolledWindow::builder()
            .child(&view)
            .min_content_height(220)
            .propagate_natural_height(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .css_classes(["card"])
            .build(),
    );

    let page = adw::PreferencesPage::new();
    page.add(&about);
    page.add(&body);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&page));
    let save = gtk::Button::builder()
        .label(if editing {
            gettext("Save")
        } else {
            gettext("Create")
        })
        .css_classes(["suggested-action"])
        .build();
    let cancel = gtk::Button::with_label(&gettext("Cancel"));
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&save);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toasts));
    let dialog = adw::Dialog::builder()
        .title(if editing {
            gettext("Edit Template")
        } else {
            gettext("New Template")
        })
        .content_width(560)
        .content_height(520)
        .child(&toolbar)
        .build();
    let closer = dialog.clone();
    cancel.connect_clicked(move |_| {
        closer.close();
    });
    let (this, closer, group, rows) = (
        Rc::clone(app),
        dialog.clone(),
        group.clone(),
        Rc::clone(rows),
    );
    save.connect_clicked(move |_| {
        let buffer = view.buffer();
        let written = Template {
            id: template.id,
            name: name.text().trim().to_string(),
            subject: subject.text().trim().to_string(),
            markdown: buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string(),
        };
        if written.name.is_empty() {
            toasts.add_toast(adw::Toast::new(&gettext("Give the template a name")));
            return;
        }
        closer.close();
        let (this, group, rows) = (Rc::clone(&this), group.clone(), Rc::clone(&rows));
        glib::spawn_future_local(async move {
            let saved = this
                .core
                .write(move |c| match editing {
                    true => mailrs_store::templates::update(c, &written),
                    false => mailrs_store::templates::add(c, &written).map(|_| ()),
                })
                .await;
            match saved {
                Ok(()) => refresh(&this, &group, &rows),
                Err(err) => tracing::warn!(error = %err, "could not save the template"),
            }
        });
    });
    dialog.present(Some(parent));
}
