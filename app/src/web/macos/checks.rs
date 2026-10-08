//! WebKit checks run by the debug driver on the application's main thread.

use std::time::Duration;

use gtk::glib;

use super::*;
use crate::unsubscribe_page::Browser;

pub async fn check_macos(url: &str) {
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
    assert!(page.0.bridge.ivars().tasks.borrow().is_empty(), "dropping the last clone removes its task");

    let count = Rc::new(Cell::new(None));
    let counted = Rc::clone(&count);
    let finder = page.finder();
    finder.on_counted(move |found| counted.set(Some(found)));
    finder.count("", false);
    assert_eq!(count.get(), Some(0));

    let hidden = crate::unsubscribe_page::PageBrowser::new();
    let form = hidden.load(url).await.expect("the hidden fixture loads");
    assert!(form.text.contains("Pictures blocked"), "the image fails while scripts still run: {}", form.text);
    eprintln!("drive: empty Find, dropped scheme task and hidden image blocking passed");
}
