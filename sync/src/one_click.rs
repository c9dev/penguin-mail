//! RFC 8058's one-click unsubscribe: one POST to a mailing list's own
//! server. No provider runs that server, so the request belongs to no
//! service and sits here instead. `MailActions` holds one; tests and the
//! demo hand it the fake, so nothing they do posts anywhere.

#[cfg(any(test, feature = "fake"))]
use crate::fake::FakeOneClick;
use crate::SyncError;

/// Where a one-click request goes.
pub enum OneClick {
    /// The list's server, over the network.
    Web,
    /// A fake that records what it was asked to post.
    #[cfg(any(test, feature = "fake"))]
    Fake(std::sync::Arc<FakeOneClick>),
}

impl OneClick {
    /// Posts the one-click request to `url`.
    pub async fn post(&self, url: &str) -> Result<(), SyncError> {
        match self {
            OneClick::Web => mailrs_gmail::one_click_unsubscribe(url)
                .await
                .map_err(SyncError::OneClick),
            #[cfg(any(test, feature = "fake"))]
            OneClick::Fake(fake) => fake.post(url),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::OneClick;
    use crate::fake::FakeOneClick;

    #[tokio::test]
    async fn the_fake_records_what_it_posts_and_refuses_when_told() {
        let fake = Arc::new(FakeOneClick::default());
        let one_click = OneClick::Fake(Arc::clone(&fake));
        fake.refuse_next();
        assert!(one_click.post("https://news.example/u/1").await.is_err());
        one_click.post("https://news.example/u/2").await.unwrap();
        assert_eq!(fake.posted(), ["https://news.example/u/2"]);
    }
}
