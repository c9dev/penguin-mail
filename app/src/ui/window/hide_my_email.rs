//! Opening the Hide My Email dialog. The addresses themselves are the
//! app's: see `crate::app::hidden`.

use std::rc::Rc;

use mailrs_domain::AccountId;

use super::MainWindow;

impl MainWindow {
    /// Opens the Hide My Email dialog, with `account_id` chosen for new
    /// addresses when given.
    pub(super) fn show_hide_my_email(self: &Rc<Self>, account_id: Option<AccountId>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let weak = Rc::downgrade(self);
        crate::ui::hide_my_email::present(&app, &self.window, account_id, move |email| {
            if let Some(win) = weak.upgrade() {
                win.authorize(Some(email));
            }
        });
    }
}
