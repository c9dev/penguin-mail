//! The files attached to an event, as the event popover and the editor
//! list them: an icon for the file's type, its title, and a click that
//! opens it in the browser. The icon comes from the icon theme by mime
//! type; Google's own `iconLink` is never loaded, since a remote image
//! tells its server who looked and when.

use adw::prelude::*;
use gtk::gio;
use mailrs_domain::calendar::{Attachment, UploadProblem};
use mailrs_domain::translate::{fill, gettext};

use crate::ui;
use mailrs_sync::{BackendError, Permitted, SyncError};

/// What became of a file the editor tried to upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Uploaded {
    /// On Drive, linked.
    Done(Attachment),
    /// The account has not granted Drive.
    NeedsPermission,
    /// The network or the rate limit stopped it: the file stays waiting,
    /// and the queue uploads it with the event.
    Later,
    /// Anything else, in words to show.
    Failed(String),
}

/// The editor's reading of an upload's answer.
pub fn uploaded(answer: Result<Permitted<Attachment>, SyncError>) -> Uploaded {
    match answer {
        Ok(Permitted::Done(file)) => Uploaded::Done(file),
        Ok(Permitted::NeedsPermission) | Err(SyncError::Backend(BackendError::NeedsPermission)) => {
            Uploaded::NeedsPermission
        }
        Err(SyncError::Backend(err)) if err.is_transient() => Uploaded::Later,
        Err(err) => Uploaded::Failed(err.to_string()),
    }
}

/// The theme icon for a file of type `mime`. Google's own types and the
/// office formats share the office icons; anything else falls back on its
/// top-level type, and an unknown type on the paper clip.
pub fn icon_name(mime: &str) -> &'static str {
    let mime = mime.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
    let (top, sub) = mime.split_once('/').unwrap_or((mime.as_str(), ""));
    let has = |words: &[&str]| words.iter().any(|w| sub.contains(w));
    if top == "application" && has(&["google-apps.folder"]) {
        "inode-directory-symbolic"
    } else if top == "application" && has(&["google-apps.drawing"]) {
        "x-office-drawing-symbolic"
    } else if has(&["spreadsheet", "ms-excel"]) || mime == "text/csv" {
        "x-office-spreadsheet-symbolic"
    } else if has(&["presentation", "ms-powerpoint"]) {
        "x-office-presentation-symbolic"
    } else if top == "application" && (has(&["google-apps.document", "wordprocessing", "opendocument.text", "msword"]) || sub == "pdf") {
        "x-office-document-symbolic"
    } else if top == "application" && has(&["zip", "gzip", "x-tar", "x-7z", "x-rar", "x-bzip", "x-xz"]) {
        "package-x-generic-symbolic"
    } else {
        match top {
            "image" => "image-x-generic-symbolic",
            "audio" => "audio-x-generic-symbolic",
            "video" => "video-x-generic-symbolic",
            "text" => "text-x-generic-symbolic",
            _ => "mail-attachment-symbolic",
        }
    }
}

/// The words a row shows for `file`: its title, or the name of the file it
/// waits to upload, or a stand-in when neither says anything.
pub fn title(file: &Attachment) -> String {
    let title = file.title.trim();
    if !title.is_empty() {
        return title.to_string();
    }
    file.waiting
        .as_deref()
        .and_then(|path| std::path::Path::new(path).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| gettext("Untitled file"))
}

/// The second line of a row for a file still waiting to upload: when it
/// goes, or why it did not. `None` for a file on Drive.
pub fn note(file: &Attachment) -> Option<String> {
    file.waiting.as_ref()?;
    Some(match &file.problem {
        None => gettext("Uploads when you are back online"),
        Some(UploadProblem::NeedsAccess) => gettext("Waiting for access"),
        Some(UploadProblem::NotFound) => gettext("File not found"),
        Some(UploadProblem::Refused(reason)) => fill(&gettext("Could not upload: {reason}"), &[("reason", reason)]),
    })
}

/// Whether the event's guests can open `file`, for a file the app uploaded
/// on an event with guests. `None` otherwise.
pub fn share_note(file: &Attachment, has_guests: bool) -> Option<String> {
    match (file.share, has_guests) {
        (Some(true), true) => Some(gettext("Guests can open this file")),
        (Some(false), true) => Some(gettext("Not shared with the guests")),
        _ => None,
    }
}

/// Opens `file` in the browser when its link is `https`. Anything else,
/// such as a file still waiting to upload, opens nothing.
pub fn open(file: &Attachment, from: &impl IsA<gtk::Widget>) {
    let Some(link) = file.link() else { return };
    let window = from.as_ref().root().and_downcast::<gtk::Window>();
    gtk::UriLauncher::new(link).launch(window.as_ref(), gio::Cancellable::NONE, |_| {});
}

/// The words a screen reader hears for a row that opens `file`.
pub fn open_words(file: &Attachment) -> String {
    fill(&gettext("Open {file} in the browser"), &[("file", &title(file))])
}

/// A row for `file`: its icon, its title, what happens to it if it still
/// waits to upload, and a click that opens it when it has a link.
pub fn row(file: &Attachment) -> adw::ActionRow {
    let row = ui::plain_row().title_lines(1).build();
    row.set_title(&title(file));
    if let Some(note) = note(file) {
        row.set_subtitle(&note);
    }
    let icon = gtk::Image::from_icon_name(icon_name(&file.mime_type));
    icon.add_css_class("dim-label");
    row.add_prefix(&icon);
    if file.link().is_some() {
        row.set_activatable(true);
        row.set_tooltip_text(Some(&gettext("Open in Browser")));
        ui::describe(&row, &title(file), &open_words(file));
        let opened = file.clone();
        row.connect_activated(move |row| open(&opened, row));
    }
    row
}

/// Widget checks, run from the one GTK test (`composer::richbuffer`).
#[cfg(test)]
pub(crate) mod checks {
    use super::*;

    pub fn run() {
        a_file_name_with_markup_characters_shows_as_written();
    }

    /// A row reads its title as Pango markup unless told otherwise, and a
    /// Drive file called "Q&A.pdf" failed to parse and showed no title.
    fn a_file_name_with_markup_characters_shows_as_written() {
        let file = Attachment { title: "Q&A <draft>.pdf".into(), ..Attachment::default() };
        let row = row(&file);
        assert!(!row.uses_markup(), "a file's name is shown as text, not markup");
        assert_eq!(row.title(), "Q&A <draft>.pdf");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(title: &str, mime: &str) -> Attachment {
        Attachment {
            title: title.into(),
            file_url: "https://drive.google.com/file/d/1/view".into(),
            mime_type: mime.into(),
            ..Attachment::default()
        }
    }

    #[test]
    fn google_files_take_the_office_icon_of_their_kind() {
        assert_eq!(icon_name("application/vnd.google-apps.document"), "x-office-document-symbolic");
        assert_eq!(icon_name("application/vnd.google-apps.spreadsheet"), "x-office-spreadsheet-symbolic");
        assert_eq!(icon_name("application/vnd.google-apps.presentation"), "x-office-presentation-symbolic");
        assert_eq!(icon_name("application/vnd.google-apps.drawing"), "x-office-drawing-symbolic");
        assert_eq!(icon_name("application/vnd.google-apps.folder"), "inode-directory-symbolic");
    }

    #[test]
    fn office_files_take_the_same_icons_as_googles() {
        assert_eq!(icon_name("application/pdf"), "x-office-document-symbolic");
        assert_eq!(
            icon_name("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
            "x-office-document-symbolic"
        );
        assert_eq!(icon_name("application/vnd.oasis.opendocument.text"), "x-office-document-symbolic");
        assert_eq!(icon_name("application/msword"), "x-office-document-symbolic");
        assert_eq!(icon_name("application/vnd.ms-excel"), "x-office-spreadsheet-symbolic");
        assert_eq!(
            icon_name("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
            "x-office-spreadsheet-symbolic"
        );
        assert_eq!(icon_name("text/csv"), "x-office-spreadsheet-symbolic");
        assert_eq!(
            icon_name("application/vnd.openxmlformats-officedocument.presentationml.presentation"),
            "x-office-presentation-symbolic"
        );
        assert_eq!(icon_name("application/vnd.oasis.opendocument.presentation"), "x-office-presentation-symbolic");
    }

    #[test]
    fn media_takes_the_icon_of_its_top_level_type() {
        assert_eq!(icon_name("image/png"), "image-x-generic-symbolic");
        assert_eq!(icon_name("audio/ogg"), "audio-x-generic-symbolic");
        assert_eq!(icon_name("video/mp4"), "video-x-generic-symbolic");
        assert_eq!(icon_name("text/plain"), "text-x-generic-symbolic");
    }

    #[test]
    fn archives_take_the_package_icon() {
        for mime in ["application/zip", "application/gzip", "application/x-tar", "application/x-7z-compressed"] {
            assert_eq!(icon_name(mime), "package-x-generic-symbolic", "{mime}");
        }
    }

    #[test]
    fn a_type_read_in_any_case_or_with_parameters_finds_its_icon() {
        assert_eq!(icon_name("Image/JPEG"), "image-x-generic-symbolic");
        assert_eq!(icon_name("text/plain; charset=utf-8"), "text-x-generic-symbolic");
    }

    #[test]
    fn an_unknown_or_missing_type_takes_the_paper_clip() {
        assert_eq!(icon_name("application/octet-stream"), "mail-attachment-symbolic");
        assert_eq!(icon_name(""), "mail-attachment-symbolic");
    }

    #[test]
    fn a_row_shows_the_files_title() {
        assert_eq!(title(&named("  Agenda.pdf ", "application/pdf")), "Agenda.pdf");
    }

    #[test]
    fn a_waiting_file_without_a_title_shows_its_file_name() {
        let waiting = Attachment { waiting: Some("/home/me/Slides/Plan.odp".into()), ..Attachment::default() };
        assert_eq!(title(&waiting), "Plan.odp");
    }

    #[test]
    fn a_file_with_no_name_at_all_says_so() {
        assert_eq!(title(&Attachment::default()), "Untitled file");
    }

    #[test]
    fn an_upload_that_went_through_is_linked() {
        let file = named("Agenda.pdf", "application/pdf");
        assert_eq!(uploaded(Ok(Permitted::Done(file.clone()))), Uploaded::Done(file));
    }

    #[test]
    fn an_upload_without_a_network_waits_for_the_queue() {
        let offline = SyncError::Backend(BackendError::Offline("no route".into()));
        assert_eq!(uploaded(Err(offline)), Uploaded::Later);
        let limited = SyncError::Backend(BackendError::RateLimited(None));
        assert_eq!(uploaded(Err(limited)), Uploaded::Later);
    }

    #[test]
    fn an_upload_without_drive_asks_for_it() {
        assert_eq!(uploaded(Ok(Permitted::NeedsPermission)), Uploaded::NeedsPermission);
    }

    #[test]
    fn a_refused_upload_says_why() {
        let refused = SyncError::Backend(BackendError::Refused("the file is too large".into()));
        assert_eq!(uploaded(Err(refused)), Uploaded::Failed("refused: the file is too large".into()));
    }

    fn stuck(problem: UploadProblem) -> Attachment {
        Attachment { waiting: Some("/home/me/Plan.odp".into()), problem: Some(problem), ..named("Plan.odp", "") }
    }

    #[test]
    fn a_file_waiting_for_drive_access_says_so() {
        assert_eq!(note(&stuck(UploadProblem::NeedsAccess)).as_deref(), Some("Waiting for access"));
    }

    #[test]
    fn a_file_that_moved_says_it_is_not_found() {
        assert_eq!(note(&stuck(UploadProblem::NotFound)).as_deref(), Some("File not found"));
    }

    #[test]
    fn a_file_drive_refused_says_why() {
        let refused = stuck(UploadProblem::Refused("the file is too large".into()));
        assert_eq!(note(&refused).as_deref(), Some("Could not upload: the file is too large"));
    }

    fn uploaded_by_us(share: bool) -> Attachment {
        Attachment { share: Some(share), file_id: "1abc".into(), ..named("Plan.odp", "") }
    }

    #[test]
    fn a_shared_file_tells_the_organizer_the_guests_can_open_it() {
        assert_eq!(share_note(&uploaded_by_us(true), true).as_deref(), Some("Guests can open this file"));
    }

    #[test]
    fn an_unshared_file_says_the_guests_cannot() {
        assert_eq!(share_note(&uploaded_by_us(false), true).as_deref(), Some("Not shared with the guests"));
    }

    #[test]
    fn sharing_says_nothing_without_guests_or_for_someone_elses_file() {
        assert_eq!(share_note(&uploaded_by_us(true), false), None);
        assert_eq!(share_note(&named("Plan.odp", ""), true), None);
    }

    #[test]
    fn a_file_on_drive_has_no_second_line() {
        assert_eq!(note(&named("Agenda.pdf", "application/pdf")), None);
    }

    #[test]
    fn a_waiting_file_says_when_it_uploads() {
        let waiting = Attachment { waiting: Some("/home/me/Plan.odp".into()), ..named("Plan.odp", "") };
        assert_eq!(note(&waiting).as_deref(), Some("Uploads when you are back online"));
    }
}
