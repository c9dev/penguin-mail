//! The language the interface is in, which the translation card measures
//! a message against. The offer and the translation are steps of the
//! thread run, in `crate::open_thread::run::translation`.

use super::MainWindow;
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
            &self.settings().language,
            &spell::locale_language(),
            &installed,
        )
    }
}
