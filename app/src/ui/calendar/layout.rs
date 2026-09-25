//! Where events land once a grid has to share their width or their
//! hours: lanes for overlapping events, which day columns a span
//! crosses, and how a month cell folds a crowded day into "N more".

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use mailrs_domain::EpochMillis;

/// The most lanes an hour's events share before the rest fold into a
/// "+N" card.
pub const MOST_LANES: usize = 4;

/// Where one event sits in its cluster's shared width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    pub index: usize,
    pub lane: usize,
    pub lanes: usize,
}

/// A run of events, all overlapping each other, that a cluster had no
/// lane left for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct More {
    pub from: EpochMillis,
    pub to: EpochMillis,
    pub hidden: Vec<usize>,
}

/// Lays `spans` (each event's start and end) out in lanes by start time,
/// greedy: an event takes the first lane whose last event has already
/// ended, or a new one. A cluster of events that overlap, directly or
/// through one another, shares one `lanes` count. When a cluster would
/// need a fifth lane, the first `MOST_LANES - 1` lanes hold events and
/// the rest fold into one or more [`More`] cards, one per run of hidden
/// events that overlap each other, so two runs hours apart in the same
/// cluster never share a card that claims to cover the gap between them.
pub fn lanes(spans: &[(EpochMillis, EpochMillis)]) -> (Vec<Placed>, Vec<More>) {
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by_key(|&i| (spans[i].0, -(spans[i].1 - spans[i].0)));
    let mut placed = Vec::new();
    let mut more = Vec::new();
    let mut cluster: Vec<(usize, usize)> = Vec::new(); // (index, lane)
    let mut lane_ends: Vec<EpochMillis> = Vec::new();
    let mut cluster_end = EpochMillis::MIN;
    let flush =
        |cluster: &mut Vec<(usize, usize)>, placed: &mut Vec<Placed>, more: &mut Vec<More>| {
            let width = cluster.iter().map(|(_, l)| l + 1).max().unwrap_or(1);
            let lanes = width.min(MOST_LANES);
            let overflow: Vec<usize> = if width > MOST_LANES {
                cluster
                    .iter()
                    .filter(|(_, l)| *l >= MOST_LANES - 1)
                    .map(|(i, _)| *i)
                    .collect()
            } else {
                Vec::new()
            };
            for (i, l) in cluster.drain(..) {
                if !overflow.contains(&i) {
                    placed.push(Placed {
                        index: i,
                        lane: l,
                        lanes,
                    });
                }
            }
            more.extend(overflow_runs(&overflow, spans));
        };
    for i in order {
        let (start, end) = spans[i];
        if start >= cluster_end && !cluster.is_empty() {
            flush(&mut cluster, &mut placed, &mut more);
            lane_ends.clear();
        }
        let lane = match lane_ends.iter().position(|&e| e <= start) {
            Some(free) => {
                lane_ends[free] = end;
                free
            }
            None => {
                lane_ends.push(end);
                lane_ends.len() - 1
            }
        };
        cluster.push((i, lane));
        cluster_end = cluster_end.max(end);
    }
    if !cluster.is_empty() {
        flush(&mut cluster, &mut placed, &mut more);
    }
    placed.sort_by_key(|p| p.index);
    (placed, more)
}

/// Splits a cluster's overflow into runs of events that overlap each
/// other, so a "more" card never spans a gap where nothing is hidden.
fn overflow_runs(overflow: &[usize], spans: &[(EpochMillis, EpochMillis)]) -> Vec<More> {
    let mut order = overflow.to_vec();
    order.sort_by_key(|&i| spans[i].0);
    let mut runs = Vec::new();
    let mut run: Vec<usize> = Vec::new();
    let mut run_end = EpochMillis::MIN;
    for i in order {
        let (start, end) = spans[i];
        if start >= run_end && !run.is_empty() {
            runs.push(more_of(&run, spans));
            run.clear();
        }
        run_end = run_end.max(end);
        run.push(i);
    }
    if !run.is_empty() {
        runs.push(more_of(&run, spans));
    }
    runs
}

fn more_of(run: &[usize], spans: &[(EpochMillis, EpochMillis)]) -> More {
    let from = run.iter().map(|&i| spans[i].0).min().unwrap_or(0);
    let to = run.iter().map(|&i| spans[i].1).max().unwrap_or(0);
    More {
        from,
        to,
        hidden: run.to_vec(),
    }
}

/// Which day columns an event covers, each clipped to that day.
pub fn clip_to_days(
    start: EpochMillis,
    end: EpochMillis,
    days: &[(EpochMillis, EpochMillis)],
) -> Vec<(usize, EpochMillis, EpochMillis)> {
    days.iter()
        .enumerate()
        .filter_map(|(i, &(day_start, day_end))| {
            let clipped = (start.max(day_start), end.min(day_end));
            (clipped.0 < clipped.1).then_some((i, clipped.0, clipped.1))
        })
        .collect()
}

/// Hours from `day_start_local`'s midnight to `at`, by wall clock rather
/// than elapsed time, so a clock-change day still places a 12:00 event
/// at 12.0 whether that day held 23, 24 or 25 hours.
pub fn wall_offset<Z: TimeZone>(at: EpochMillis, day_start_local: NaiveDateTime, tz: &Z) -> f64 {
    let Some(utc) = DateTime::<Utc>::from_timestamp_millis(at) else {
        return 0.0;
    };
    let local = utc.with_timezone(tz).naive_local();
    (local - day_start_local).num_seconds() as f64 / 3600.0
}

/// How many of a month cell's `count` events fit in `rows_that_fit`
/// before a "N more" line, keeping the last row for that line once the
/// cell is crowded.
pub fn month_fit(count: usize, rows_that_fit: usize) -> (usize, usize) {
    if count <= rows_that_fit {
        (count, 0)
    } else {
        let shown = rows_that_fit - 1;
        (shown, count - shown)
    }
}

/// The hour a day or week grid opens scrolled to: 08:00, or earlier when
/// a timed event starts before it, so the day's first event is not
/// hidden above the fold.
pub fn first_hour(timed_starts: &[f64]) -> f64 {
    timed_starts.iter().copied().fold(8.0, f64::min)
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: EpochMillis = 3_600_000;

    #[test]
    fn events_that_overlap_share_the_width() {
        let (placed, more) = lanes(&[(10 * H, 11 * H + H / 2), (11 * H, 12 * H), (14 * H, 15 * H)]);
        assert!(more.is_empty());
        assert_eq!(
            placed[0],
            Placed {
                index: 0,
                lane: 0,
                lanes: 2
            }
        );
        assert_eq!(
            placed[1],
            Placed {
                index: 1,
                lane: 1,
                lanes: 2
            }
        );
        assert_eq!(
            placed[2],
            Placed {
                index: 2,
                lane: 0,
                lanes: 1
            }
        );
    }

    #[test]
    fn a_crowded_hour_folds_into_a_more_card() {
        let spans: Vec<_> = (0..6).map(|_| (9 * H, 10 * H)).collect();
        let (placed, more) = lanes(&spans);
        assert_eq!(placed.len(), MOST_LANES - 1);
        assert_eq!(more.len(), 1);
        assert_eq!(more[0].hidden.len(), 3);
    }

    #[test]
    fn hidden_events_hours_apart_get_a_more_card_each() {
        // One long event holds the cluster together across a gap; a
        // crowded hour in the morning and another in the afternoon each
        // overflow a lane, but nothing overlaps between the two, so one
        // "more" card must never claim to cover the hours in between.
        let mut spans = vec![(0, 10 * H)];
        spans.extend((0..4).map(|_| (H, 2 * H)));
        spans.extend((0..4).map(|_| (7 * H, 8 * H)));
        let (placed, more) = lanes(&spans);
        assert_eq!(placed.len(), 5);
        assert_eq!(
            more.len(),
            2,
            "two runs of hidden events hours apart get a card each"
        );
        assert_eq!(more[0].hidden.len(), 2);
        assert_eq!(more[1].hidden.len(), 2);
        assert!(
            more[0].to <= more[1].from,
            "a more card never spans the gap between two runs of hidden events"
        );
    }

    #[test]
    fn an_event_across_midnight_shows_in_both_days() {
        let days = [(0, 24 * H), (24 * H, 48 * H)];
        assert_eq!(
            clip_to_days(22 * H, 25 * H, &days),
            vec![(0, 22 * H, 24 * H), (1, 24 * H, 25 * H)]
        );
    }

    #[test]
    fn the_clock_change_day_places_by_wall_time() {
        use chrono::TimeZone;
        let tz = chrono_tz::Europe::Lisbon;
        let midnight = chrono::NaiveDate::from_ymd_opt(2026, 10, 25)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let noon = tz
            .from_local_datetime(&midnight.date().and_hms_opt(12, 0, 0).unwrap())
            .single()
            .unwrap()
            .timestamp_millis();
        assert_eq!(wall_offset(noon, midnight, &tz), 12.0);
    }

    #[test]
    fn a_month_cell_keeps_a_row_for_the_more_line() {
        assert_eq!(month_fit(2, 3), (2, 0));
        assert_eq!(month_fit(3, 3), (3, 0));
        assert_eq!(month_fit(5, 3), (2, 3));
    }

    #[test]
    fn the_grid_opens_at_eight_or_the_first_event_if_earlier() {
        assert_eq!(first_hour(&[]), 8.0);
        assert_eq!(first_hour(&[9.5, 10.0]), 8.0);
        assert_eq!(first_hour(&[6.25, 9.0]), 6.25);
    }
}
