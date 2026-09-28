//! Where events land once a grid has to share their width or their
//! hours: lanes for overlapping events, which day columns a span
//! crosses, and how the month shares its height between week rows and
//! folds a crowded day into "N more".

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
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
    lanes_within(spans, MOST_LANES)
}

/// [`lanes`] with `most` lanes before a cluster folds, rather than
/// [`MOST_LANES`]. The month passes `usize::MAX`, since it folds by the
/// height each week row gets instead of by a lane count.
pub fn lanes_within(spans: &[(EpochMillis, EpochMillis)], most: usize) -> (Vec<Placed>, Vec<More>) {
    let most = most.max(1);
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
            let lanes = width.min(most);
            let overflow: Vec<usize> = if width > most {
                cluster
                    .iter()
                    .filter(|(_, l)| *l >= most - 1)
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

/// The instant at `hours` of wall clock on `day` in `tz`, the inverse of
/// [`wall_offset`]. An hour the clock repeats gives its first pass; one
/// the clock skips gives the first instant after the gap.
pub fn instant_at<Z: TimeZone>(day: NaiveDate, hours: f64, tz: &Z) -> EpochMillis {
    let minutes = (hours * 60.0).round() as i64;
    let wall = day.and_hms_opt(0, 0, 0).expect("midnight exists") + chrono::Duration::minutes(minutes);
    (0..=8)
        .find_map(|quarter| tz.from_local_datetime(&(wall + chrono::Duration::minutes(15 * quarter))).earliest())
        .map_or(0, |at| at.timestamp_millis())
}

/// The piece of a span of days that one week row of the month draws.
/// Columns count from the row's first day; `end` is exclusive. An end
/// is squared when the span carries on past it, into the row before or
/// after or off the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub week: usize,
    pub start: usize,
    pub end: usize,
    pub squared_start: bool,
    pub squared_end: bool,
}

/// The segments of a span from day `first` to day `end` (exclusive),
/// counted from the grid's first day, over a grid of `weeks` rows of
/// seven days. Days before or after the grid drop out, and the ends
/// they cut off come out squared.
pub fn week_segments(first: i64, end: i64, weeks: usize) -> Vec<Segment> {
    let last_day = (weeks * 7) as i64;
    let from = first.max(0);
    let to = end.min(last_day);
    let mut segments = Vec::new();
    let mut day = from;
    while day < to {
        let week_end = (day / 7 + 1) * 7;
        let piece_end = to.min(week_end);
        segments.push(Segment {
            week: (day / 7) as usize,
            start: (day % 7) as usize,
            end: (piece_end - (day / 7) * 7) as usize,
            squared_start: day > first,
            squared_end: piece_end < end,
        });
        day = piece_end;
    }
    segments
}

/// What a week row shows of its items once it holds only `rows` lines:
/// whether each item shows, and how many items each of the seven days
/// hides behind its "N more" line. `spans` are the items' columns
/// (start, exclusive end) and `lanes` the line each sits on. While
/// every item fits, all show. Otherwise the last line goes to "N more"
/// on each day that hides something, and an item on that line still
/// shows when none of its days hides anything.
pub fn fold_week(spans: &[(usize, usize)], lanes: &[usize], rows: usize) -> (Vec<bool>, [usize; 7]) {
    let rows = rows.max(1);
    let days = |&(start, end): &(usize, usize)| start.min(7)..end.min(7);
    let mut overflows = [false; 7];
    for (span, &lane) in spans.iter().zip(lanes) {
        if lane >= rows {
            days(span).for_each(|day| overflows[day] = true);
        }
    }
    let shown: Vec<bool> = spans
        .iter()
        .zip(lanes)
        .map(|(span, &lane)| lane + 1 < rows || (lane + 1 == rows && !days(span).any(|day| overflows[day])))
        .collect();
    let mut more = [0; 7];
    for (span, _) in spans.iter().zip(&shown).filter(|(_, shown)| !**shown) {
        days(span).for_each(|day| more[day] += 1);
    }
    (shown, more)
}

/// Each week row's height out of `available` pixels, for weeks that
/// need `needs` lines of events each. A row is `heading` pixels for its
/// date and `row` pixels a line. Every week gets its date and one line;
/// the lines left go one at a time to the week with the fewest among
/// those that still want more, so a busy week grows before a quiet one
/// and only folds into "N more" once nothing is left to give it. Pixels
/// left after that spread evenly, so the rows fill `available`. When
/// even the minimum does not fit, the weeks split `available` evenly.
pub fn week_heights(needs: &[usize], available: i32, heading: i32, row: i32) -> Vec<i32> {
    let weeks = needs.len() as i32;
    if weeks == 0 {
        return Vec::new();
    }
    let row = row.max(1);
    let mut heights = vec![0; needs.len()];
    if available >= weeks * (heading + row) {
        let mut lines = vec![1usize; needs.len()];
        let mut spare = (available - weeks * (heading + row)) / row;
        while spare > 0 {
            let wanting = (0..needs.len())
                .filter(|&w| lines[w] < needs[w])
                .min_by_key(|&w| lines[w]);
            let Some(week) = wanting else { break };
            lines[week] += 1;
            spare -= 1;
        }
        for (height, l) in heights.iter_mut().zip(lines) {
            *height = heading + l as i32 * row;
        }
    }
    let left = available - heights.iter().sum::<i32>();
    for (week, height) in heights.iter_mut().enumerate() {
        *height += left / weeks + i32::from((week as i32) < left % weeks);
    }
    heights
}

/// How many lines of events a week row `height` pixels tall holds,
/// never fewer than one.
pub fn week_rows(height: i32, heading: i32, row: i32) -> usize {
    ((height - heading).max(0) / row.max(1)).max(1) as usize
}

/// The hour a day or week grid opens scrolled to: 08:00, or earlier when
/// a timed event starts before it, so the day's first event is not
/// hidden above the fold.
pub fn first_hour(timed_starts: &[f64]) -> f64 {
    timed_starts.iter().copied().fold(8.0, f64::min)
}

/// The hour a grid scrolls to when it opens an event starting at
/// `start`: an hour above the event, so its block and the popover
/// pointing at it land in the upper part of the view with some of the
/// day above for context.
pub fn open_hour<Z: TimeZone>(start: EpochMillis, tz: &Z) -> f64 {
    let Some(local) = DateTime::<Utc>::from_timestamp_millis(start).map(|utc| utc.with_timezone(tz)) else {
        return 0.0;
    };
    let midnight = local.date_naive().and_hms_opt(0, 0, 0).expect("midnight exists");
    (wall_offset(start, midnight, tz) - 1.0).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: EpochMillis = 3_600_000;

    #[test]
    fn an_event_opened_at_14_00_scrolls_the_grid_to_13_00() {
        // Tuesday 6 October 2026, 14:00 UTC.
        let start = 1_791_295_200_000;
        assert_eq!(open_hour(start, &Utc), 13.0);
    }

    #[test]
    fn an_event_opened_before_01_00_scrolls_the_grid_to_midnight() {
        let midnight = 1_791_244_800_000;
        assert_eq!(open_hour(midnight + H / 2, &Utc), 0.0);
    }

    #[test]
    fn an_event_opened_at_23_00_scrolls_the_grid_to_22_00() {
        let midnight = 1_791_244_800_000;
        assert_eq!(open_hour(midnight + 23 * H, &Utc), 22.0);
    }

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
    fn a_wall_clock_time_turns_back_into_the_instant() {
        use chrono::TimeZone;
        let tz = chrono_tz::Europe::Lisbon;
        let day = chrono::NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
        let noon = tz.from_local_datetime(&day.and_hms_opt(12, 0, 0).unwrap()).single().unwrap().timestamp_millis();
        assert_eq!(instant_at(day, 12.0, &tz), noon);
        assert_eq!(wall_offset(instant_at(day, 15.25, &tz), day.and_hms_opt(0, 0, 0).unwrap(), &tz), 15.25);
    }

    #[test]
    fn a_time_the_clock_skips_lands_after_the_gap() {
        use chrono::TimeZone;
        let tz = chrono_tz::Europe::Lisbon;
        // On 29 March 2026 Lisbon jumps from 01:00 to 02:00.
        let day = chrono::NaiveDate::from_ymd_opt(2026, 3, 29).unwrap();
        let two = chrono::Utc.with_ymd_and_hms(2026, 3, 29, 1, 0, 0).unwrap().timestamp_millis();
        assert_eq!(instant_at(day, 1.5, &tz), two);
    }

    fn seg(week: usize, start: usize, end: usize, squared_start: bool, squared_end: bool) -> Segment {
        Segment { week, start, end, squared_start, squared_end }
    }

    #[test]
    fn a_span_inside_one_week_is_one_segment_with_round_ends() {
        assert_eq!(week_segments(9, 12, 6), vec![seg(1, 2, 5, false, false)]);
    }

    #[test]
    fn a_span_across_a_week_boundary_squares_the_ends_that_continue() {
        // Friday to Monday in a grid whose weeks start on Monday.
        assert_eq!(
            week_segments(4, 8, 6),
            vec![seg(0, 4, 7, false, true), seg(1, 0, 1, true, false)]
        );
    }

    #[test]
    fn a_span_over_three_weeks_squares_both_ends_of_the_middle_one() {
        assert_eq!(
            week_segments(5, 20, 6),
            vec![seg(0, 5, 7, false, true), seg(1, 0, 7, true, true), seg(2, 0, 6, true, false)]
        );
    }

    #[test]
    fn a_span_ending_on_the_last_day_of_a_week_stays_in_that_week() {
        assert_eq!(week_segments(12, 14, 6), vec![seg(1, 5, 7, false, false)]);
    }

    #[test]
    fn a_span_from_before_the_grid_is_squared_where_the_grid_cuts_it() {
        assert_eq!(week_segments(-3, 2, 6), vec![seg(0, 0, 2, true, false)]);
    }

    #[test]
    fn a_span_past_the_grid_is_squared_at_its_last_day() {
        assert_eq!(week_segments(40, 45, 6), vec![seg(5, 5, 7, false, true)]);
    }

    #[test]
    fn a_span_wholly_outside_the_grid_has_no_segment() {
        assert!(week_segments(-5, 0, 6).is_empty());
        assert!(week_segments(42, 44, 6).is_empty());
    }

    #[test]
    fn a_single_day_is_one_segment_one_column_wide() {
        assert_eq!(week_segments(3, 4, 6), vec![seg(0, 3, 4, false, false)]);
    }

    fn day_spans(days: &[(usize, usize)]) -> Vec<(EpochMillis, EpochMillis)> {
        days.iter().map(|&(s, e)| (s as EpochMillis, e as EpochMillis)).collect()
    }

    #[test]
    fn lanes_within_no_limit_never_folds() {
        let spans = day_spans(&[(0, 1); 9]);
        let (placed, more) = lanes_within(&spans, usize::MAX);
        assert!(more.is_empty());
        assert_eq!(placed.iter().map(|p| p.lane).collect::<Vec<_>>(), (0..9).collect::<Vec<_>>());
    }

    #[test]
    fn a_bar_takes_the_top_line_over_chips_that_start_on_its_first_day() {
        // A chip on Tuesday listed before a bar from Tuesday to Thursday,
        // and a chip on Wednesday: the bar goes first, the chips below.
        let spans = day_spans(&[(1, 2), (1, 4), (2, 3)]);
        let (placed, _) = lanes_within(&spans, usize::MAX);
        let lanes: Vec<usize> = placed.iter().map(|p| p.lane).collect();
        assert_eq!(lanes, vec![1, 0, 1]);
    }

    #[test]
    fn chips_on_different_days_share_a_line() {
        let spans = day_spans(&[(0, 1), (3, 4), (6, 7)]);
        let (placed, _) = lanes_within(&spans, usize::MAX);
        assert!(placed.iter().all(|p| p.lane == 0));
    }

    #[test]
    fn lanes_keeps_folding_past_four() {
        let spans: Vec<_> = (0..6).map(|_| (9 * H, 10 * H)).collect();
        assert_eq!(lanes(&spans), lanes_within(&spans, MOST_LANES));
    }

    #[test]
    fn a_week_with_room_shows_everything() {
        let (shown, more) = fold_week(&[(0, 3), (0, 1), (1, 2)], &[0, 1, 1], 2);
        assert_eq!(shown, vec![true, true, true]);
        assert_eq!(more, [0; 7]);
    }

    #[test]
    fn a_crowded_day_gives_its_last_line_to_n_more() {
        // Monday holds three chips on lines 0 to 2; only two lines fit.
        let (shown, more) = fold_week(&[(0, 1), (0, 1), (0, 1)], &[0, 1, 2], 2);
        assert_eq!(shown, vec![true, false, false]);
        assert_eq!(more, [2, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn an_item_on_the_last_line_stays_when_its_days_hide_nothing() {
        // Monday overflows; Friday's chip on line 1 has nothing under it.
        let (shown, more) = fold_week(&[(0, 1), (0, 1), (0, 1), (4, 5), (4, 5)], &[0, 1, 2, 0, 1], 2);
        assert_eq!(shown, vec![true, false, false, true, true]);
        assert_eq!(more, [2, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn a_bar_on_the_last_line_folds_on_every_day_it_covers() {
        // A bar from Monday to Wednesday on line 1, and a chip under it
        // on Tuesday; one line of events and "N more" fit.
        let (shown, more) = fold_week(&[(0, 1), (0, 3), (1, 2)], &[0, 1, 2], 2);
        assert_eq!(shown, vec![true, false, false]);
        assert_eq!(more, [1, 2, 1, 0, 0, 0, 0]);
    }

    #[test]
    fn a_week_of_one_line_shows_only_n_more_on_a_crowded_day() {
        let (shown, more) = fold_week(&[(2, 3), (2, 3), (5, 6)], &[0, 1, 0], 1);
        assert_eq!(shown, vec![false, false, true]);
        assert_eq!(more, [0, 0, 2, 0, 0, 0, 0]);
    }

    #[test]
    fn weeks_that_fit_get_what_they_need_and_share_the_rest_evenly() {
        // Heading 30, line 20: needs of 3, 0 and 1 lines take 90, 50 and
        // 50; 60 pixels are left over, 20 for each week.
        assert_eq!(week_heights(&[3, 0, 1], 250, 30, 20), vec![110, 70, 70]);
    }

    #[test]
    fn an_empty_week_keeps_its_date_and_one_line() {
        // Only the minimum fits for the quiet weeks once the busy one
        // takes the rest.
        let heights = week_heights(&[0, 10, 0], 3 * 50 + 3 * 20, 30, 20);
        assert_eq!(heights[0], 50);
        assert_eq!(heights[2], 50);
    }

    #[test]
    fn a_busy_week_grows_before_a_quiet_one() {
        // 6 weeks, heading 30, line 20, 400 pixels: the minimum is 300,
        // so five lines are left, all for the week that needs eight.
        let heights = week_heights(&[1, 8, 0, 1, 0, 0], 400, 30, 20);
        assert_eq!(heights, vec![50, 150, 50, 50, 50, 50]);
        assert_eq!(week_rows(heights[1], 30, 20), 6);
    }

    #[test]
    fn two_busy_weeks_share_the_lines_left() {
        let heights = week_heights(&[6, 6, 0], 30 * 3 + 20 * 7, 30, 20);
        assert_eq!(heights, vec![90, 90, 50]);
    }

    #[test]
    fn the_rows_always_fill_the_space_given() {
        for available in [200, 333, 401, 777, 1000] {
            let heights = week_heights(&[2, 5, 0, 1, 7, 0], available, 28, 21);
            assert_eq!(heights.iter().sum::<i32>(), available, "{available}");
        }
    }

    #[test]
    fn too_little_space_splits_evenly() {
        assert_eq!(week_heights(&[3, 0, 0], 100, 30, 20), vec![34, 33, 33]);
    }

    #[test]
    fn a_week_row_holds_at_least_one_line() {
        assert_eq!(week_rows(70, 30, 20), 2);
        assert_eq!(week_rows(49, 30, 20), 1);
        assert_eq!(week_rows(10, 30, 20), 1);
    }

    #[test]
    fn the_grid_opens_at_eight_or_the_first_event_if_earlier() {
        assert_eq!(first_hour(&[]), 8.0);
        assert_eq!(first_hour(&[9.5, 10.0]), 8.0);
        assert_eq!(first_hour(&[6.25, 9.0]), 6.25);
    }
}
