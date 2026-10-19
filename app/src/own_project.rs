//! The setup that lets a person use their own Google project.

use mailrs_domain::translate::{fill, gettext};
use mailrs_gmail::GmailError;
use std::path::{Path, PathBuf};

use gtk::{gio, glib, prelude::*};
use mailrs_sync::sign_in::own_client::{FILE_LIMIT, FileProblem, OwnClient, read_file};
use mailrs_sync::sign_in::{BrowserSignInError, ClientSource};

use crate::core::BrowserTimedOut;

/// The instructions with screenshots, on the website.
pub const HELP: &str = "https://penguin-mail.com/help/gmail-own-project";

/// Google Cloud's page of the project's keys, for a project whose id is
/// not known.
pub const PROJECT_PAGE: &str = "https://console.cloud.google.com/auth/clients";

/// A page of the setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Intro,
    Project,
    Apis,
    Screen,
    Publish,
    Key,
    File,
    Warning,
}

impl Step {
    /// The number the person sees, 1 to 6, for the six steps.
    pub fn number(self) -> Option<u8> {
        match self {
            Step::Project => Some(1),
            Step::Apis => Some(2),
            Step::Screen => Some(3),
            Step::Publish => Some(4),
            Step::Key => Some(5),
            Step::File => Some(6),
            Step::Intro | Step::Warning => None,
        }
    }

    /// The page tag in the navigation view.
    pub fn tag(self) -> &'static str {
        match self {
            Step::Intro => "own-intro",
            Step::Project => "own-1",
            Step::Apis => "own-2",
            Step::Screen => "own-3",
            Step::Publish => "own-4",
            Step::Key => "own-5",
            Step::File => "own-file",
            Step::Warning => "own-warning",
        }
    }

    /// The page after this one, or `None` after the last.
    pub fn next(self) -> Option<Step> {
        Some(match self {
            Step::Intro => Step::Project,
            Step::Project => Step::Apis,
            Step::Apis => Step::Screen,
            Step::Screen => Step::Publish,
            Step::Publish => Step::Key,
            Step::Key => Step::File,
            Step::File => Step::Warning,
            Step::Warning => return None,
        })
    }

    /// The step with this number, 1 to 6.
    pub fn of_number(n: u8) -> Option<Step> {
        match n {
            1 => Some(Step::Project),
            2 => Some(Step::Apis),
            3 => Some(Step::Screen),
            4 => Some(Step::Publish),
            5 => Some(Step::Key),
            6 => Some(Step::File),
            _ => None,
        }
    }
}

/// Where the setup opens for the step saved in `config.toml`. Nothing
/// saved, or 0 from a hand edit, opens the intro; a number past 6 opens
/// the last step.
pub fn resume_at(saved: Option<u8>) -> Step {
    match saved {
        None | Some(0) => Step::Intro,
        Some(n) => Step::of_number(n).unwrap_or(Step::File),
    }
}

/// Google Cloud's page of the keys in the project `project_id` names, or
/// of whichever project Google last had open when the id is unknown or
/// not one Google makes.
pub fn project_page(project_id: Option<&str>) -> String {
    match project_id {
        Some(id)
            if !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') =>
        {
            format!("{PROJECT_PAGE}?project={id}")
        }
        _ => PROJECT_PAGE.to_string(),
    }
}

/// The help for `step`: its own part of the page for the six steps.
pub fn help_url(step: Step) -> String {
    match step.number() {
        Some(n) => format!("{HELP}#step-{n}"),
        None => HELP.to_string(),
    }
}

/// The Google Cloud page a step opens, for the five steps done there.
pub fn console_url(step: Step) -> Option<&'static str> {
    Some(match step {
        Step::Project => "https://console.cloud.google.com/projectcreate",
        Step::Apis => {
            "https://console.cloud.google.com/flows/enableapi?apiid=gmail.googleapis.com,calendar-json.googleapis.com,people.googleapis.com,drive.googleapis.com"
        }
        Step::Screen => "https://console.cloud.google.com/auth/overview",
        Step::Publish => "https://console.cloud.google.com/auth/audience",
        Step::Key => "https://console.cloud.google.com/auth/clients/create",
        Step::Intro | Step::File | Step::Warning => return None,
    })
}

/// The four APIs the account uses, each with its page in Google Cloud's
/// library: the fallback for the one link of step 2 if Google changes it.
pub const API_LIBRARY: [(&str, &str); 4] = [
    (
        "Gmail API",
        "https://console.cloud.google.com/apis/library/gmail.googleapis.com",
    ),
    (
        "Google Calendar API",
        "https://console.cloud.google.com/apis/library/calendar-json.googleapis.com",
    ),
    (
        "People API",
        "https://console.cloud.google.com/apis/library/people.googleapis.com",
    ),
    (
        "Google Drive API",
        "https://console.cloud.google.com/apis/library/drive.googleapis.com",
    ),
];

/// What a page of the setup says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Words {
    pub heading: String,
    /// The line under the heading, plain text.
    pub lede: Option<String>,
    /// What to do, in order, as Pango markup: Google's labels in `<b>`,
    /// what to type in `<tt>`. The intro's paragraphs are plain.
    pub checklist: Vec<String>,
    /// Plain lines under the list.
    pub notes: Vec<String>,
}

/// "Step N of 6" over the six steps' headings.
pub fn caption(step: Step) -> Option<String> {
    let number = step.number()?.to_string();
    Some(fill(&gettext("Step {number} of 6"), &[("number", &number)]))
}

/// The words of `step`'s page.
pub fn words(step: Step) -> Words {
    let open = || {
        // Translators: Google is a name; keep it as it is. "Open Google Cloud" is the button above this line.
        gettext("Open Google Cloud, then follow the list.")
    };
    let lede = |first: String| Some(format!("{first} {}", open()));
    match step {
        Step::Intro => Words {
            // Translators: Google is a name; keep it as it is.
            heading: gettext("Your own Google project"),
            lede: None,
            checklist: vec![
                // Translators: Google, Penguin Mail and Gmail are names; keep them as they are.
                gettext(
                    "A Google project of your own gives Penguin Mail every Gmail feature. It's free, just for you, takes about 10 minutes, and you do it once.",
                ),
                // Translators: Google and Penguin Mail are names; keep them as they are.
                gettext(
                    "You'll use the Google account you want to read mail from. Penguin Mail opens each page for you and says what to click.",
                ),
            ],
            notes: Vec::new(),
        },
        Step::Project => Words {
            heading: gettext("Make a project"),
            // Translators: Google and Penguin Mail are names; keep them as they are.
            lede: lede(gettext(
                "Google keeps what Penguin Mail needs in a project of its own.",
            )),
            checklist: vec![
                gettext(
                    "If Google asks you to agree to its terms first, choose your country and agree.",
                ),
                // This is a button on Google's page; use the words Google's page shows in your language.
                gettext("In <b>Project name</b>, type <tt>Penguin Mail</tt>."),
                // This is a button on Google's page; use the words Google's page shows in your language.
                gettext("Leave <b>Location</b> as it is and click <b>Create</b>."),
                gettext("Wait for the bell icon to say the project is ready."),
            ],
            notes: Vec::new(),
        },
        Step::Apis => Words {
            // Translators: Gmail is a name; keep it as it is.
            heading: gettext("Turn on Gmail, Calendar and Contacts"),
            // Translators: Penguin Mail is a name; keep it as it is.
            lede: lede(gettext(
                "Penguin Mail reads your mail, calendar and contacts through these.",
            )),
            checklist: vec![
                // This is a button on Google's page; use the words Google's page shows in your language.
                gettext(
                    "Check that the project at the top says <b>Penguin Mail</b>. If not, click it and pick Penguin Mail.",
                ),
                // This is a button on Google's page; use the words Google's page shows in your language.
                gettext("Click <b>Next</b>, then <b>Enable</b>."),
            ],
            notes: Vec::new(),
        },
        Step::Screen => Words {
            heading: gettext("Name the sign-in screen"),
            // Translators: Google is a name; keep it as it is.
            lede: lede(gettext("Google shows this name when you sign in.")),
            checklist: vec![
                // This is a button on Google's page; use the words Google's page shows in your language.
                gettext("Click <b>Get started</b>."),
                // This is a button on Google's page; use the words Google's page shows in your language.
                gettext(
                    "<b>App name</b>: <tt>Penguin Mail</tt>. <b>User support email</b>: your address.",
                ),
                // This is a button on Google's page; use the words Google's page shows in your language.
                gettext(
                    "<b>Audience</b>: choose <b>External</b>. (A work or school account with an organization may see <b>Internal</b>; that also works and needs no warning screen later. Pick it if offered.)",
                ),
                // This is a button on Google's page; use the words Google's page shows in your language.
                gettext("<b>Contact information</b>: your address again."),
                // This is a button on Google's page; use the words Google's page shows in your language.
                gettext("Tick the agreement and click <b>Create</b>."),
            ],
            notes: Vec::new(),
        },
        Step::Publish => Words {
            heading: gettext("Publish it"),
            lede: lede(gettext("Publishing keeps you signed in.")),
            checklist: vec![
                // This is a button on Google's page; use the words Google's page shows in your language.
                gettext(
                    "Under <b>Publishing status</b>, click <b>Publish app</b>, then <b>Confirm</b>.",
                ),
                // This is a button on Google's page; use the words Google's page shows in your language.
                gettext("It should now say <b>In production</b>."),
            ],
            notes: vec![
                // Translators: Google and Penguin Mail are names; keep them as they are.
                gettext("If you skip this, Google signs you out of Penguin Mail every 7 days."),
                // This is a button on Google's page; use the words Google's page shows in your language.
                gettext("Chose Internal? Then there's nothing to do here."),
            ],
        },
        Step::Key => Words {
            // Translators: Penguin Mail is a name; keep it as it is.
            heading: gettext("Make the key for Penguin Mail"),
            // Translators: Penguin Mail is a name; keep it as it is.
            lede: lede(gettext(
                "The key lets Penguin Mail sign in through your project.",
            )),
            checklist: vec![
                // This is a button on Google's page; use the words Google's page shows in your language.
                gettext("<b>Application type</b>: choose <b>Desktop app</b>."),
                // This is a button on Google's page; use the words Google's page shows in your language.
                gettext("<b>Name</b>: <tt>Penguin Mail</tt>, then <b>Create</b>."),
                // This is a button on Google's page; use the words Google's page shows in your language.
                gettext("On the next screen, click <b>Download JSON</b> before you close it."),
            ],
            notes: vec![
                // Translators: Google is a name; keep it as it is.
                gettext(
                    "Google shows this only once. If you closed it already, delete that key and make a new one.",
                ),
            ],
        },
        Step::File => Words {
            // Translators: Penguin Mail is a name; keep it as it is.
            heading: gettext("Give the file to Penguin Mail"),
            lede: None,
            checklist: Vec::new(),
            notes: Vec::new(),
        },
        Step::Warning => Words {
            // Translators: Google is a name; keep it as it is.
            heading: gettext("Sign in with Google"),
            lede: None,
            checklist: vec![
                // This is a button on Google's page; use the words Google's page shows in your language.
                gettext(
                    "Google will say <b>\"Google hasn't verified this app\"</b>. That's expected: the app is your own project, and you're the only one using it. Click <b>Advanced</b>, then <b>Go to Penguin Mail (unsafe)</b>. Then tick every box so every feature works.",
                ),
            ],
            notes: Vec::new(),
        },
    }
}

/// What a failed sign-in through the person's own project says, and where
/// it leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub words: String,
    /// The step that fixes it.
    pub back_to: Option<Step>,
    /// Google's page that fixes it.
    pub open: Option<String>,
}

/// The words for `err`, a sign-in through a client from `source` that
/// failed, when the person's own project explains it. A sign-in through
/// the built-in client or the environment's keeps the error's own words.
pub fn problem(err: &anyhow::Error, source: ClientSource) -> Option<Problem> {
    if !matches!(source, ClientSource::Setup | ClientSource::Kept) {
        return None;
    }
    let publish = |said: String| Problem {
        words: format!(
            "{said} {}",
            // This is a button on Google's page; use the words Google's page shows in your language.
            gettext("Did you publish your project? If it still says Testing, go to step 4.")
        ),
        back_to: Some(Step::Publish),
        open: None,
    };
    if err.downcast_ref::<BrowserTimedOut>().is_some() {
        return Some(publish(err.to_string()));
    }
    let Some(BrowserSignInError::Google(google)) = err.downcast_ref::<BrowserSignInError>() else {
        return None;
    };
    match google {
        GmailError::OAuth(reason) if reason.ends_with("access_denied") => Some(publish(fill(
            &gettext("You did not allow Penguin Mail on {provider}'s page."),
            &[("provider", "Google")],
        ))),
        GmailError::Http { status, body }
            if matches!(status, 400 | 401)
                && (body.contains("invalid_client") || body.contains("deleted_client")) =>
        {
            Some(Problem {
                // Translators: Google is a name; keep it as it is.
                words: gettext(
                    "Google doesn't recognize this key. Make a new one and download it again.",
                ),
                back_to: Some(Step::Key),
                open: None,
            })
        }
        GmailError::ApiDisabled {
            service,
            enable_url,
        } => api_off_words(service).map(|words| Problem {
            words,
            back_to: None,
            open: Some(enable_url.clone()),
        }),
        _ => None,
    }
}

/// What to say when Google names `service` as turned off in the person's
/// project, for the four services Penguin Mail uses.
pub fn api_off_words(service: &str) -> Option<String> {
    let service = service.to_lowercase();
    let has = |word: &str| service.contains(word);
    Some(if has("gmail") {
        // Translators: Gmail is a name; keep it as it is.
        gettext("Gmail is turned off in your project.")
    } else if has("calendar") {
        gettext("Calendar is turned off in your project.")
    } else if has("people") || has("contacts") {
        gettext("Contacts is turned off in your project.")
    } else if has("drive") {
        gettext("Drive is turned off in your project.")
    } else {
        return None;
    })
}

/// The file to read from a drop: the first one named `.json`, in any
/// case. A drop of text arrives as no paths and reads nothing.
pub fn dropped(paths: &[PathBuf]) -> Option<&Path> {
    paths
        .iter()
        .find(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        })
        .map(PathBuf::as_path)
}

/// The client in the key `file` holds. Reads at most one byte past
/// [`FILE_LIMIT`], so a large file is refused without being read whole.
pub async fn read_key_file(file: &gio::File) -> Result<OwnClient, FileProblem> {
    let bytes = read_head(file, FILE_LIMIT + 1).await.map_err(|err| {
        tracing::info!(error = %err, "could not read the key file");
        FileProblem::Unreadable
    })?;
    read_file(&bytes)
}

/// The first `limit` bytes of `file`, or all of a shorter one.
async fn read_head(file: &gio::File, limit: usize) -> Result<Vec<u8>, glib::Error> {
    let stream = file.read_future(glib::Priority::DEFAULT).await?;
    let mut bytes = Vec::new();
    while bytes.len() < limit {
        let chunk = stream
            .read_bytes_future(limit - bytes.len(), glib::Priority::DEFAULT)
            .await?;
        if chunk.is_empty() {
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// The body of the question that offers to turn `service` on: the
/// words for the person's own project when the service is one Penguin
/// Mail uses, else what Google named.
pub fn api_off_body(service: &str) -> String {
    api_off_words(service).unwrap_or_else(|| {
        fill(
            &gettext(
                "The Google Cloud project Penguin Mail signs in with has the {service} \
                 switched off, so Google refuses before it can ask for your permission. \
                 Turn it on, wait a minute, and try again.",
            ),
            &[("service", service)],
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::BrowserTimedOut;
    use mailrs_gmail::GmailError;
    use mailrs_sync::sign_in::BrowserSignInError;

    const ALL: [Step; 8] = [
        Step::Intro,
        Step::Project,
        Step::Apis,
        Step::Screen,
        Step::Publish,
        Step::Key,
        Step::File,
        Step::Warning,
    ];

    fn google(err: GmailError) -> anyhow::Error {
        BrowserSignInError::Google(err).into()
    }

    fn denied() -> anyhow::Error {
        google(GmailError::OAuth("Google returned access_denied".into()))
    }

    #[test]
    fn the_library_fallback_covers_each_api_of_the_one_link() {
        let one = console_url(Step::Apis).unwrap();
        for (_, url) in API_LIBRARY {
            let id = url.rsplit('/').next().unwrap();
            assert!(one.contains(id), "{id}");
        }
    }

    #[test]
    fn a_denied_consent_through_an_own_project_points_to_step_4() {
        let found = problem(&denied(), ClientSource::Setup).unwrap();
        assert_eq!(found.back_to, Some(Step::Publish));
        assert_eq!(
            found.words,
            "You did not allow Penguin Mail on Google's page. Did you publish your project? If it still says Testing, go to step 4."
        );
        assert_eq!(found.open, None);
    }

    #[test]
    fn a_timeout_through_an_own_project_points_to_step_4() {
        let found = problem(&anyhow::Error::new(BrowserTimedOut), ClientSource::Kept).unwrap();
        assert_eq!(found.back_to, Some(Step::Publish));
        assert_eq!(
            found.words,
            "Gave up waiting for the browser after five minutes. Did you publish your project? If it still says Testing, go to step 4."
        );
    }

    #[test]
    fn a_denied_consent_through_the_built_in_client_keeps_its_words() {
        assert_eq!(problem(&denied(), ClientSource::BuiltIn), None);
    }

    #[test]
    fn a_denied_consent_through_the_environment_keeps_its_words() {
        assert_eq!(problem(&denied(), ClientSource::Environment), None);
    }

    #[test]
    fn an_unknown_key_points_to_step_5() {
        let body =
            r#"{"error":"invalid_client","error_description":"The OAuth client was not found."}"#;
        let err = google(GmailError::Http {
            status: 401,
            body: body.into(),
        });
        let found = problem(&err, ClientSource::Setup).unwrap();
        assert_eq!(
            found.words,
            "Google doesn't recognize this key. Make a new one and download it again."
        );
        assert_eq!(found.back_to, Some(Step::Key));
    }

    #[test]
    fn a_deleted_key_points_to_step_5() {
        let body =
            r#"{"error":"deleted_client","error_description":"The OAuth client was deleted."}"#;
        let err = google(GmailError::Http {
            status: 400,
            body: body.into(),
        });
        let found = problem(&err, ClientSource::Kept).unwrap();
        assert_eq!(
            found.words,
            "Google doesn't recognize this key. Make a new one and download it again."
        );
        assert_eq!(found.back_to, Some(Step::Key));
        assert_eq!(found.open, None);
    }

    #[test]
    fn another_refusal_through_an_own_project_keeps_its_words() {
        let err = google(GmailError::Http {
            status: 500,
            body: "invalid_client".into(),
        });
        assert_eq!(problem(&err, ClientSource::Setup), None);
    }

    #[test]
    fn gmail_off_in_the_project_opens_googles_page() {
        let url = "https://console.developers.google.com/apis/api/gmail.googleapis.com/overview?project=4711";
        let off = GmailError::ApiDisabled {
            service: "Gmail API".into(),
            enable_url: url.into(),
        };
        let found = problem(&google(off), ClientSource::Setup).unwrap();
        assert_eq!(found.words, "Gmail is turned off in your project.");
        assert_eq!(found.open.as_deref(), Some(url));
        assert_eq!(found.back_to, None);
    }

    #[test]
    fn each_service_off_has_its_own_words() {
        assert_eq!(
            api_off_words("Google Calendar API").as_deref(),
            Some("Calendar is turned off in your project.")
        );
        assert_eq!(
            api_off_words("People API").as_deref(),
            Some("Contacts is turned off in your project.")
        );
        assert_eq!(
            api_off_words("Google Drive API").as_deref(),
            Some("Drive is turned off in your project.")
        );
        assert_eq!(
            api_off_words("Gmail API").as_deref(),
            Some("Gmail is turned off in your project.")
        );
        assert_eq!(api_off_words("Cloud Pub/Sub API"), None);
    }

    #[test]
    fn the_service_off_question_names_the_service_alone() {
        assert_eq!(
            api_off_body("People API"),
            "Contacts is turned off in your project."
        );
        assert!(api_off_body("Cloud Pub/Sub API").contains("Cloud Pub/Sub API"));
    }

    #[test]
    fn every_step_names_a_console_page_but_the_last() {
        assert_eq!(
            console_url(Step::Project),
            Some("https://console.cloud.google.com/projectcreate")
        );
        assert_eq!(
            console_url(Step::Apis),
            Some(
                "https://console.cloud.google.com/flows/enableapi?apiid=gmail.googleapis.com,calendar-json.googleapis.com,people.googleapis.com,drive.googleapis.com"
            )
        );
        assert_eq!(
            console_url(Step::Screen),
            Some("https://console.cloud.google.com/auth/overview")
        );
        assert_eq!(
            console_url(Step::Publish),
            Some("https://console.cloud.google.com/auth/audience")
        );
        assert_eq!(
            console_url(Step::Key),
            Some("https://console.cloud.google.com/auth/clients/create")
        );
        for step in [Step::Intro, Step::File, Step::Warning] {
            assert_eq!(console_url(step), None, "{step:?}");
        }
        for url in ALL.iter().filter_map(|step| console_url(*step)) {
            assert!(!url.contains("authuser"), "{url}");
        }
    }

    #[test]
    fn each_step_links_to_its_own_help() {
        assert_eq!(help_url(Step::Intro), HELP);
        assert_eq!(help_url(Step::Warning), HELP);
        assert_eq!(help_url(Step::Project), format!("{HELP}#step-1"));
        assert_eq!(help_url(Step::File), format!("{HELP}#step-6"));
    }

    #[test]
    fn the_six_steps_have_a_caption() {
        assert_eq!(caption(Step::Project).as_deref(), Some("Step 1 of 6"));
        assert_eq!(caption(Step::File).as_deref(), Some("Step 6 of 6"));
        assert_eq!(caption(Step::Intro), None);
        assert_eq!(caption(Step::Warning), None);
    }

    #[test]
    fn each_step_has_its_heading_and_list() {
        let screen = words(Step::Screen);
        assert_eq!(screen.heading, "Name the sign-in screen");
        assert_eq!(screen.checklist.len(), 5);
        assert_eq!(screen.checklist[0], "Click <b>Get started</b>.");
        assert_eq!(words(Step::Project).checklist.len(), 4);
        assert_eq!(words(Step::Apis).checklist.len(), 2);
        assert_eq!(words(Step::Publish).notes.len(), 2);
        assert_eq!(
            words(Step::Key).notes,
            [
                "Google shows this only once. If you closed it already, delete that key and make a new one."
            ]
        );
        assert!(words(Step::File).checklist.is_empty());
        assert_eq!(words(Step::Warning).checklist.len(), 1);
    }

    #[test]
    fn every_checklist_item_is_valid_markup() {
        for step in ALL {
            for item in words(step).checklist {
                assert!(
                    gtk::pango::parse_markup(&item, '\0').is_ok(),
                    "{step:?}: {item}"
                );
            }
        }
    }

    /// `text` without its tags.
    fn plain(text: &str) -> String {
        let mut out = String::new();
        let mut inside = false;
        for c in text.chars() {
            match c {
                '<' => inside = true,
                '>' => inside = false,
                c if !inside => out.push(c),
                _ => {}
            }
        }
        out
    }

    #[test]
    fn no_step_says_the_forbidden_words() {
        let forbidden = [
            "OAuth",
            "API",
            "scope",
            "credentials",
            "verif",
            "CASA",
            "free",
            "client",
        ];
        for step in ALL {
            let shown = words(step);
            let mut texts = vec![shown.heading.clone()];
            texts.extend(shown.lede.clone());
            texts.extend(shown.checklist.iter().map(|item| plain(item)));
            texts.extend(shown.notes.clone());
            for text in texts {
                // Google's own warning names itself; the page quotes it.
                let text = text.replace("Google hasn't verified this app", "");
                // The intro says what the project costs, once.
                let text = if step == Step::Intro {
                    text.replace("It's free", "")
                } else {
                    text
                };
                for word in forbidden {
                    assert!(
                        !text.to_lowercase().contains(&word.to_lowercase()),
                        "{step:?} says {word:?}: {text}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_drop_reads_its_first_json_file() {
        let paths = [
            PathBuf::from("/d/notes.txt"),
            PathBuf::from("/d/client_secret_123.JSON"),
            PathBuf::from("/d/b.json"),
        ];
        assert_eq!(
            dropped(&paths),
            Some(Path::new("/d/client_secret_123.JSON"))
        );
    }

    #[test]
    fn a_drop_without_a_json_file_reads_nothing() {
        assert_eq!(dropped(&[PathBuf::from("/d/notes.txt")]), None);
    }

    #[test]
    fn a_drop_of_text_reads_nothing() {
        // Text, such as a link dragged from a browser's download bar,
        // reaches the page as no paths at all.
        assert_eq!(dropped(&[]), None);
    }

    const DESKTOP_KEY: &str = r#"{"installed":{"client_id":"123-abc.apps.googleusercontent.com","project_id":"penguin-mail-4711","client_secret":"GOCSPX-abc"}}"#;

    #[test]
    fn a_key_file_reads_into_its_client() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client_secret.json");
        std::fs::write(&path, DESKTOP_KEY).unwrap();
        let file = gtk::gio::File::for_path(&path);
        let read = gtk::glib::MainContext::new().block_on(read_key_file(&file));
        let Ok(own) = read else {
            panic!("the key file did not read");
        };
        assert_eq!(own.id(), "123-abc.apps.googleusercontent.com");
        assert_eq!(own.project_id(), Some("penguin-mail-4711"));
    }

    #[test]
    fn a_large_file_is_never_read_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.json");
        // A valid key followed by spaces past the limit: read whole, it
        // would parse.
        let mut big = DESKTOP_KEY.to_string();
        big.push_str(&" ".repeat(FILE_LIMIT * 2));
        std::fs::write(&path, big).unwrap();
        let file = gtk::gio::File::for_path(&path);
        let head = gtk::glib::MainContext::new().block_on(read_head(&file, FILE_LIMIT + 1));
        assert_eq!(head.map(|bytes| bytes.len()).ok(), Some(FILE_LIMIT + 1));
        let read = gtk::glib::MainContext::new().block_on(read_key_file(&file));
        assert_eq!(read.err(), Some(FileProblem::NotTheFile));
    }

    #[test]
    fn a_file_that_cannot_be_read_says_so() {
        let file = gtk::gio::File::for_path("/nonexistent/penguin/key.json");
        let read = gtk::glib::MainContext::new().block_on(read_key_file(&file));
        assert_eq!(read.err(), Some(FileProblem::Unreadable));
    }

    #[test]
    fn the_project_link_opens_the_persons_own_project() {
        assert_eq!(
            project_page(Some("penguin-mail-4711")),
            "https://console.cloud.google.com/auth/clients?project=penguin-mail-4711"
        );
        assert_eq!(project_page(None), PROJECT_PAGE);
        assert_eq!(project_page(Some("a b&c")), PROJECT_PAGE);
    }

    #[test]
    fn next_leads_through_the_steps_in_order() {
        assert_eq!(Step::Intro.next(), Some(Step::Project));
        assert_eq!(Step::Project.next(), Some(Step::Apis));
        assert_eq!(Step::Publish.next(), Some(Step::Key));
        assert_eq!(Step::Key.next(), Some(Step::File));
        assert_eq!(Step::File.next(), Some(Step::Warning));
        assert_eq!(Step::Warning.next(), None);
    }

    #[test]
    fn nothing_saved_opens_the_intro() {
        assert_eq!(resume_at(None), Step::Intro);
    }

    #[test]
    fn a_saved_step_opens_that_step() {
        assert_eq!(resume_at(Some(4)), Step::Publish);
    }

    #[test]
    fn step_zero_opens_the_intro() {
        assert_eq!(resume_at(Some(0)), Step::Intro);
    }

    #[test]
    fn a_step_past_six_opens_the_file_step() {
        assert_eq!(resume_at(Some(9)), Step::File);
    }

    #[test]
    fn every_step_has_its_own_tag() {
        let all = [
            Step::Intro,
            Step::Project,
            Step::Apis,
            Step::Screen,
            Step::Publish,
            Step::Key,
            Step::File,
            Step::Warning,
        ];
        let tags: std::collections::HashSet<_> = all.iter().map(|s| s.tag()).collect();
        assert_eq!(tags.len(), all.len());
        assert_eq!(Step::Intro.tag(), "own-intro");
        assert_eq!(Step::Key.tag(), "own-5");
        assert_eq!(Step::Warning.tag(), "own-warning");
    }
}
