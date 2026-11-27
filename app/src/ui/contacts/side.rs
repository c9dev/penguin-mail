//! The sidebar of the Contacts mode: Lists, Groups and Accounts.

use std::rc::Rc;

use adw::prelude::*;
use mailrs_domain::translate::gettext;

use super::{Contacts, List, parts, people};
use crate::format::account_label;

impl Contacts {
    pub(super) fn draw_side(self: &Rc<Self>) {
        while let Some(child) = self.side_box.first_child() {
            self.side_box.remove(&child);
        }
        let (current, counts, groups, accounts) = {
            let state = self.state.borrow();
            let d = &state.data;
            let grouped = people::group(&d.contacts, &d.links);
            let settings = self.app.upgrade().map(|a| a.settings());
            let vips = grouped
                .iter()
                .filter(|p| {
                    settings
                        .as_ref()
                        .is_some_and(|s| p.emails(&d.contacts).iter().any(|e| s.is_vip(e)))
                })
                .count();
            let mut groups: Vec<(String, usize)> = Vec::new();
            for p in &grouped {
                let mut names: Vec<&String> = p
                    .members
                    .iter()
                    .flat_map(|&i| d.contacts[i].details.groups.iter())
                    .collect();
                names.sort();
                names.dedup();
                for name in names {
                    match groups.iter_mut().find(|(g, _)| g == name) {
                        Some((_, n)) => *n += 1,
                        None => groups.push((name.clone(), 1)),
                    }
                }
            }
            groups.sort_by_key(|(g, _)| g.to_lowercase());
            let counts = [
                grouped.len(),
                vips,
                super::from_mail(d).len(),
                state.merges.len(),
                state.pairs.len(),
            ];
            (
                state.list.clone().unwrap_or(List::All),
                counts,
                groups,
                d.accounts.clone(),
            )
        };
        let nav = |label: &str, count: Option<String>, tip: &str, list: List, current: &List| {
            let row = gtk::Box::builder().spacing(8).build();
            let name = gtk::Label::builder()
                .label(label)
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            row.append(&name);
            if let Some(count) = count {
                row.append(&gtk::Label::builder().label(count).css_classes(["cts-n"]).build());
            }
            self.nav_button(row, tip, label, list, current)
        };
        self.side_box
            .append(&parts::label(&gettext("Lists"), &["cts-kicker"]));
        for (label, count, tip, list) in [
            (gettext("All contacts"), counts[0], String::new(), List::All),
            (
                gettext("VIPs"),
                counts[1],
                gettext("People you mark, so you can open them from here"),
                List::Vips,
            ),
            (
                gettext("From mail"),
                counts[2],
                gettext("People found in mail. They are not contacts yet."),
                List::FromMail,
            ),
            (
                gettext("Same address"),
                counts[3],
                gettext("Two contacts in one account share an address. Merging deletes one."),
                List::SameAddress,
            ),
            (
                gettext("Same person"),
                counts[4],
                gettext("A close name in two accounts. Linking deletes nothing."),
                List::SamePerson,
            ),
        ] {
            self.side_box
                .append(&nav(&label, Some(count.to_string()), &tip, list, &current));
        }
        if !groups.is_empty() {
            self.side_box
                .append(&parts::label(&gettext("Groups"), &["cts-kicker"]));
            for (name, count) in groups {
                self.side_box.append(&nav(
                    &name,
                    Some(count.to_string()),
                    "",
                    List::Group(name.clone()),
                    &current,
                ));
            }
        }
        self.side_box
            .append(&parts::label(&gettext("Accounts"), &["cts-kicker"]));
        for account in accounts {
            let on = self.reads(&account);
            let held = self
                .state
                .borrow()
                .data
                .contacts
                .iter()
                .filter(|c| c.account_id == account.id)
                .count();
            let row = gtk::Box::builder().spacing(8).build();
            let dot = parts::dot(account.id);
            dot.set_valign(gtk::Align::Start);
            dot.set_margin_top(5);
            row.append(&dot);
            let text = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .hexpand(true)
                .build();
            text.append(&gtk::Label::builder().label(account_label(account.id).short).xalign(0.0).build());
            text.append(
                &gtk::Label::builder()
                    .label(&account.email)
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(["cts-small"])
                    .build(),
            );
            row.append(&text);
            let state_word = if !on {
                Some(gettext("Off"))
            } else if held == 0 {
                Some(gettext("On"))
            } else {
                None
            };
            if let Some(word) = state_word {
                row.append(&gtk::Label::builder().label(word).valign(gtk::Align::Start).css_classes(["cts-n"]).build());
            }
            let button = self.nav_button(
                row,
                "",
                &format!("{}, {}", account_label(account.id).short, account.email),
                List::Account(account.id),
                &current,
            );
            button.add_css_class("stack");
            self.side_box.append(&button);
        }
    }

    fn nav_button(
        self: &Rc<Self>,
        child: gtk::Box,
        tip: &str,
        spoken: &str,
        list: List,
        current: &List,
    ) -> gtk::ToggleButton {
        let button = gtk::ToggleButton::builder()
            .child(&child)
            .active(&list == current)
            .css_classes(["cts-nav"])
            .build();
        if !tip.is_empty() {
            button.set_tooltip_text(Some(tip));
        }
        super::super::name(&button, spoken);
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.show_list(list.clone());
            }
        });
        button
    }
}
