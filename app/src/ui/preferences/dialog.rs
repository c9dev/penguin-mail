//! Preferences navigation and search, including rows inside account expanders.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::translate::gettext;

pub(crate) struct Dialog {
    pub window: adw::Dialog,
    pub toasts: adw::ToastOverlay,
    split: adw::NavigationSplitView,
    stack: gtk::Stack,
    list: gtk::ListBox,
    pages: RefCell<Vec<adw::PreferencesPage>>,
    title: adw::WindowTitle,
    search: gtk::SearchEntry,
    searching: gtk::ToggleButton,
    results: adw::PreferencesGroup,
    result_stack: gtk::Stack,
}

impl Dialog {
    pub fn new() -> Rc<Self> {
        let window = adw::Dialog::builder()
            .title(gettext("Preferences"))
            .content_width(920)
            .content_height(640)
            .build();
        window.add_css_class("preferences");
        window.add_css_class("mail-preferences");
        let title = adw::WindowTitle::new(&gettext("Preferences"), "");
        let search = gtk::SearchEntry::builder()
            .placeholder_text(gettext("Search preferences"))
            .hexpand(true)
            .build();
        super::super::name(&search, &gettext("Search preferences"));
        let titles = gtk::Stack::builder().hhomogeneous(false).build();
        titles.add_named(&title, Some("title"));
        let clamp = adw::Clamp::builder()
            .maximum_size(400)
            .child(&search)
            .build();
        titles.add_named(&clamp, Some("search"));
        let searching = gtk::ToggleButton::builder()
            .icon_name("edit-find-symbolic")
            .tooltip_text(gettext("Search"))
            .build();
        super::super::name(&searching, &gettext("Search"));
        let back = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text(gettext("Back"))
            .visible(false)
            .build();
        super::super::name(&back, &gettext("Back"));
        let header = adw::HeaderBar::builder()
            .title_widget(&titles)
            .centering_policy(adw::CenteringPolicy::Strict)
            .build();
        header.pack_start(&back);
        header.pack_start(&searching);
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .tab_behavior(gtk::ListTabBehavior::Item)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(8)
            .margin_end(8)
            .build();
        list.add_css_class("navigation-sidebar");
        list.add_css_class("preferences-sections");
        let sidebar = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let stack = gtk::Stack::builder()
            .hhomogeneous(false)
            .vhomogeneous(false)
            .build();
        let split = adw::NavigationSplitView::builder()
            .sidebar(&adw::NavigationPage::new(&sidebar, &gettext("Preferences")))
            .content(&adw::NavigationPage::new(&stack, &gettext("Preferences")))
            .min_sidebar_width(210.0)
            .max_sidebar_width(230.0)
            .show_content(false)
            .build();
        let results = adw::PreferencesGroup::new();
        let results_page = adw::PreferencesPage::new();
        results_page.add(&results);
        let empty = adw::StatusPage::builder()
            .icon_name("edit-find-symbolic")
            .title(gettext("No Results Found"))
            .description(gettext("Try a different search"))
            .build();
        let result_stack = gtk::Stack::new();
        result_stack.add_named(&results_page, Some("results"));
        result_stack.add_named(&empty, Some("empty"));
        let content = gtk::Stack::builder()
            .hhomogeneous(false)
            .vhomogeneous(false)
            .build();
        content.add_named(&split, Some("pages"));
        content.add_named(&result_stack, Some("search"));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&content));
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&toolbar));
        window.set_child(Some(&toasts));
        let this = Rc::new(Self {
            window,
            toasts,
            split,
            stack,
            list,
            pages: RefCell::new(Vec::new()),
            title,
            search,
            searching,
            results,
            result_stack,
        });
        let weak = Rc::downgrade(&this);
        this.list.connect_row_selected(move |_, row| {
            let (Some(this), Some(row)) = (weak.upgrade(), row) else {
                return;
            };
            let page = this.pages.borrow().get(row.index() as usize).cloned();
            if let Some(page) = page {
                this.stack.set_visible_child(&page);
                this.update_title();
            }
        });
        let weak = Rc::downgrade(&this);
        this.list.connect_row_activated(move |list, row| {
            list.select_row(Some(row));
            if let Some(this) = weak.upgrade() {
                this.split.set_show_content(true);
            }
        });
        let weak = Rc::downgrade(&this);
        back.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                if this.searching.is_active() {
                    this.searching.set_active(false);
                } else {
                    this.split.set_show_content(false);
                }
            }
        });
        let weak = Rc::downgrade(&this);
        let shown_back = back.clone();
        this.split.connect_show_content_notify(move |split| {
            shown_back.set_visible(split.is_collapsed() && split.shows_content());
            if let Some(this) = weak.upgrade() {
                this.update_title();
            }
        });
        let weak = Rc::downgrade(&this);
        this.split.connect_collapsed_notify(move |split| {
            back.set_visible(split.is_collapsed() && split.shows_content());
            if let Some(this) = weak.upgrade() {
                this.update_title();
            }
        });
        let weak = Rc::downgrade(&this);
        this.searching.connect_toggled(move |button| {
            let Some(this) = weak.upgrade() else { return };
            let active = button.is_active();
            titles.set_visible_child_name(if active { "search" } else { "title" });
            content.set_visible_child_name(if active { "search" } else { "pages" });
            if active {
                this.fill_results();
                this.search.grab_focus();
            } else {
                this.search.set_text("");
            }
        });
        let weak = Rc::downgrade(&this);
        this.search.connect_search_changed(move |_| {
            if let Some(this) = weak.upgrade() {
                this.fill_results();
            }
        });
        let weak = Rc::downgrade(&this);
        this.search.connect_activate(move |_| {
            if let Some(this) = weak.upgrade()
                && let Some(result) = descendants(this.results.upcast_ref())
                    .into_iter()
                    .find_map(|widget| widget.downcast::<adw::ActionRow>().ok())
            {
                result.emit_by_name::<()>("activated", &[]);
            }
        });
        let weak = Rc::downgrade(&this);
        this.search.connect_stop_search(move |_| {
            if let Some(this) = weak.upgrade() {
                this.searching.set_active(false);
            }
        });
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&this);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(this) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if key == gtk::gdk::Key::f && modifiers.contains(crate::ui::PRIMARY) {
                this.searching.set_active(true);
                this.search.grab_focus();
                return glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::Left
                && modifiers.contains(gtk::gdk::ModifierType::ALT_MASK)
                && this.split.is_collapsed()
                && this.split.shows_content()
            {
                if this.searching.is_active() {
                    this.searching.set_active(false);
                } else {
                    this.split.set_show_content(false);
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        this.window.add_controller(keys);
        this
    }

    pub fn present(self: &Rc<Self>, parent: &impl IsA<gtk::Widget>) {
        // Callers open Preferences without keeping its handle. Keep the
        // navigation alive while it is open, then break this hold on close.
        let held = RefCell::new(Some(Rc::clone(self)));
        self.window.connect_closed(move |_| {
            held.borrow_mut().take();
        });
        self.window.present(Some(parent));
    }

    pub fn add(&self, page: &adw::PreferencesPage) {
        for widget in descendants(page.upcast_ref()) {
            if let Some(row) = widget.downcast_ref::<adw::ComboRow>()
                && row
                    .model()
                    .is_some_and(|model| model.is::<gtk::StringList>())
            {
                super::super::combo_value::widen_value(row);
            }
        }
        let item = gtk::ListBoxRow::new();
        // An ActionRow is itself a ListBoxRow, so use its label and icon in
        // the navigation row without introducing a second nested row.
        let label = gtk::Label::builder()
            .label(page.title())
            .xalign(0.0)
            .hexpand(true)
            .build();
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        line.append(&gtk::Image::from_icon_name(
            page.icon_name()
                .as_deref()
                .unwrap_or("preferences-system-symbolic"),
        ));
        line.append(&label);
        let arrow = gtk::Image::from_icon_name("penguin-mail-chevron-right-symbolic");
        arrow.add_css_class("preference-section-arrow");
        arrow.set_visible(false);
        line.append(&arrow);
        item.set_child(Some(&line));
        super::super::name(&item, &page.title());
        self.list.append(&item);
        self.stack.add_child(page);
        self.pages.borrow_mut().push(page.clone());
        if let Some(clamp) = descendants(page.upcast_ref())
            .into_iter()
            .find_map(|widget| widget.downcast::<adw::Clamp>().ok())
        {
            clamp.set_maximum_size(740);
            clamp.set_tightening_threshold(740);
        }
        if self.pages.borrow().len() == 1 {
            self.list.select_row(Some(&item));
        }
    }

    pub fn set_visible_page(&self, page: &adw::PreferencesPage) {
        let index = self.pages.borrow().iter().position(|p| p == page);
        if let Some(index) = index {
            self.list
                .select_row(self.list.row_at_index(index as i32).as_ref());
            self.split.set_show_content(true);
        }
    }

    pub fn set_visible_page_name(&self, name: &str) {
        let page = self
            .pages
            .borrow()
            .iter()
            .find(|p| p.name().as_deref() == Some(name))
            .cloned();
        if let Some(page) = page {
            self.set_visible_page(&page);
        }
    }

    pub fn adapt(&self, narrow: &adw::Breakpoint) {
        narrow.add_setter(&self.split, "collapsed", Some(&true.to_value()));
        narrow.add_setter(&self.window, "content-height", Some(&680.to_value()));
        let window = self.window.downgrade();
        narrow.connect_apply(move |_| {
            if let Some(window) = window.upgrade() {
                window.add_css_class("preferences-compact");
            }
        });
        let window = self.window.downgrade();
        narrow.connect_unapply(move |_| {
            if let Some(window) = window.upgrade() {
                window.remove_css_class("preferences-compact");
            }
        });
        let mut item = self.list.first_child();
        while let Some(row) = item {
            if let Some(line) = row.first_child()
                && let Some(arrow) = line.last_child()
            {
                narrow.add_setter(&arrow, "visible", Some(&true.to_value()));
            }
            item = row.next_sibling();
        }
        for page in self.pages.borrow().iter() {
            for widget in descendants(page.upcast_ref()) {
                if widget.has_css_class("preference-explanation") {
                    if let Some(explanation) = widget.property::<Option<String>>("subtitle") {
                        explain(&widget, &explanation);
                        widget.connect_notify_local(Some("subtitle"), move |widget, _| {
                            explain(widget, &explanation);
                        });
                    }
                    narrow.add_setter(&widget, "subtitle", Some(&None::<String>.to_value()));
                }
            }
        }
    }

    fn update_title(&self) {
        let title = if self.split.is_collapsed() && self.split.shows_content() {
            self.stack
                .visible_child()
                .and_downcast::<adw::PreferencesPage>()
                .map(|p| p.title().to_string())
                .unwrap_or_else(|| gettext("Preferences"))
        } else {
            gettext("Preferences")
        };
        self.title.set_title(&title);
    }

    fn fill_results(self: &Rc<Self>) {
        // The group owns its rows; drop the previous query before building
        // the next one so searches do not retain the whole dialog.
        while let Some(row) = descendants(self.results.upcast_ref())
            .into_iter()
            .find_map(|w| w.downcast::<adw::ActionRow>().ok())
        {
            self.results.remove(&row);
        }
        let query = glib::casefold(self.search.text());
        let mut count = 0;
        for page in self.pages.borrow().iter() {
            for widget in descendants(page.upcast_ref()) {
                let Some(row) = widget.downcast_ref::<adw::PreferencesRow>() else {
                    continue;
                };
                if !row.get_visible() || row.title().is_empty() || row.has_css_class("header") {
                    continue;
                }
                let subtitle = row
                    .clone()
                    .downcast::<adw::ActionRow>()
                    .ok()
                    .and_then(|r| r.subtitle().map(|s| s.to_string()))
                    .unwrap_or_default();
                let explanation = row.tooltip_text().unwrap_or_default();
                if !matches_query(
                    &plain_title(row),
                    &format!("{subtitle} {explanation}"),
                    &query,
                ) {
                    continue;
                }
                let mut trail = Vec::new();
                let mut parent = row.parent();
                while let Some(widget) = parent {
                    if let Some(expander) = widget.downcast_ref::<adw::ExpanderRow>() {
                        trail.push(plain_title(expander.upcast_ref()));
                    }
                    parent = widget.parent();
                }
                trail.push(page.title().to_string());
                trail.reverse();
                let result = adw::ActionRow::builder()
                    .use_markup(false)
                    .activatable(true)
                    .build();
                result.set_title(&plain_title(row));
                result.set_subtitle(&trail.join(" · "));
                result.add_suffix(&gtk::Image::from_icon_name(
                    "penguin-mail-chevron-right-symbolic",
                ));
                let (weak, page, target) = (Rc::downgrade(self), page.downgrade(), row.downgrade());
                result.connect_activated(move |_| {
                    let (Some(this), Some(page), Some(target)) =
                        (weak.upgrade(), page.upgrade(), target.upgrade())
                    else {
                        return;
                    };
                    this.searching.set_active(false);
                    this.set_visible_page(&page);
                    let mut parent = target.parent();
                    while let Some(widget) = parent {
                        if let Some(expander) = widget.downcast_ref::<adw::ExpanderRow>() {
                            expander.set_expanded(true);
                        }
                        parent = widget.parent();
                    }
                    target.set_focusable(true);
                    target.grab_focus();
                });
                self.results.add(&result);
                count += 1;
            }
        }
        self.result_stack
            .set_visible_child_name(if count == 0 { "empty" } else { "results" });
    }
}

/// Keeps help available to search and to the control a screen reader reaches.
pub(in crate::ui) fn explain(widget: &impl IsA<gtk::Widget>, explanation: &str) {
    let widget = widget.as_ref();
    widget.set_tooltip_text(Some(explanation));
    if let Some(row) = widget.downcast_ref::<adw::ExpanderRow>() {
        widget.update_property(&[gtk::accessible::Property::Label(&plain_title(
            row.upcast_ref(),
        ))]);
    }
    widget.reset_relation(gtk::AccessibleRelation::DescribedBy);
    widget.update_property(&[gtk::accessible::Property::Description(explanation)]);
    for child in descendants(widget) {
        if child.is::<gtk::Switch>() || child.has_css_class("header") {
            child.reset_relation(gtk::AccessibleRelation::DescribedBy);
            child.update_property(&[gtk::accessible::Property::Description(explanation)]);
        }
    }
}

fn descendants(root: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut widgets = Vec::new();
    let mut child = root.first_child();
    while let Some(widget) = child {
        if widget.get_visible() {
            widgets.push(widget.clone());
            widgets.extend(descendants(&widget));
        }
        child = widget.next_sibling();
    }
    widgets
}

fn plain_title(row: &adw::PreferencesRow) -> String {
    let title = row.title();
    if row.uses_markup() {
        gtk::pango::parse_markup(&title, '\0')
            .map(|(_, text, _)| text.to_string())
            .unwrap_or_else(|_| title.to_string())
    } else {
        title.to_string()
    }
}

fn matches_query(title: &str, subtitle: &str, query: &str) -> bool {
    glib::casefold(title).contains(query) || glib::casefold(subtitle).contains(query)
}

#[cfg(test)]
pub(crate) mod checks {
    use super::*;

    pub fn run() {
        a_search_opens_a_row_inside_its_account();
        navigation_lives_until_the_dialog_closes();
        hidden_explanations_and_plain_breadcrumbs_stay_searchable();
        an_expander_appears_once_in_search();
        compact_explanations_have_spoken_descriptions();
        tab_leaves_the_selected_page_in_place();
    }

    fn fixture() -> (Rc<Dialog>, adw::PreferencesPage, adw::ExpanderRow) {
        let dialog = Dialog::new();
        let page = adw::PreferencesPage::builder()
            .title("Accounts")
            .name("accounts")
            .build();
        let group = adw::PreferencesGroup::new();
        let account =
            crate::ui::disclosure::row(adw::ExpanderRow::builder().title("me@example.com").build());
        account.add_row(
            &adw::SwitchRow::builder()
                .title("Contacts")
                .use_markup(false)
                .build(),
        );
        group.add(&account);
        page.add(&group);
        dialog.add(&page);
        (dialog, page, account)
    }

    fn a_search_opens_a_row_inside_its_account() {
        let window = adw::Window::new();
        window.present();
        let (dialog, page, account) = fixture();
        dialog.present(&window);
        dialog.split.set_collapsed(true);
        assert!(
            !dialog.split.shows_content(),
            "a narrow dialog should start on its page list"
        );
        dialog.search.set_text("contacts");
        dialog.fill_results();
        let result = descendants(dialog.results.upcast_ref())
            .into_iter()
            .find_map(|w| w.downcast::<adw::ActionRow>().ok())
            .expect("a nested contacts result");
        result.emit_by_name::<()>("activated", &[]);
        assert!(
            account.is_expanded(),
            "a search result should expand its account"
        );
        assert_eq!(
            dialog.stack.visible_child().as_ref(),
            Some(page.upcast_ref())
        );
        assert!(
            dialog.split.shows_content(),
            "a search result should leave the narrow page list"
        );
        dialog.set_visible_page_name("accounts");
        assert!(
            dialog.split.shows_content(),
            "a named page link should open its page"
        );
        dialog.window.force_close();
        window.destroy();
    }

    fn hidden_explanations_and_plain_breadcrumbs_stay_searchable() {
        let window = adw::Window::new();
        window.present();
        let (dialog, _, account) = fixture();
        account.set_use_markup(false);
        account.set_title("r&d@example.com");
        let row = descendants(account.upcast_ref())
            .into_iter()
            .find_map(|widget| widget.downcast::<adw::SwitchRow>().ok())
            .unwrap();
        row.set_tooltip_text(Some("Suggest recipients from phone numbers"));
        row.set_subtitle("");
        dialog.present(&window);
        dialog.search.set_text("PHONE");
        dialog.fill_results();
        let result = descendants(dialog.results.upcast_ref())
            .into_iter()
            .find_map(|widget| widget.downcast::<adw::ActionRow>().ok())
            .expect("the hidden explanation should still match");
        assert_eq!(
            result.subtitle().as_deref(),
            Some("Accounts · r&d@example.com")
        );
        dialog.search.emit_by_name::<()>("activate", &[]);
        assert!(account.is_expanded());
        dialog.window.force_close();
        window.destroy();
    }

    fn an_expander_appears_once_in_search() {
        let window = adw::Window::new();
        window.present();
        let (dialog, _, account) = fixture();
        account.add_row(&crate::ui::disclosure::row(
            adw::ExpanderRow::builder().title("Signature").build(),
        ));
        dialog.present(&window);
        dialog.search.set_text("signature");
        dialog.fill_results();
        let rows = descendants(dialog.results.upcast_ref())
            .into_iter()
            .filter(|widget| widget.is::<adw::ActionRow>())
            .count();
        assert_eq!(
            rows, 1,
            "the expander's internal header is not another preference"
        );
        dialog.window.force_close();
        window.destroy();
    }

    fn tab_leaves_the_selected_page_in_place() {
        let window = adw::Window::new();
        window.present();
        let (dialog, page, _) = fixture();
        dialog.add(
            &adw::PreferencesPage::builder()
                .title("Reading")
                .name("reading")
                .build(),
        );
        dialog.present(&window);
        dialog.list.row_at_index(0).unwrap().grab_focus();
        dialog.list.child_focus(gtk::DirectionType::TabForward);
        assert_eq!(
            dialog.list.selected_row().unwrap().index(),
            0,
            "Tab should leave the selected page in place"
        );
        assert_eq!(
            dialog.stack.visible_child().as_ref(),
            Some(page.upcast_ref())
        );
        dialog.window.force_close();
        window.destroy();
    }

    fn compact_explanations_have_spoken_descriptions() {
        let window = adw::Window::new();
        window.present();
        let (dialog, _, account) = fixture();
        let row = descendants(account.upcast_ref())
            .into_iter()
            .find_map(|widget| widget.downcast::<adw::SwitchRow>().ok())
            .unwrap();
        row.set_subtitle("Contacts stay on this computer");
        row.add_css_class("preference-explanation");
        let narrow =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 450sp").unwrap());
        dialog.adapt(&narrow);
        dialog.window.add_breakpoint(narrow);
        dialog.present(&window);
        assert!(gtk::test_accessible_has_property(
            row.upcast_ref::<gtk::Widget>(),
            gtk::AccessibleProperty::Description
        ));
        let switch = descendants(row.upcast_ref())
            .into_iter()
            .find_map(|widget| widget.downcast::<gtk::Switch>().ok())
            .unwrap();
        assert!(gtk::test_accessible_has_property(
            &switch,
            gtk::AccessibleProperty::Description
        ));
        dialog.window.force_close();
        window.destroy();
    }

    fn navigation_lives_until_the_dialog_closes() {
        let window = adw::Window::new();
        window.present();
        let (dialog, _, _) = fixture();
        let weak = Rc::downgrade(&dialog);
        dialog.present(&window);
        drop(dialog);
        assert!(
            weak.upgrade().is_some(),
            "the caller can discard a presented dialog handle"
        );
        let native = weak.upgrade().unwrap().window.clone();
        native.force_close();
        assert!(
            weak.upgrade().is_none(),
            "closing should release the navigation"
        );
        window.destroy();
    }
}
