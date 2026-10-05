//! The provider tiles' grid: three to a row where three fit, as the
//! approved desktop layout has them, and two to a row in a narrower
//! window, each row centred. The choice comes from the width the grid is
//! given and the tiles' own measured width, so no breakpoint has to guess
//! either.

use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::*;

use crate::add_account::post;

/// The space between two tiles, across and down.
const GAP: i32 = 12;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TileGrid;

    #[glib::object_subclass]
    impl ObjectSubclass for TileGrid {
        const NAME: &'static str = "MailrsTileGrid";
        type Type = super::TileGrid;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for TileGrid {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for TileGrid {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let obj = self.obj();
            let tiles = obj.tiles();
            let tile = obj.tile_width();
            let across = |per_row: usize| {
                let count = tiles.len().min(per_row) as i32;
                (count * tile + (count - 1).max(0) * GAP).max(0)
            };
            match orientation {
                gtk::Orientation::Horizontal => (across(2), across(3), -1, -1),
                _ => {
                    let per_row = if for_size < 0 {
                        3
                    } else {
                        post::tiles_per_row(for_size, tile, GAP)
                    };
                    let rows = post::tile_rows(tiles.len(), per_row).len() as i32;
                    let height = rows * obj.tile_height(tile) + (rows - 1).max(0) * GAP;
                    (height, height, -1, -1)
                }
            }
        }

        fn size_allocate(&self, width: i32, _height: i32, _baseline: i32) {
            let obj = self.obj();
            let tiles = obj.tiles();
            let tile = obj.tile_width();
            let tall = obj.tile_height(tile);
            let mut rest = tiles.iter();
            let mut y = 0;
            for count in post::tile_rows(tiles.len(), post::tiles_per_row(width, tile, GAP)) {
                let row = count as i32 * tile + (count as i32 - 1) * GAP;
                let mut x = (width - row) / 2;
                for child in rest.by_ref().take(count) {
                    child.size_allocate(&gtk::Allocation::new(x, y, tile, tall), -1);
                    x += tile + GAP;
                }
                y += tall + GAP;
            }
        }
    }
}

glib::wrapper! {
    pub struct TileGrid(ObjectSubclass<imp::TileGrid>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TileGrid {
    /// A grid of `tiles`, in their order, which is also the order Tab and
    /// a screen reader meet them in.
    pub fn new(tiles: &[gtk::Button]) -> TileGrid {
        let grid: TileGrid = glib::Object::builder().build();
        for tile in tiles {
            tile.set_parent(&grid);
        }
        grid
    }

    fn tiles(&self) -> Vec<gtk::Widget> {
        let mut tiles = Vec::new();
        let mut child = self.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if widget.is_visible() {
                tiles.push(widget);
            }
        }
        tiles
    }

    /// The widest tile's natural width, which every tile takes.
    fn tile_width(&self) -> i32 {
        self.tiles()
            .iter()
            .map(|tile| tile.measure(gtk::Orientation::Horizontal, -1).1)
            .max()
            .unwrap_or(0)
    }

    /// The tallest tile's natural height at `width`, which every row takes.
    fn tile_height(&self, width: i32) -> i32 {
        self.tiles()
            .iter()
            .map(|tile| tile.measure(gtk::Orientation::Vertical, width).1)
            .max()
            .unwrap_or(0)
    }
}
