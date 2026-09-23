//! The page run with no WebKit: a table of pages in memory, one page
//! for whatever a submission leads to, and a log of what was submitted.
//!
//! It is the second adapter behind [`Browser`], and the one the rules
//! and the run are tested through. The assistant's own tests reach for
//! it too, so what it records is what a test asserts on: nothing may be
//! submitted before the person says yes.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use super::{Adviser, Answer, Browser, PageError, PageForm, Plan};

pub struct FakeBrowser {
    /// The page each address loads.
    pub pages: HashMap<String, PageForm>,
    /// The page a submission lands on.
    pub after: PageForm,
    /// What that page says when it is read again a moment later, for a
    /// page that answers after the press. Unset, it says the same.
    pub later: Option<PageForm>,
    /// Every plan submitted, oldest first.
    pub submitted: RefCell<Vec<Plan>>,
    /// The address each submission was told to type.
    pub typed: RefCell<Vec<String>>,
    /// What every call answers instead, once a test sets it.
    pub fail: Option<PageError>,
    /// The page the view stands on, as the real one keeps it.
    pub standing: RefCell<String>,
}

impl FakeBrowser {
    /// A browser holding one page at `url`, and a page saying nothing
    /// after a submission.
    pub fn holding(url: &str, page: PageForm) -> FakeBrowser {
        FakeBrowser {
            pages: HashMap::from([(url.to_string(), page)]),
            after: PageForm {
                text: "Thanks!".to_string(),
                ..PageForm::default()
            },
            later: None,
            submitted: RefCell::new(Vec::new()),
            typed: RefCell::new(Vec::new()),
            fail: None,
            standing: RefCell::new(String::new()),
        }
    }

    /// The same, with `text` on the page a submission lands on.
    pub fn answering(mut self, text: &str) -> FakeBrowser {
        self.after.text = text.to_string();
        self
    }

    pub fn submissions(&self) -> Vec<Plan> {
        self.submitted.borrow().clone()
    }
}

impl Browser for FakeBrowser {
    fn at(&self) -> String {
        self.standing.borrow().clone()
    }

    fn load(&self, url: &str) -> Answer<'_, Result<PageForm, PageError>> {
        *self.standing.borrow_mut() = url.to_string();
        let answer = match &self.fail {
            Some(err) => Err(err.clone()),
            None => self
                .pages
                .get(url)
                .cloned()
                .ok_or_else(|| PageError::Load(format!("nothing is served at {url}"))),
        };
        Box::pin(async move { answer })
    }

    fn submit(&self, plan: &Plan, address: &str) -> Answer<'_, Result<PageForm, PageError>> {
        self.submitted.borrow_mut().push(plan.clone());
        self.typed.borrow_mut().push(address.to_string());
        let answer = match &self.fail {
            Some(err) => Err(err.clone()),
            None => Ok(self.after.clone()),
        };
        Box::pin(async move { answer })
    }

    fn reread(&self) -> Answer<'_, Result<PageForm, PageError>> {
        let answer = match &self.fail {
            Some(err) => Err(err.clone()),
            None => Ok(self.later.clone().unwrap_or_else(|| self.after.clone())),
        };
        Box::pin(async move { answer })
    }
}

/// A model that answers the same thing every time, and counts how often
/// it was asked.
pub struct FakeAdviser {
    pub plan: Option<Plan>,
    pub asked: Cell<usize>,
}

impl FakeAdviser {
    pub fn saying(plan: Option<Plan>) -> FakeAdviser {
        FakeAdviser {
            plan,
            asked: Cell::new(0),
        }
    }
}

impl Adviser for FakeAdviser {
    fn advise(&self, _form: &PageForm, _address: &str) -> Answer<'_, Option<Plan>> {
        self.asked.set(self.asked.get() + 1);
        let plan = self.plan.clone();
        Box::pin(async move { plan })
    }
}
