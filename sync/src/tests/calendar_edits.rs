//! Event edits survive a write and a fresh read through each calendar adapter.

use mailrs_domain::calendar::{Access, Calendar, Event, Notify};

use crate::fake::FakeJmap;
use crate::{CalendarService, calendar_copy::new_event_id};

#[tokio::test]
async fn new_event_edits_survive_a_server_read_on_every_provider() {
    let google = super::harness().await;
    google.fake.with(|s| {
        s.calendars = vec![Calendar {
            id: "primary".into(),
            name: "Personal".into(),
            access: Access::Owner,
            zone: "UTC".into(),
            primary: true,
            shown: true,
            hidden: false,
            color: "#3584e4".into(),
            reminders: Vec::new(),
            all_day_reminders: None,
        }]
    });
    let microsoft = super::microsoft::outlook().await;
    let jmap = super::jmap::over(FakeJmap::new().with_calendars()).await;
    let (_dav, caldav) = super::calendar_account::calendar_only();
    let providers = [
        ("Google", google.sync.services().calendar.clone().unwrap()),
        (
            "Microsoft",
            microsoft.sync.services().calendar.clone().unwrap(),
        ),
        ("CalDAV", caldav.calendar.unwrap()),
        ("JMAP", jmap.sync.services().calendar.clone().unwrap()),
    ];
    for (provider, service) in providers {
        let calendars = service.calendars().await.unwrap();
        let event = Event {
            calendar: calendars[0].id.clone(),
            id: new_event_id(),
            title: "Coffee".into(),
            zone: "UTC".into(),
            busy: true,
            start: 1_791_288_000_000,
            end: 1_791_291_600_000,
            ..Event::default()
        };
        let made = service
            .put_event(&event, None, true, Notify::Nobody)
            .await
            .unwrap();
        let edited = Event {
            all_day: true,
            start: 1_791_244_800_000,
            end: 1_791_331_200_000,
            ..made.clone()
        };
        let saved = service
            .put_event(&edited, Some(&made.etag), false, Notify::Nobody)
            .await
            .unwrap();
        assert!(saved.all_day, "{provider}");
        let renamed = Event {
            title: "Coffee with Ann".into(),
            ..saved.clone()
        };
        let saved = service
            .put_event(&renamed, Some(&saved.etag), false, Notify::Nobody)
            .await
            .unwrap();
        assert_eq!(saved.title, "Coffee with Ann", "{provider}");
        let page = service
            .event_range(&saved.calendar, edited.start, edited.end, None)
            .await
            .unwrap();
        let read = page
            .events
            .iter()
            .find(|e| e.id == saved.id)
            .expect(provider);
        assert_eq!(read.title, "Coffee with Ann", "{provider}");
        assert!(read.all_day, "{provider}");
    }
}
