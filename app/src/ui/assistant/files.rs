//! Files picked for a question. Picking names a path; the existing mail
//! tools ask before reading its contents into a message.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::gio;
use mailrs_domain::translate::{fill, gettext};

pub(super) struct Files {
    pub row: gtk::FlowBox,
    paths: RefCell<Vec<PathBuf>>,
}

impl Files {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            row: gtk::FlowBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .column_spacing(4)
                .row_spacing(4)
                .min_children_per_line(1)
                .max_children_per_line(6)
                .visible(false)
                .build(),
            paths: RefCell::new(Vec::new()),
        })
    }

    pub fn choose(self: &Rc<Self>, parent: &gtk::Window) {
        let dialog = gtk::FileDialog::builder().title(gettext("Attach")).build();
        let weak = Rc::downgrade(self);
        dialog.open_multiple(Some(parent), None::<&gio::Cancellable>, move |result| {
            let (Some(files), Ok(picked)) = (weak.upgrade(), result) else {
                return;
            };
            for index in 0..picked.n_items() {
                if let Some(path) = picked
                    .item(index)
                    .and_downcast::<gio::File>()
                    .and_then(|f| f.path())
                {
                    files.add(path);
                }
            }
        });
    }

    fn add(self: &Rc<Self>, path: PathBuf) {
        if self.paths.borrow().contains(&path) {
            return;
        }
        let filename = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy();
        let label = gtk::Label::builder()
            .label(&*filename)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(22)
            .build();
        let remove = gtk::Button::builder()
            .icon_name("penguin-mail-window-close-symbolic")
            .css_classes(["flat", "circular"])
            .build();
        let said = fill(&gettext("Remove {file}"), &[("file", &filename)]);
        crate::ui::name(&remove, &said);
        remove.set_tooltip_text(Some(&said));
        let chip = gtk::Box::builder()
            .spacing(2)
            .css_classes(["assistant-file"])
            .build();
        chip.set_tooltip_text(Some(&path.to_string_lossy()));
        chip.append(&label);
        chip.append(&remove);
        self.row.insert(&chip, -1);
        self.row.set_visible(true);
        self.paths.borrow_mut().push(path.clone());
        let weak = Rc::downgrade(self);
        let chip = chip.downgrade();
        remove.connect_clicked(move |_| {
            if let Some(files) = weak.upgrade() {
                files.paths.borrow_mut().retain(|p| p != &path);
                if let Some(child) = chip
                    .upgrade()
                    .and_then(|c| c.parent())
                    .and_downcast::<gtk::FlowBoxChild>()
                {
                    files.row.remove(&child);
                }
                files.row.set_visible(!files.paths.borrow().is_empty());
            }
        });
    }

    pub fn take(&self, question: &str) -> (String, String, Arc<[PathBuf]>) {
        let paths = self.paths.take();
        self.row.remove_all();
        self.row.set_visible(false);
        let shown = if paths.is_empty() {
            question.to_string()
        } else {
            let names: Vec<_> = paths
                .iter()
                .map(|p| p.file_name().unwrap_or(p.as_os_str()).to_string_lossy())
                .collect();
            format!(
                "{question}\n\n{}",
                fill(
                    &gettext("Attached: {files}"),
                    &[("files", &names.join(", "))]
                )
            )
        };
        // Resolve only the files the chooser returned. This grants no access
        // to siblings, and metadata does not read their contents.
        let picked = paths
            .iter()
            .filter_map(|p| std::fs::canonicalize(p).ok())
            .collect();
        (with_paths(question, &paths), shown, picked)
    }

    pub fn clear(&self) {
        self.paths.borrow_mut().clear();
        self.row.remove_all();
        self.row.set_visible(false);
    }
}

fn with_paths(question: &str, paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return question.to_string();
    }
    // JSON escapes quotes and newlines in names, so a filename cannot
    // alter the structure of this list. No file contents are read here.
    let paths: Vec<_> = paths.iter().map(|p| p.to_string_lossy()).collect();
    format!(
        "{question}\n\nFiles selected by the user for this request (local paths, not file contents):\n{}",
        serde_json::json!(paths)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_paths_are_escaped_without_reading_files() {
        let path = PathBuf::from("/missing/lease\"\n.pdf");
        let question = with_paths("Send the lease", std::slice::from_ref(&path));
        let paths: Vec<String> = serde_json::from_str(question.lines().last().unwrap()).unwrap();
        assert_eq!(paths, [path.to_string_lossy()]);
        assert!(question.starts_with("Send the lease\n\n"));
        assert_eq!(with_paths("Plain question", &[]), "Plain question");
    }
}

#[cfg(test)]
pub(super) mod checks {
    use super::*;
    pub fn run() {
        let files = Files::new();
        files.add(PathBuf::from("/missing/lease.pdf"));
        files.add(PathBuf::from("/missing/lease.pdf"));
        assert_eq!(files.paths.borrow().len(), 1);
        let row = files.row.first_child().unwrap();
        let chip = row.first_child().unwrap().downcast::<gtk::Box>().unwrap();
        let button = chip
            .last_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        button.emit_clicked();
        assert!(files.paths.borrow().is_empty());
        assert!(!files.row.is_visible());
        files.add(PathBuf::from("/missing/new.pdf"));
        assert!(files.take("Send this").0.contains("/missing/new.pdf"));
        assert!(files.paths.borrow().is_empty());
    }
}
