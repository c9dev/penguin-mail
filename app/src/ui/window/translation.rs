//! The language the interface is in, which the translation card measures
//! a message against. The offer and the translation are steps of the
//! thread run, in `crate::open_thread::run::translation`.

use std::rc::Rc;

use gtk::glib;
use mailrs_domain::translate::{fill, gettext};

use super::MainWindow;
use crate::settings::Change;
use crate::translation::{self, Language};
use crate::ui::composer::spell;

impl MainWindow {
    /// The language the interface is in, when it is one whose words this
    /// app can count. `None` leaves every message alone.
    pub(super) fn interface_language(&self) -> Option<Language> {
        let installed: Vec<String> = crate::language::choices()
            .into_iter()
            .map(|language| language.code)
            .collect();
        translation::interface_language(
            &self.settings_with(|s| s.language.clone()),
            &spell::locale_language(),
            &installed,
        )
    }

    /// Stops offering to translate messages in `language`, and says so on
    /// a toast whose Undo starts again. Preferences lists the languages
    /// too, for a later change of mind.
    pub(super) fn never_translate(self: &Rc<Self>, language: &'static str) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let set = move |never| Change::NeverTranslate {
            language: language.to_string(),
            never,
        };
        app.change_settings(set(true));
        let name = translation::named(language)
            .map(|language| language.name())
            .unwrap_or_default();
        let said = fill(
            &gettext("Messages in {language} will no longer offer a translation"),
            &[("language", &name)],
        );
        let toast = adw::Toast::builder()
            .title(glib::markup_escape_text(&said))
            .button_label(gettext("Undo"))
            .timeout(6)
            .build();
        let weak = Rc::downgrade(&app);
        toast.connect_button_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.change_settings(set(false));
            }
        });
        self.toasts.add_toast(toast);
    }
}
