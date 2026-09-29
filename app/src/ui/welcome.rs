//! The first-run page: the band and the provider tiles across the empty
//! window, to add the first account. A tile opens Add Account on its
//! step: the browser sign-in for Google, the address for the rest.

use adw::prelude::*;
use mailrs_domain::translate::gettext;

use crate::add_account::post::{Band, Tile};
use crate::ui::add_account::tiles;
use crate::ui::post_band::PostBand;

/// The band's height across the window, with the art at 1.25 times its
/// size in the dialog.
const BAND: i32 = 250;
const SCALE: f32 = 1.25;

/// Offers to add the first account from one of the tiles.
pub fn first_account_page(on_tile: impl Fn(Tile) + 'static) -> gtk::Widget {
    let band = PostBand::new(BAND, SCALE);
    band.show(Band::Idle, None, None);
    let title = gtk::Label::builder()
        .label(gettext("Add your first account"))
        .wrap(true)
        .justify(gtk::Justification::Center)
        .accessible_role(gtk::AccessibleRole::Heading)
        .css_classes(["post-first-title"])
        .build();
    let lede = gtk::Label::builder()
        .label(gettext(
            "Choose where your mail lives. You can add more later from the main menu.",
        ))
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["post-first-lede"])
        .build();
    let grid = tiles(136);
    let on_tile = std::rc::Rc::new(on_tile);
    for (tile, button, _) in &grid.buttons {
        let (tile, on_tile) = (*tile, std::rc::Rc::clone(&on_tile));
        button.connect_clicked(move |_| on_tile(tile));
    }
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["post-first"])
        .build();
    column.append(&band.widget);
    column.append(&title);
    column.append(&lede);
    column.append(&grid.grid);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&column)
        .vexpand(true)
        .build();
    let header = adw::HeaderBar::builder()
        .title_widget(&gtk::Label::new(None))
        .css_classes(["post-band-header"])
        .build();
    let toolbar = adw::ToolbarView::builder()
        .extend_content_to_top_edge(true)
        .top_bar_style(adw::ToolbarStyle::Flat)
        .content(&scroller)
        .build();
    toolbar.add_top_bar(&header);
    toolbar.upcast()
}
