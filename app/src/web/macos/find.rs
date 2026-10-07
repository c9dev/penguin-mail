//! Finding text in a page. WKWebView finds and selects one match at a
//! time but counts nothing, so a script of the app's own counts the
//! matches, inside the shadow roots that hold the message bodies too.

use std::cell::RefCell;
use std::rc::Rc;

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_foundation::NSString;
use objc2_web_kit::{WKFindConfiguration, WKFindResult};

use super::WeakWebView;

/// Counts the matches of a query in the page's text, skipping style and
/// script, and answers the number as a string.
const COUNT: &str = r#"(function (query, sensitive) {
  if (!sensitive) query = query.toLowerCase();
  var found = 0;
  var walk = function (root) {
    var walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT);
    for (var node = walker.nextNode(); node; node = walker.nextNode()) {
      if (node.nodeType === 1) {
        if (node.shadowRoot) walk(node.shadowRoot);
        continue;
      }
      var parent = node.parentNode && node.parentNode.nodeName;
      if (parent === 'STYLE' || parent === 'SCRIPT' || parent === 'TITLE') continue;
      var text = sensitive ? node.data : node.data.toLowerCase();
      for (var at = text.indexOf(query); at !== -1; at = text.indexOf(query, at + query.length)) found++;
    }
  };
  walk(document.body || document.documentElement);
  return String(found);
})"#;

/// Clears the selection, so the next find starts from the top.
const UNSELECT: &str = "window.getSelection().removeAllRanges()";

type Heard = Rc<dyn Fn()>;

#[derive(Default)]
struct Listeners {
    found: Option<Heard>,
    counted: Option<Rc<dyn Fn(usize)>>,
    failed: Option<Heard>,
}

/// Finds text in one page and selects the match.
pub struct Finder {
    page: WeakWebView,
    query: RefCell<(String, bool)>,
    listeners: Rc<RefCell<Listeners>>,
}

impl Finder {
    pub(super) fn new(page: WeakWebView) -> Finder {
        Finder {
            page,
            query: RefCell::new((String::new(), false)),
            listeners: Rc::default(),
        }
    }

    /// Selects the first match of `query` and counts them all.
    pub fn search(&self, query: &str, case_sensitive: bool) {
        *self.query.borrow_mut() = (query.to_string(), case_sensitive);
        let Some(page) = self.page.upgrade() else { return };
        page.run(UNSELECT);
        self.count(query, case_sensitive);
        self.find(false);
    }

    /// Counts the matches again, leaving the selection where it is.
    pub fn count(&self, query: &str, case_sensitive: bool) {
        let Some(page) = self.page.upgrade() else { return };
        let (Ok(query), Ok(sensitive)) = (
            serde_json::to_string(query),
            serde_json::to_string(&case_sensitive),
        ) else {
            return;
        };
        let listeners = Rc::clone(&self.listeners);
        page.eval(&format!("{COUNT}({query},{sensitive})"), move |answer| {
            let counted = listeners.borrow().counted.clone();
            if let (Some(counted), Ok(Ok(matches))) =
                (counted, answer.map(|value| value.parse::<usize>()))
            {
                counted(matches);
            }
        });
    }

    pub fn next(&self) {
        self.find(false);
    }

    pub fn previous(&self) {
        self.find(true);
    }

    /// Takes the selection off the page.
    pub fn finish(&self) {
        if let Some(page) = self.page.upgrade() {
            page.run(UNSELECT);
        }
    }

    fn find(&self, backwards: bool) {
        let Some(page) = self.page.upgrade() else { return };
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let (query, sensitive) = self.query.borrow().clone();
        if query.is_empty() {
            return;
        }
        let config = unsafe { WKFindConfiguration::new(mtm) };
        unsafe {
            config.setBackwards(backwards);
            config.setCaseSensitive(sensitive);
            config.setWraps(true);
        }
        let listeners = Rc::clone(&self.listeners);
        let done = RcBlock::new(move |result: std::ptr::NonNull<WKFindResult>| {
            let found = unsafe { result.as_ref().matchFound() };
            let listeners = listeners.borrow();
            let heard = match found {
                true => listeners.found.clone(),
                false => listeners.failed.clone(),
            };
            drop(listeners);
            if let Some(heard) = heard {
                heard();
            }
        });
        unsafe {
            page.0
                .view
                .findString_withConfiguration_completionHandler(
                    &NSString::from_str(&query),
                    Some(&config),
                    &done,
                );
        }
    }

    pub fn on_found(&self, found: impl Fn() + 'static) {
        self.listeners.borrow_mut().found = Some(Rc::new(found));
    }

    pub fn on_counted(&self, counted: impl Fn(usize) + 'static) {
        self.listeners.borrow_mut().counted = Some(Rc::new(counted));
    }

    pub fn on_failed(&self, failed: impl Fn() + 'static) {
        self.listeners.borrow_mut().failed = Some(Rc::new(failed));
    }
}
