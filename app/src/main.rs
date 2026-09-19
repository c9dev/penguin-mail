mod compose;
mod core;
mod demo;
mod diff;
mod format;
mod render;
mod sanitize;

fn main() {
    let _ = core::Core::open(true);
}
