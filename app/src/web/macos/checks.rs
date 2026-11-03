//! WebKit checks run by the debug driver on the application's main thread.

use std::time::Duration;

use gtk::glib;

use super::*;
use crate::unsubscribe_page::Browser;

pub async fn check_macos(url: &str) {
    crate::web::frame::check().await;
    let held = Rc::new(RefCell::new(None));
    let received = Rc::clone(&held);
    let page = WebView::reading("mailrs-check", move |request| {
        *received.borrow_mut() = Some(request.clone());
    });
    page.load_html("<img src='mailrs-check:picture'>");
    for _ in 0..100 {
        if held.borrow().is_some() {
            break;
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
    assert!(held.borrow().is_some(), "WebKit issued the picture request");
    assert_eq!(page.0.bridge.ivars().tasks.borrow().len(), 1);
    drop(held.borrow_mut().take());
    assert!(
        page.0.bridge.ivars().tasks.borrow().is_empty(),
        "dropping the last clone removes its task"
    );

    let count = Rc::new(Cell::new(None));
    let counted = Rc::clone(&count);
    let finder = page.finder();
    finder.on_counted(move |found| counted.set(Some(found)));
    finder.count("", false);
    assert_eq!(count.get(), Some(0));

    resize_with_a_cover().await;

    let hidden = crate::unsubscribe_page::PageBrowser::new();
    let form = hidden.load(url).await.expect("the hidden fixture loads");
    assert!(
        form.text.contains("Pictures blocked"),
        "the image fails while scripts still run: {}",
        form.text
    );
    eprintln!("drive: empty Find, dropped scheme task and hidden image blocking passed");
}

async fn resize_with_a_cover() {
    let page = WebView::sealed();
    page.load_html("<p>A page with a fixed cover</p>");
    let cover = gtk::Box::builder()
        .width_request(100)
        .height_request(60)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .margin_start(20)
        .margin_top(20)
        .build();
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&page.widget()));
    overlay.add_overlay(&cover);
    page.cover(&cover);
    let window = adw::Window::builder().content(&overlay).build();
    window.present();
    let mut holes = None;
    for (width, height) in [(400, 300), (600, 450), (400, 250), (700, 500), (500, 350)] {
        window.set_default_size(width, height);
        for _ in 0..100 {
            glib::timeout_future(Duration::from_millis(50)).await;
            if page.0.view.frame().size
                == objc2_foundation::NSSize::new(width as f64, height as f64)
            {
                break;
            }
        }
        let size = objc2_foundation::NSSize::new(width as f64, height as f64);
        assert_eq!(
            page.0.view.frame().size,
            size,
            "the native page follows the window"
        );
        let layer = page.0.view.layer().expect("the page has a layer");
        let mask = layer.mask().expect("the cover cuts a hole in the page");
        assert_eq!(mask.frame().size, size, "the mask follows the resized page");
        let current = page.0.float.holes.borrow().clone();
        assert_eq!(current.len(), 1);
        if let Some(before) = &holes {
            assert_eq!(before, &current, "the cover has not moved");
        }
        holes = Some(current);
        above_gtk_tiles(&page);
    }
    cover.set_visible(false);
    for _ in 0..100 {
        glib::timeout_future(Duration::from_millis(50)).await;
        if page.0.float.holes.borrow().is_empty() {
            break;
        }
    }
    assert!(
        page.0.view.layer().unwrap().mask().is_none(),
        "removing the cover clears the mask"
    );
    let dialog = adw::Dialog::builder()
        .child(&gtk::Label::new(Some("A dialog over the page")))
        .build();
    dialog.present(Some(&window));
    for _ in 0..100 {
        glib::timeout_future(Duration::from_millis(50)).await;
        if page.0.clip.isHidden() {
            break;
        }
    }
    assert!(
        page.0.clip.isHidden(),
        "the native page gives way to a dialog"
    );
    assert!(page.0.picture.is_visible(), "a snapshot replaces the page");
    dialog.close();
    for _ in 0..100 {
        glib::timeout_future(Duration::from_millis(50)).await;
        if !page.0.clip.isHidden() {
            break;
        }
    }
    assert!(!page.0.clip.isHidden(), "the page returns after the dialog");
    assert!(!page.0.picture.is_visible(), "the snapshot is removed");
    above_gtk_tiles(&page);
    window.close();
    eprintln!("drive: resizing the native page with a stationary cover passed");
}

fn above_gtk_tiles(page: &WebView) {
    let layer = page.0.clip.layer().expect("the native clip has a layer");
    let parent = layer
        .superlayer()
        .expect("the clip is attached to the window");
    let siblings = unsafe { parent.sublayers() }.expect("the window has layers");
    assert!(siblings.len() > 1, "GTK has painted its tiles");
    for sibling in siblings.iter() {
        if sibling != layer {
            assert!(
                layer.zPosition() > sibling.zPosition(),
                "the native page stays above GTK's replacement tiles"
            );
        }
    }
}
