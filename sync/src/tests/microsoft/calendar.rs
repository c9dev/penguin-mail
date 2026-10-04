use crate::CalendarService;

#[tokio::test]
async fn the_calendar_is_not_wired_yet() {
    let h = super::outlook().await;
    let calendar = h.sync.services().calendar.clone().unwrap();
    assert!(matches!(calendar.calendars().await, Err(crate::BackendError::Unsupported)));
}
