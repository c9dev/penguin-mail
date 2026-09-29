//! The taupe band at the top of Add Account and of the first-run window:
//! the tuxedo envelope in one pose per state, and an overlay that draws
//! what moves on it. `crate::add_account::post` decides which pose and
//! which stamp; this draws them.
//!
//! Each pose is an SVG in the resource bundle, shown by a `gtk::Picture`
//! in a crossfading stack. `Marks`, laid over the stack, draws the stamp,
//! the warning badge, the walking dots, the gliding letters and the
//! blink in `snapshot`, from the envelope's geometry in each pose, the
//! numbers `mockups_d1c.py` draws the art with.

use std::cell::{Cell, RefCell};
use std::f32::consts::PI;
use std::time::Duration;

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};

use crate::add_account::post::{Band, Stamp};

/// The art's canvas, the same for every pose.
pub const ART_W: f32 = 440.0;
pub const ART_H: f32 = 196.0;

const PAPER: (u8, u8, u8) = (0xfb, 0xf7, 0xf0);
const INK: (u8, u8, u8) = (0x2a, 0x26, 0x23);
const AMBER: (u8, u8, u8) = (0xf5, 0xc2, 0x11);
/// The letter on a provider's tile.
const CREAM: (u8, u8, u8) = (0xfb, 0xf1, 0xc7);
/// The face just above the eyes, where its gradient has got to.
const FACE: (u8, u8, u8) = (0xfd, 0xfa, 0xf6);

fn rgba((r, g, b): (u8, u8, u8), alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
        alpha,
    )
}

/// `#rrggbb` as a colour, or the ink for anything else.
fn hex(colour: &str) -> gdk::RGBA {
    let byte = |at: usize| u8::from_str_radix(colour.get(at..at + 2)?, 16).ok();
    match (byte(1), byte(3), byte(5)) {
        (Some(r), Some(g), Some(b)) => rgba((r, g, b), 1.0),
        _ => rgba(INK, 1.0),
    }
}

/// The envelope in one pose: icon C's geometry at width `w`, top-left at
/// (`x`, `top`), with the pupils nudged by `look`.
#[derive(Debug, Clone, Copy)]
struct Env {
    x: f32,
    top: f32,
    w: f32,
    look: (f32, f32),
}

impl Env {
    fn of(band: Band) -> Env {
        match band {
            Band::Idle => Env { x: 20.0, top: 66.0, w: 124.0, look: (0.0, 0.0) },
            Band::Stamped | Band::Error => Env { x: 150.0, top: 62.0, w: 140.0, look: (0.0, 1.0) },
            Band::Browser | Band::Lookup => Env { x: 52.0, top: 66.0, w: 124.0, look: (4.0, -1.0) },
            Band::Unreachable => Env { x: 52.0, top: 66.0, w: 124.0, look: (0.0, 1.0) },
            Band::Success => Env { x: 154.0, top: 92.0, w: 132.0, look: (0.0, 0.0) },
        }
    }

    fn h(self) -> f32 {
        self.w * 78.0 / 108.0
    }

    /// The stamp's middle, its width and its height.
    fn stamp(self) -> (f32, f32, f32, f32) {
        let sw = self.w * 0.27;
        let sh = sw * 1.16;
        (self.x + self.w - sw * 0.42, self.top + sh * 0.12, sw, sh)
    }

    /// The eyes' middles and their radius.
    fn eyes(self) -> ([(f32, f32); 2], f32) {
        let (dx, dy) = self.look;
        let cx = self.x + self.w / 2.0;
        let y = self.top + self.h() * 0.66 + dy;
        let ex = self.w * 0.2;
        ([(cx - ex + dx, y), (cx + ex + dx, y)], self.w * 0.04)
    }
}

type Point = (f32, f32);

/// A point on the quadratic curve p0, p1, p2 at `t`.
fn along((p0, p1, p2): (Point, Point, Point), t: f32) -> Point {
    let u = 1.0 - t;
    (
        u * u * p0.0 + 2.0 * u * t * p1.0 + t * t * p2.0,
        u * u * p0.1 + 2.0 * u * t * p1.1 + t * t * p2.1,
    )
}

/// The dashed path to the browser window, drawn in the art.
fn to_browser() -> (Point, Point, Point) {
    let e = Env::of(Band::Browser);
    ((e.x + e.w + 12.0, e.top + 50.0), (e.x + e.w + 64.0, e.top + 2.0), (286.0, 104.0))
}

/// The two lookup lines: to the upper server, and to the lower one that
/// waits for MX.
fn to_servers() -> [(Point, Point, Point); 2] {
    let e = Env::of(Band::Lookup);
    [
        ((e.x + e.w + 12.0, e.top + 44.0), (e.x + e.w + 70.0, e.top + 4.0), (300.0, 70.0)),
        ((e.x + e.w + 12.0, e.top + 64.0), (e.x + e.w + 70.0, e.top + 96.0), (300.0, 136.0)),
    ]
}

/// The arc the letters glide along into the open envelope.
fn letter_arc() -> (Point, Point, Point) {
    let e = Env::of(Band::Success);
    ((430.0, 150.0), (420.0, 50.0), (e.x + e.w / 2.0 + 44.0, e.top - 30.0))
}

/// The ease-in-out the lookup dots run with.
fn ease_in_out(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Marks {
        pub band: Cell<Option<Band>>,
        pub stamp: Cell<Option<Stamp>>,
        /// The stamp the last one replaced, fading out while `swap` runs.
        pub old_stamp: Cell<Option<Stamp>>,
        /// 0 to 1 while a new stamp fades in over the old one.
        pub swap: Cell<f64>,
        /// Where a flying stamp starts, in the art's coordinates, and how
        /// far it has got: 0 at the tile, 1 on the corner.
        pub flight_from: Cell<Option<Point>>,
        pub flight: Cell<f64>,
        /// 0 to 1 while the badge grows in.
        pub badge: Cell<f64>,
        /// Where the looping dots or letters are, 0 to 1, or `None`
        /// while nothing loops: animations off, or the band hidden.
        pub phase: Cell<Option<f64>>,
        /// 0 to 1 through a blink.
        pub blink: Cell<f64>,
        /// The lower lookup line has something to ask: MX answered.
        pub lower: Cell<bool>,
        /// Whether the success letters still come: the first sync runs.
        pub letters: Cell<bool>,
        /// How much larger than the art's own size the band draws it.
        pub scale: Cell<f32>,
        /// The loops and the blink, stopped when the band leaves the
        /// screen or changes pose.
        pub loops: RefCell<Vec<adw::Animation>>,
        /// One-off moves: the stamp's flight, its swap, the badge.
        pub moves: RefCell<Vec<adw::Animation>>,
        pub blink_timer: RefCell<Option<glib::SourceId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Marks {
        const NAME: &'static str = "MailrsPostMarks";
        type Type = super::Marks;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            // Everything this draws is part of the picture the band as a
            // whole describes.
            klass.set_accessible_role(gtk::AccessibleRole::Presentation);
        }
    }

    impl ObjectImpl for Marks {}

    impl WidgetImpl for Marks {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let scale = self.scale.get().max(1.0);
            let size = match orientation {
                gtk::Orientation::Horizontal => ART_W * scale,
                _ => ART_H * scale,
            };
            (size as i32, size as i32, -1, -1)
        }

        fn map(&self) {
            self.parent_map();
            self.obj().restart_loops();
        }

        fn unmap(&self) {
            self.obj().stop_loops();
            self.parent_unmap();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let Some(band) = self.band.get() else {
                return;
            };
            let obj = self.obj();
            let scale = self.scale.get().max(1.0);
            // The art sits in the middle of the widget at its own size
            // times `scale`, as the picture below it does.
            let left = (obj.width() as f32 - ART_W * scale) / 2.0;
            let top = (obj.height() as f32 - ART_H * scale) / 2.0;
            snapshot.save();
            snapshot.translate(&graphene::Point::new(left, top));
            snapshot.scale(scale, scale);
            let env = Env::of(band);
            match band {
                Band::Idle => self.draw_blink(snapshot, env),
                Band::Browser => {
                    if let Some(phase) = self.phase.get() {
                        draw_walking_dots(snapshot, phase as f32);
                    }
                }
                Band::Lookup => {
                    if let Some(phase) = self.phase.get() {
                        let lines = to_servers();
                        let asking = if self.lower.get() { 2 } else { 1 };
                        for line in &lines[..asking] {
                            for offset in [0.0, 0.5] {
                                let t = ease_in_out((phase as f32 + offset) % 1.0);
                                draw_dot(snapshot, along(*line, t), fade(t));
                            }
                        }
                    }
                }
                Band::Success => self.draw_letters(snapshot),
                _ => {}
            }
            if band != Band::Idle {
                self.draw_stamps(snapshot, env);
            }
            if band.warns() {
                draw_badge(snapshot, env, self.badge.get() as f32);
            }
            snapshot.restore();
        }
    }

    impl Marks {
        fn draw_stamps(&self, snapshot: &gtk::Snapshot, env: Env) {
            let swap = self.swap.get() as f32;
            if let Some(old) = self.old_stamp.get()
                && swap < 1.0
            {
                draw_stamp(snapshot, env, old, None, 1.0 - swap);
            }
            if let Some(stamp) = self.stamp.get() {
                let flight = self
                    .flight_from
                    .get()
                    .map(|from| (from, self.flight.get() as f32));
                draw_stamp(snapshot, env, stamp, flight, swap);
            }
        }

        fn draw_blink(&self, snapshot: &gtk::Snapshot, env: Env) {
            let closed = (self.blink.get() as f32 * PI).sin();
            if closed <= 0.0 {
                return;
            }
            let (eyes, r) = env.eyes();
            for (cx, cy) in eyes {
                let cover = gsk::PathBuilder::new();
                cover.add_circle(&graphene::Point::new(cx, cy), r + 0.9);
                snapshot.append_fill(&cover.to_path(), gsk::FillRule::Winding, &rgba(FACE, 1.0));
                // The eye closes to a line, as a lid comes down.
                let lid = gsk::PathBuilder::new();
                let ry = (r * (1.0 - closed)).max(0.7);
                lid.add_rounded_rect(&gsk::RoundedRect::from_rect(
                    graphene::Rect::new(cx - r, cy - ry, 2.0 * r, 2.0 * ry),
                    ry,
                ));
                snapshot.append_fill(&lid.to_path(), gsk::FillRule::Winding, &rgba(INK, 1.0));
            }
        }

        fn draw_letters(&self, snapshot: &gtk::Snapshot) {
            let arc = letter_arc();
            match self.phase.get() {
                Some(phase) if self.letters.get() => {
                    for offset in [0.0, 0.5] {
                        let t = (phase as f32 + offset) % 1.0;
                        draw_letter(snapshot, along(arc, t), letter_angle(t), fade(t));
                    }
                }
                // At rest, two letters wait on the arc, as the mockup
                // draws them.
                _ => {
                    for t in [0.3, 0.66] {
                        draw_letter(snapshot, along(arc, t), letter_angle(t), 1.0);
                    }
                }
            }
        }
    }
}

/// How visible a travelling mark is at `t` along its path: it fades in
/// as it leaves and out as it arrives, so no mark pops.
fn fade(t: f32) -> f32 {
    (t / 0.15).min((1.0 - t) / 0.15).clamp(0.0, 1.0)
}

/// A mini letter turns as it glides, from -26° at the start to lying
/// flat as it drops in; the mockup has -18° at 0.3 and -8° at 0.66.
fn letter_angle(t: f32) -> f32 {
    -18.0 + (t - 0.3) * 10.0 / 0.36
}

fn draw_dot(snapshot: &gtk::Snapshot, (x, y): Point, alpha: f32) {
    let dot = gsk::PathBuilder::new();
    dot.add_circle(&graphene::Point::new(x, y), 3.4);
    snapshot.append_fill(&dot.to_path(), gsk::FillRule::Winding, &rgba(PAPER, alpha));
}

/// Three dots a third of the way apart walk the path to the browser, the
/// first brightest, as the mockup draws them.
fn draw_walking_dots(snapshot: &gtk::Snapshot, phase: f32) {
    let path = to_browser();
    for (index, strength) in [1.0, 0.75, 0.5].into_iter().enumerate() {
        let t = (phase + 1.0 - index as f32 / 3.0) % 1.0;
        draw_dot(snapshot, along(path, t), strength * fade(t));
    }
}

fn shadow(snapshot: &gtk::Snapshot, bounds: &gsk::RoundedRect, alpha: f32) {
    snapshot.append_outset_shadow(
        bounds,
        &gdk::RGBA::new(0.11, 0.098, 0.09, 0.28 * alpha),
        0.0,
        3.0,
        0.0,
        8.0,
    );
}

/// The provider's stamp on the envelope's top-right corner, turned 7°.
/// While `flight` runs it travels from where the tile was, turning and
/// growing into place.
fn draw_stamp(
    snapshot: &gtk::Snapshot,
    env: Env,
    stamp: Stamp,
    flight: Option<(Point, f32)>,
    alpha: f32,
) {
    if alpha <= 0.0 {
        return;
    }
    let (mut cx, mut cy, sw, sh) = env.stamp();
    let mut angle = 7.0;
    let mut grow = 1.0;
    if let Some(((fx, fy), t)) = flight {
        cx = fx + (cx - fx) * t;
        cy = fy + (cy - fy) * t;
        angle *= t;
        // The tile's mark is 38 px, a little larger than the stamp.
        grow = 1.0 + (38.0 / sw - 1.0) * (1.0 - t);
    }
    snapshot.save();
    snapshot.translate(&graphene::Point::new(cx, cy));
    snapshot.rotate(angle);
    snapshot.scale(grow, grow);
    snapshot.push_opacity(f64::from(alpha));
    let paper = graphene::Rect::new(-sw / 2.0, -sh / 2.0, sw, sh);
    let rounded = gsk::RoundedRect::from_rect(paper, 2.0);
    shadow(snapshot, &rounded, 1.0);
    let body = gsk::PathBuilder::new();
    body.add_rounded_rect(&rounded);
    let body = body.to_path();
    snapshot.append_fill(&body, gsk::FillRule::Winding, &rgba(PAPER, 1.0));
    // A dashed stroke in the paper's colour over the edge is the
    // perforation: the dashes reach past the edge and the gaps bite in.
    let perforation = gsk::Stroke::new(3.0);
    perforation.set_dash(&[2.4, 2.2]);
    snapshot.append_stroke(&body, &perforation, &rgba(PAPER, 1.0));
    let inset = sw * 0.14;
    let tile = gsk::PathBuilder::new();
    tile.add_rounded_rect(&gsk::RoundedRect::from_rect(
        graphene::Rect::new(-sw / 2.0 + inset, -sh / 2.0 + inset, sw - 2.0 * inset, sh - 2.0 * inset),
        2.0,
    ));
    snapshot.append_fill(&tile.to_path(), gsk::FillRule::Winding, &hex(stamp.colour));
    draw_initial(snapshot, stamp.letter, sw * 0.5, sw * 0.19);
    snapshot.pop();
    snapshot.restore();
}

/// The provider's initial, centred on (0, 0) with its baseline at
/// `baseline`, as the mockup sets it.
fn draw_initial(snapshot: &gtk::Snapshot, letter: char, size: f32, baseline: f32) {
    let fonts = pangocairo_context();
    let layout = pango::Layout::new(&fonts);
    let mut font = pango::FontDescription::from_string("Sans");
    font.set_weight(pango::Weight::Heavy);
    font.set_absolute_size(f64::from(size) * f64::from(pango::SCALE));
    layout.set_font_description(Some(&font));
    layout.set_text(&letter.to_string());
    let (_, logical) = layout.pixel_extents();
    let base = layout.baseline() as f32 / pango::SCALE as f32;
    snapshot.save();
    snapshot.translate(&graphene::Point::new(-(logical.width() as f32) / 2.0, baseline - base));
    snapshot.append_layout(&layout, &rgba(CREAM, 1.0));
    snapshot.restore();
}

fn pangocairo_context() -> pango::Context {
    thread_local! {
        static FONTS: pango::Context = {
            let label = gtk::Label::new(None);
            label.pango_context()
        };
    }
    FONTS.with(Clone::clone)
}

/// The warning badge on the bottom-right corner: an ink ring, an amber
/// disc and an ink mark. `grown` runs from 0 to 1 as it pops in, from
/// 0.8 of its size.
fn draw_badge(snapshot: &gtk::Snapshot, env: Env, grown: f32) {
    if grown <= 0.0 {
        return;
    }
    let r = env.w * 0.1;
    let (cx, cy) = (env.x + env.w - r * 0.5, env.top + env.h() - r * 0.3);
    let size = 0.8 + 0.2 * grown;
    snapshot.save();
    snapshot.translate(&graphene::Point::new(cx, cy));
    snapshot.scale(size, size);
    snapshot.push_opacity(f64::from(grown));
    let circle = |radius: f32, colour: gdk::RGBA| {
        let path = gsk::PathBuilder::new();
        path.add_circle(&graphene::Point::new(0.0, 0.0), radius);
        snapshot.append_fill(&path.to_path(), gsk::FillRule::Winding, &colour);
    };
    circle(r + 2.6, rgba(INK, 1.0));
    circle(r, rgba(AMBER, 1.0));
    let bar = gsk::PathBuilder::new();
    bar.add_rounded_rect(&gsk::RoundedRect::from_rect(
        graphene::Rect::new(-r * 0.13, -r * 0.58, r * 0.26, r * 0.72),
        r * 0.13,
    ));
    snapshot.append_fill(&bar.to_path(), gsk::FillRule::Winding, &rgba(INK, 1.0));
    let dot = gsk::PathBuilder::new();
    dot.add_circle(&graphene::Point::new(0.0, r * 0.47), r * 0.15);
    snapshot.append_fill(&dot.to_path(), gsk::FillRule::Winding, &rgba(INK, 1.0));
    snapshot.pop();
    snapshot.restore();
}

/// A mini letter, 26 by 18, turned `angle` degrees.
fn draw_letter(snapshot: &gtk::Snapshot, (cx, cy): Point, angle: f32, alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let (w, h) = (26.0, 18.0);
    snapshot.save();
    snapshot.translate(&graphene::Point::new(cx, cy));
    snapshot.rotate(angle);
    snapshot.push_opacity(f64::from(alpha));
    let rounded = gsk::RoundedRect::from_rect(graphene::Rect::new(-w / 2.0, -h / 2.0, w, h), 3.0);
    shadow(snapshot, &rounded, 1.0);
    let paper = gsk::PathBuilder::new();
    paper.add_rounded_rect(&rounded);
    snapshot.append_fill(&paper.to_path(), gsk::FillRule::Winding, &rgba(PAPER, 1.0));
    let flap = gsk::PathBuilder::new();
    flap.move_to(-w / 2.0 + 3.0, -h / 2.0 + 3.0);
    flap.line_to(0.0, 1.0);
    flap.line_to(w / 2.0 - 3.0, -h / 2.0 + 3.0);
    let stroke = gsk::Stroke::new(2.0);
    stroke.set_line_join(gsk::LineJoin::Round);
    stroke.set_line_cap(gsk::LineCap::Round);
    snapshot.append_stroke(&flap.to_path(), &stroke, &rgba(INK, 1.0));
    snapshot.pop();
    snapshot.restore();
}

glib::wrapper! {
    /// What moves on the band, drawn over its pose.
    pub struct Marks(ObjectSubclass<imp::Marks>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// Whether the desktop lets things move.
fn animations_on() -> bool {
    gtk::Settings::default().is_some_and(|s| s.is_gtk_enable_animations())
}

impl Marks {
    fn new(scale: f32) -> Marks {
        let marks: Marks = glib::Object::builder().property("can-target", false).build();
        marks.imp().scale.set(scale);
        marks.imp().swap.set(1.0);
        marks.imp().flight.set(1.0);
        marks.imp().badge.set(1.0);
        if let Some(settings) = gtk::Settings::default() {
            let weak = marks.downgrade();
            settings.connect_gtk_enable_animations_notify(move |_| {
                if let Some(marks) = weak.upgrade() {
                    marks.restart_loops();
                }
            });
        }
        marks
    }

    /// An animation of a value from 0 to 1 that stores it with `set` and
    /// redraws. The animation is kept until the next one of its kind.
    fn animate(&self, set: fn(&imp::Marks, f64)) -> adw::CallbackAnimationTarget {
        let weak = self.downgrade();
        adw::CallbackAnimationTarget::new(move |value| {
            if let Some(marks) = weak.upgrade() {
                set(marks.imp(), value);
                marks.queue_draw();
            }
        })
    }

    fn keep(&self, animation: impl IsA<adw::Animation>) {
        let animation = animation.upcast();
        animation.play();
        self.imp().loops.borrow_mut().push(animation);
    }

    /// Plays a one-off move and keeps it alive while it runs.
    fn play_move(&self, animation: impl IsA<adw::Animation>) {
        let animation = animation.upcast();
        animation.play();
        let mut moves = self.imp().moves.borrow_mut();
        moves.retain(|a| a.state() == adw::AnimationState::Playing);
        moves.push(animation);
    }

    fn stop_loops(&self) {
        let imp = self.imp();
        let running: Vec<adw::Animation> = imp.loops.borrow_mut().drain(..).collect();
        for animation in running {
            animation.skip();
        }
        if let Some(timer) = imp.blink_timer.borrow_mut().take() {
            timer.remove();
        }
        imp.phase.set(None);
        imp.blink.set(0.0);
        self.queue_draw();
    }

    /// Starts the loop the pose has, if it has one and the desktop lets
    /// things move. A loop runs only while the band is on screen.
    fn restart_loops(&self) {
        self.stop_loops();
        let imp = self.imp();
        if !self.is_mapped() || !animations_on() {
            return;
        }
        let Some(band) = imp.band.get() else {
            return;
        };
        let (period, easing) = match band {
            Band::Idle => return self.schedule_blink(),
            Band::Browser => (1600, adw::Easing::Linear),
            // The dots ease in and out themselves, since two run on one
            // line half a cycle apart.
            Band::Lookup | Band::Success => (1200, adw::Easing::Linear),
            _ => return,
        };
        if band == Band::Success && !imp.letters.get() {
            return;
        }
        imp.phase.set(Some(0.0));
        let target = self.animate(|imp, value| imp.phase.set(Some(value)));
        let looping = adw::TimedAnimation::builder()
            .widget(self)
            .value_from(0.0)
            .value_to(1.0)
            .duration(period)
            .repeat_count(0)
            .easing(easing)
            .target(&target)
            .build();
        self.keep(looping);
    }

    /// Blinks once in 6 to 9 seconds, 160 ms each time.
    fn schedule_blink(&self) {
        let wait = 6000 + u64::from(glib::random_int_range(0, 3000) as u32);
        let weak = self.downgrade();
        let timer = glib::timeout_add_local_once(Duration::from_millis(wait), move || {
            let Some(marks) = weak.upgrade() else {
                return;
            };
            marks.imp().blink_timer.replace(None);
            let target = marks.animate(|imp, value| imp.blink.set(value));
            let blink = adw::TimedAnimation::builder()
                .widget(&marks)
                .value_from(0.0)
                .value_to(1.0)
                .duration(160)
                .easing(adw::Easing::Linear)
                .target(&target)
                .build();
            marks.keep(blink);
            marks.schedule_blink();
        });
        self.imp().blink_timer.replace(Some(timer));
    }
}

/// The band: a taupe strip with the envelope's pose in the middle and the
/// marks over it. The whole band is one picture to a screen reader, named
/// for the state it shows.
pub struct PostBand {
    pub widget: gtk::Box,
    poses: gtk::Stack,
    marks: Marks,
    shown: Cell<Option<Band>>,
    stamp: Cell<Option<Stamp>>,
    provider: RefCell<Option<String>>,
}

impl PostBand {
    /// A band `height` tall, with the art drawn `scale` times its own
    /// size: 1 in the dialog, 1.25 across the first-run window.
    pub fn new(height: i32, scale: f32) -> PostBand {
        let poses = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(200)
            .can_target(false)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .build();
        let size = ((ART_W * scale) as i32, (ART_H * scale) as i32);
        for band in Band::ALL {
            let picture = gtk::Picture::builder()
                .content_fit(gtk::ContentFit::Contain)
                .can_shrink(false)
                .width_request(size.0)
                .height_request(size.1)
                .accessible_role(gtk::AccessibleRole::Presentation)
                .build();
            poses.add_named(&picture, Some(band.name()));
        }
        let marks = Marks::new(scale);
        marks.set_halign(gtk::Align::Center);
        marks.set_valign(gtk::Align::End);
        let art = gtk::Overlay::builder().child(&poses).build();
        art.add_overlay(&marks);
        let widget = gtk::Box::builder()
            .height_request(height)
            .valign(gtk::Align::Start)
            .accessible_role(gtk::AccessibleRole::Img)
            .css_classes(["post-band"])
            .build();
        // The art keeps its own width: a picture measured for the whole
        // band's width would keep its aspect and grow taller than the band.
        // It is centred by the band's own box, which gives a child that
        // does not expand only its natural width.
        let left = gtk::Box::builder().hexpand(true).build();
        let right = gtk::Box::builder().hexpand(true).build();
        art.set_valign(gtk::Align::End);
        widget.append(&left);
        widget.append(&art);
        widget.append(&right);
        let band = PostBand {
            widget,
            poses,
            marks,
            shown: Cell::new(None),
            stamp: Cell::new(None),
            provider: RefCell::new(None),
        };
        // The pictures are drawn from the SVGs at the size and the scale
        // the display wants, since a picture scaled up after loading goes
        // soft.
        let (poses, scale_up) = (band.poses.clone(), scale);
        band.widget.connect_realize(move |widget| {
            load_poses(&poses, scale_up, widget.scale_factor());
        });
        let (poses, scale_up) = (band.poses.clone(), scale);
        band.widget.connect_scale_factor_notify(move |widget| {
            load_poses(&poses, scale_up, widget.scale_factor());
        });
        band
    }

    /// Shows `band`, with `stamp` on the corner and `provider` named to
    /// a screen reader. A new pose crossfades in; a new stamp fades over
    /// the old one; a warning pose grows its badge in.
    pub fn show(&self, band: Band, stamp: Option<Stamp>, provider: Option<&str>) {
        let before = self.shown.replace(Some(band));
        let imp = self.marks.imp();
        self.poses
            .set_transition_duration(if band == Band::Success { 250 } else { 200 });
        self.poses.set_visible_child_name(band.name());
        let old = self.stamp.replace(stamp);
        if old != stamp {
            imp.old_stamp.set(old);
            imp.stamp.set(stamp);
            if old.is_some() && stamp.is_some() && before.is_some() && imp.flight_from.get().is_none() {
                self.run(150, adw::Easing::EaseOutCubic, |imp, v| imp.swap.set(v));
            } else {
                imp.swap.set(1.0);
            }
        }
        if band.warns() && before.is_none_or(|b| !b.warns()) {
            self.run(200, adw::Easing::EaseOutCubic, |imp, v| imp.badge.set(v));
        }
        imp.band.set(Some(band));
        self.provider.replace(provider.map(str::to_string));
        self.widget
            .update_property(&[gtk::accessible::Property::Label(&band.described(provider))]);
        if before != Some(band) {
            self.marks.restart_loops();
        }
        self.marks.queue_draw();
    }

    /// Flies the stamp in from `from`, a point in `widget`, on a
    /// critically damped spring with a 0.35 s response.
    pub fn fly_from(&self, widget: &impl IsA<gtk::Widget>, from: &graphene::Point) {
        let Some(point) = widget.compute_point(&self.marks, from) else {
            return;
        };
        let imp = self.marks.imp();
        let scale = imp.scale.get().max(1.0);
        let left = (self.marks.width() as f32 - ART_W * scale) / 2.0;
        let top = (self.marks.height() as f32 - ART_H * scale) / 2.0;
        let x = (point.x() - left) / scale;
        // The page below covers the band's art under its bottom edge, so
        // the stamp starts where it first shows, at the tile's x.
        let y = ((point.y() - top) / scale).min(ART_H + 20.0);
        imp.flight_from.set(Some((x, y)));
        imp.swap.set(1.0);
        imp.old_stamp.set(None);
        let target = self.marks.animate(|imp, value| imp.flight.set(value));
        let spring = adw::SpringAnimation::builder()
            .widget(&self.marks)
            .value_from(0.0)
            .value_to(1.0)
            .spring_params(&adw::SpringParams::new(1.0, 1.0, 322.0))
            .target(&target)
            .build();
        let weak = self.marks.downgrade();
        spring.connect_done(move |_| {
            if let Some(marks) = weak.upgrade() {
                marks.imp().flight_from.set(None);
                marks.queue_draw();
            }
        });
        imp.flight.set(0.0);
        self.marks.play_move(spring);
    }

    /// Lets the lower lookup line ask too, once MX has answered.
    pub fn set_lower_asking(&self, asking: bool) {
        self.marks.imp().lower.set(asking);
        self.marks.queue_draw();
    }

    /// Whether letters glide in on the success pose: while the first sync
    /// runs.
    pub fn set_letters(&self, running: bool) {
        let imp = self.marks.imp();
        if imp.letters.replace(running) != running {
            self.marks.restart_loops();
        }
    }

    fn run(&self, ms: u32, easing: adw::Easing, set: fn(&imp::Marks, f64)) {
        let target = self.marks.animate(set);
        set(self.marks.imp(), 0.0);
        let animation = adw::TimedAnimation::builder()
            .widget(&self.marks)
            .value_from(0.0)
            .value_to(1.0)
            .duration(ms)
            .easing(easing)
            .target(&target)
            .build();
        self.marks.play_move(animation);
    }
}

/// Draws each pose's SVG into its picture at `scale` times the art's
/// size, for a display at `factor`.
fn load_poses(poses: &gtk::Stack, scale: f32, factor: i32) {
    let width = (ART_W * scale * factor as f32).round() as i32;
    let height = (ART_H * scale * factor as f32).round() as i32;
    for band in Band::ALL {
        let Some(picture) = poses
            .child_by_name(band.name())
            .and_downcast::<gtk::Picture>()
        else {
            continue;
        };
        let path = format!("/io/github/c9dev/PenguinMail/band/{}.svg", band.name());
        match gtk::gdk_pixbuf::Pixbuf::from_resource_at_scale(&path, width, height, true) {
            #[allow(deprecated)]
            Ok(pixbuf) => picture.set_paintable(Some(&gdk::Texture::for_pixbuf(&pixbuf))),
            Err(err) => tracing::warn!(%path, error = %err, "could not draw a band pose"),
        }
    }
}
