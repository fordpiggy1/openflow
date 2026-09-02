//! The overlay pill: a borderless `NSPanel` holding one custom-drawn view.
//!
//! Geometry, colours and animation are ported from `overlay.html` rather than
//! reinvented, so the native build and the Tauri build look identical: 28 px
//! tall, 28 / 82 / 72 px wide for idle / recording / transcribing, the same
//! `rgba(26,19,50,0.94)` body, the same per-position corner radii, the same ten
//! waveform bars and three pulsing dots.
//!
//! The only thing that ticks is one `NSTimer` at 30 Hz, and it exists only
//! while the pill is animating. On idle it is invalidated, which is what keeps
//! the app at the measured idle floor.

use std::cell::{Cell, RefCell};
use std::f64::consts::PI;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBezierPath, NSColor, NSEvent, NSPanel, NSScreen, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSTimer};

use openflow_core::engine::{Engine, RecordingState};

// ── Geometry, ported from overlay.html ────────────────────

/// The pill is always this tall; only its width changes.
pub const OVERLAY_HEIGHT: f64 = 28.0;
pub const WIDTH_IDLE: f64 = 28.0;
pub const WIDTH_RECORDING: f64 = 82.0;
pub const WIDTH_TRANSCRIBING: f64 = 72.0;
/// The corner radius `overlay.html` uses on whichever corners face the screen.
pub const CORNER_RADIUS: f64 = 10.0;

/// The eight anchors, as fractions of the work area in the HTML's coordinate
/// space: x from left, y from *top*.
pub const POSITIONS: &[(&str, f64, f64)] = &[
    ("top-left", 0.0, 0.0),
    ("top-center", 0.5, 0.0),
    ("top-right", 1.0, 0.0),
    ("left-center", 0.0, 0.5),
    ("right-center", 1.0, 0.5),
    ("bottom-left", 0.0, 1.0),
    ("bottom-center", 0.5, 1.0),
    ("bottom-right", 1.0, 1.0),
];

pub fn is_known_position(name: &str) -> bool {
    POSITIONS.iter().any(|(known, _, _)| *known == name)
}

/// A rectangle in AppKit screen coordinates: origin bottom-left, y upwards.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

pub fn width_for(state: RecordingState) -> f64 {
    match state {
        RecordingState::Recording => WIDTH_RECORDING,
        RecordingState::Transcribing => WIDTH_TRANSCRIBING,
        // `Formatting` is never emitted; if a future host does emit it, resting
        // width is the honest answer rather than a fourth shape nobody drew.
        _ => WIDTH_IDLE,
    }
}

/// Where the pill's bottom-left corner goes, for a window `width` wide inside
/// `visible` (the screen minus menu bar and Dock).
///
/// `overlay.html` measures y downwards from the top of the work area; AppKit
/// measures upwards from the bottom, so the y fraction is inverted here and
/// nowhere else.
pub fn anchor_for(position: &str, width: f64, visible: Rect) -> (f64, f64) {
    let (_, fx, fy) = POSITIONS
        .iter()
        .find(|(name, _, _)| *name == position)
        .copied()
        .unwrap_or(("left-center", 0.0, 0.5));
    let span_x = (visible.width - width).max(0.0);
    let span_y = (visible.height - OVERLAY_HEIGHT).max(0.0);
    let x = visible.x + fx * span_x;
    let y = visible.y + (1.0 - fy) * span_y;
    (x, y)
}

/// The anchor a pill dropped at `origin` is closest to, by the same normalized
/// distance `overlay.html` uses when a drag ends.
pub fn nearest_position(origin: (f64, f64), width: f64, visible: Rect) -> &'static str {
    let span_x = (visible.width - width).max(1.0);
    let span_y = (visible.height - OVERLAY_HEIGHT).max(1.0);
    let px = (origin.0 - visible.x) / span_x;
    // Back to the HTML's top-down y before comparing with the table.
    let py = 1.0 - (origin.1 - visible.y) / span_y;

    let mut nearest = "left-center";
    let mut best = f64::INFINITY;
    for (name, fx, fy) in POSITIONS {
        let distance = ((px - fx).powi(2) + (py - fy).powi(2)).sqrt();
        if distance < best {
            best = distance;
            nearest = name;
        }
    }
    nearest
}

/// Corner radii in visual order: top-left, top-right, bottom-right, bottom-left,
/// exactly as the `border-radius` shorthand in `overlay.html` writes them.
pub fn corner_radii(position: &str) -> [f64; 4] {
    let r = CORNER_RADIUS;
    match position {
        "top-left" => [0.0, 0.0, r, 0.0],
        "top-center" => [0.0, 0.0, r, r],
        "top-right" => [0.0, 0.0, 0.0, r],
        "right-center" => [r, 0.0, 0.0, r],
        "bottom-left" => [0.0, r, 0.0, 0.0],
        "bottom-center" => [r, r, 0.0, 0.0],
        "bottom-right" => [r, 0.0, 0.0, 0.0],
        // left-center is the default, and the default's radii are its own.
        _ => [0.0, r, r, 0.0],
    }
}

// ── Animation, ported from the CSS keyframes ──────────────

/// `animation-delay` per bar, in seconds, in document order.
const WAVE_DELAYS: [f64; 10] = [0.0, 0.07, 0.14, 0.1, 0.03, 0.18, 0.06, 0.12, 0.16, 0.09];
/// `wave` runs 0.45s and alternates, so a full cycle is twice that.
const WAVE_PERIOD: f64 = 0.9;
const WAVE_MIN_HEIGHT: f64 = 4.0;
const WAVE_MAX_HEIGHT: f64 = 16.0;
const DOT_DELAYS: [f64; 3] = [0.0, 0.15, 0.3];
const DOT_PERIOD: f64 = 0.8;

/// `ease-in-out` on a 0..1 progress, which is what the CSS timing function
/// approximates closely enough at 30 Hz.
fn ease_in_out(t: f64) -> f64 {
    0.5 - 0.5 * (PI * t).cos()
}

/// Height of waveform bar `index` at `elapsed` seconds.
pub fn wave_height(index: usize, elapsed: f64) -> f64 {
    let delay = WAVE_DELAYS[index % WAVE_DELAYS.len()];
    let phase = (elapsed + delay).rem_euclid(WAVE_PERIOD) / (WAVE_PERIOD / 2.0);
    let triangle = if phase <= 1.0 { phase } else { 2.0 - phase };
    WAVE_MIN_HEIGHT + (WAVE_MAX_HEIGHT - WAVE_MIN_HEIGHT) * ease_in_out(triangle)
}

/// Opacity of transcribing dot `index` at `elapsed` seconds: 0.25 at the ends
/// of the cycle, 1.0 in the middle.
pub fn dot_opacity(index: usize, elapsed: f64) -> f64 {
    let delay = DOT_DELAYS[index % DOT_DELAYS.len()];
    let phase = (elapsed + delay).rem_euclid(DOT_PERIOD) / DOT_PERIOD;
    0.25 + 0.75
        * ease_in_out(if phase <= 0.5 {
            phase * 2.0
        } else {
            (1.0 - phase) * 2.0
        })
}

// ── The view ──────────────────────────────────────────────

pub struct PillIvars {
    state: Cell<RecordingState>,
    position: RefCell<String>,
    /// Seconds since the current animation started.
    elapsed: Cell<f64>,
    /// Where the pointer sat inside the window when a drag began.
    drag_offset: Cell<Option<NSPoint>>,
    engine: RefCell<Option<Arc<Engine>>>,
}

define_class!(
    // SAFETY: NSView permits subclassing and this class overrides only drawing
    // and mouse handling. It has no Drop impl.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "OpenFlowPillView"]
    #[ivars = PillIvars]
    struct PillView;

    impl PillView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = { event.locationInWindow() };
            self.ivars().drag_offset.set(Some(point));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let Some(offset) = self.ivars().drag_offset.get() else {
                return;
            };
            let Some(window) = self.window() else { return };
            let in_window = { event.locationInWindow() };
            let frame = window.frame();
            let origin = NSPoint::new(
                frame.origin.x + in_window.x - offset.x,
                frame.origin.y + in_window.y - offset.y,
            );
            window.setFrameOrigin(origin);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            if self.ivars().drag_offset.take().is_none() {
                return;
            }
            self.settle();
        }
    }
);

impl PillView {
    fn new(mtm: MainThreadMarker, position: String) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(PillIvars {
            state: Cell::new(RecordingState::Idle),
            position: RefCell::new(position),
            elapsed: Cell::new(0.0),
            drag_offset: Cell::new(None),
            engine: RefCell::new(None),
        });
        let frame = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(WIDTH_IDLE, OVERLAY_HEIGHT),
        );
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        this.setWantsLayer(true);
        this
    }

    /// Snap to the nearest anchor and persist it, the way `finishDrag` does.
    fn settle(&self) {
        let Some(window) = self.window() else { return };
        let Some(visible) = visible_frame(&window) else {
            return;
        };
        let frame = window.frame();
        let nearest = nearest_position((frame.origin.x, frame.origin.y), frame.size.width, visible);
        *self.ivars().position.borrow_mut() = nearest.to_string();
        let (x, y) = anchor_for(nearest, frame.size.width, visible);
        window.setFrameOrigin(NSPoint::new(x, y));
        self.setNeedsDisplay(true);

        if let Some(engine) = self.ivars().engine.borrow().as_ref() {
            let _ = engine.settings().set("overlay_position", nearest);
        }
    }

    fn draw(&self) {
        let bounds = self.bounds();
        let state = self.ivars().state.get();
        let position = self.ivars().position.borrow().clone();
        let elapsed = self.ivars().elapsed.get();

        // Body: rgba(26,19,50,0.94) with a 1 px border whose colour is the
        // state's accent, inset by half the stroke so the stroke stays inside.
        let inset = NSRect::new(
            NSPoint::new(bounds.origin.x + 0.5, bounds.origin.y + 0.5),
            NSSize::new(
                (bounds.size.width - 1.0).max(0.0),
                (bounds.size.height - 1.0).max(0.0),
            ),
        );
        let path = rounded_path(inset, corner_radii(&position));
        {
            NSColor::colorWithSRGBRed_green_blue_alpha(
                26.0 / 255.0,
                19.0 / 255.0,
                50.0 / 255.0,
                0.94,
            )
            .set();
        }
        path.fill();
        let (br, bg, bb, ba) = match state {
            RecordingState::Recording => (239.0, 68.0, 68.0, 0.5),
            RecordingState::Transcribing => (123.0, 163.0, 201.0, 0.4),
            _ => (255.0, 255.0, 255.0, 0.06),
        };
        {
            NSColor::colorWithSRGBRed_green_blue_alpha(br / 255.0, bg / 255.0, bb / 255.0, ba)
                .set();
        }
        path.setLineWidth(1.0);
        path.stroke();

        // Logo: 14x14, centred when idle, 6 px from the left edge otherwise.
        let logo_size = 14.0;
        let logo_x = if matches!(
            state,
            RecordingState::Recording | RecordingState::Transcribing
        ) {
            6.0
        } else {
            (bounds.size.width - logo_size) / 2.0
        };
        let logo_y = (bounds.size.height - logo_size) / 2.0;
        let (lr, lg, lb, la) = match state {
            RecordingState::Recording => (239.0, 68.0, 68.0, 1.0),
            RecordingState::Transcribing => (123.0, 163.0, 201.0, 1.0),
            _ => (255.0, 255.0, 255.0, 0.7),
        };
        {
            NSColor::colorWithSRGBRed_green_blue_alpha(lr / 255.0, lg / 255.0, lb / 255.0, la)
                .set();
        }
        draw_microphone(NSRect::new(
            NSPoint::new(logo_x, logo_y),
            NSSize::new(logo_size, logo_size),
        ));

        match state {
            RecordingState::Recording => self.draw_waveform(bounds, logo_x + logo_size, elapsed),
            RecordingState::Transcribing => self.draw_dots(bounds, logo_x + logo_size, elapsed),
            _ => {}
        }
    }

    /// Ten 2.5 px bars, 2 px apart, growing from 4 px to 16 px.
    fn draw_waveform(&self, bounds: NSRect, after_logo: f64, elapsed: f64) {
        {
            NSColor::colorWithSRGBRed_green_blue_alpha(
                239.0 / 255.0,
                68.0 / 255.0,
                68.0 / 255.0,
                1.0,
            )
            .set();
        }
        let bar_width = 2.5;
        let gap = 2.0;
        let mut x = after_logo + 6.0;
        for index in 0..WAVE_DELAYS.len() {
            let height = wave_height(index, elapsed);
            let y = (bounds.size.height - height) / 2.0;
            let bar = NSRect::new(NSPoint::new(x, y), NSSize::new(bar_width, height));
            let path = { NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(bar, 1.25, 1.25) };
            path.fill();
            x += bar_width + gap;
        }
    }

    /// Three 4 px dots, 3 px apart, pulsing between 0.25 and 1.0 opacity.
    fn draw_dots(&self, bounds: NSRect, after_logo: f64, elapsed: f64) {
        let diameter = 4.0;
        let gap = 3.0;
        let mut x = after_logo + 6.0;
        let y = (bounds.size.height - diameter) / 2.0;
        for index in 0..DOT_DELAYS.len() {
            {
                NSColor::colorWithSRGBRed_green_blue_alpha(
                    123.0 / 255.0,
                    163.0 / 255.0,
                    201.0 / 255.0,
                    dot_opacity(index, elapsed),
                )
                .set();
            }
            let dot = NSRect::new(NSPoint::new(x, y), NSSize::new(diameter, diameter));
            { NSBezierPath::bezierPathWithOvalInRect(dot) }.fill();
            x += diameter + gap;
        }
    }
}

/// The mic glyph from `overlay.html`, drawn as primitives rather than parsed
/// from the SVG path: a capsule for the body, an arc for the pickup ring, and a
/// stem. At 14 px the two are indistinguishable, and this needs no path parser.
fn draw_microphone(frame: NSRect) {
    let unit = frame.size.width / 24.0;
    let x = |v: f64| frame.origin.x + v * unit;
    let y = |v: f64| frame.origin.y + (24.0 - v) * unit;

    // Body: rounded rect from (9,1) to (15,13), radius 3.
    let body = NSRect::new(
        NSPoint::new(x(9.0), y(13.0)),
        NSSize::new(6.0 * unit, 12.0 * unit),
    );
    let capsule =
        { NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(body, 3.0 * unit, 3.0 * unit) };
    capsule.fill();

    // Pickup ring: the open U from (5,10) round to (19,10), 2 px thick.
    let ring = NSBezierPath::new();
    {
        ring.setLineWidth(2.0 * unit);
        ring.moveToPoint(NSPoint::new(x(4.0), y(10.0)));
        ring.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle(
            NSPoint::new(x(12.0), y(12.0)),
            8.0 * unit,
            180.0,
            360.0,
        );
    }
    ring.stroke();

    // Stem and base.
    let stem = NSRect::new(
        NSPoint::new(x(11.0), y(23.0)),
        NSSize::new(2.0 * unit, 3.0 * unit),
    );
    { NSBezierPath::bezierPathWithRect(stem) }.fill();
}

/// A rounded rectangle with per-corner radii, in visual order (top-left,
/// top-right, bottom-right, bottom-left). `NSBezierPath`'s own rounded-rect
/// helper only takes one radius, and every position in `overlay.html` rounds a
/// different pair of corners.
fn rounded_path(rect: NSRect, radii: [f64; 4]) -> Retained<NSBezierPath> {
    let [tl, tr, br, bl] = radii;
    let left = rect.origin.x;
    let bottom = rect.origin.y;
    let right = left + rect.size.width;
    let top = bottom + rect.size.height;
    let path = NSBezierPath::new();
    {
        path.moveToPoint(NSPoint::new(left + tl, top));
        path.lineToPoint(NSPoint::new(right - tr, top));
        if tr > 0.0 {
            path.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle(
                NSPoint::new(right - tr, top - tr),
                tr,
                90.0,
                0.0,
            );
        }
        path.lineToPoint(NSPoint::new(right, bottom + br));
        if br > 0.0 {
            path.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle(
                NSPoint::new(right - br, bottom + br),
                br,
                0.0,
                -90.0,
            );
        }
        path.lineToPoint(NSPoint::new(left + bl, bottom));
        if bl > 0.0 {
            path.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle(
                NSPoint::new(left + bl, bottom + bl),
                bl,
                270.0,
                180.0,
            );
        }
        path.lineToPoint(NSPoint::new(left, top - tl));
        if tl > 0.0 {
            path.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle(
                NSPoint::new(left + tl, top - tl),
                tl,
                180.0,
                90.0,
            );
        }
        path.closePath();
    }
    path
}

fn visible_frame(window: &NSWindow) -> Option<Rect> {
    let screen = window
        .screen()
        .or_else(|| MainThreadMarker::new().and_then(NSScreen::mainScreen))?;
    let frame = screen.visibleFrame();
    Some(Rect {
        x: frame.origin.x,
        y: frame.origin.y,
        width: frame.size.width,
        height: frame.size.height,
    })
}

// ── The panel ─────────────────────────────────────────────

pub struct Overlay {
    panel: Retained<NSPanel>,
    view: Retained<PillView>,
    timer: RefCell<Option<Retained<NSTimer>>>,
    state: Cell<RecordingState>,
    engine: Arc<Engine>,
}

impl Overlay {
    pub fn new(engine: &Arc<Engine>, mtm: MainThreadMarker) -> Self {
        let mut position = engine.settings().overlay_position();
        if !is_known_position(&position) {
            position = "left-center".to_string();
        }

        let content = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(WIDTH_IDLE, OVERLAY_HEIGHT),
        );
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let panel = {
            NSPanel::initWithContentRect_styleMask_backing_defer(
                NSPanel::alloc(mtm),
                content,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // Above every ordinary window, on every Space, and never a full-screen
        // app's problem. `NSStatusWindowLevel` is 25.
        panel.setLevel(25);
        panel.setOpaque(false);
        panel.setHasShadow(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        panel.setIgnoresMouseEvents(false);
        panel.setMovableByWindowBackground(false);
        panel.setFloatingPanel(true);
        panel.setHidesOnDeactivate(false);

        let view = PillView::new(mtm, position.clone());
        *view.ivars().engine.borrow_mut() = Some(Arc::clone(engine));
        panel.setContentView(Some(&view));

        let overlay = Self {
            panel,
            view,
            timer: RefCell::new(None),
            state: Cell::new(RecordingState::Idle),
            engine: Arc::clone(engine),
        };
        overlay.snap(WIDTH_IDLE, false);
        overlay.panel.orderFrontRegardless();
        overlay
    }

    /// Read the hide-when-idle setting fresh, like `overlay.html` does, so the
    /// toggle takes effect the moment it is saved.
    pub fn apply_visibility_setting(&self) {
        let hide_when_idle = self.engine.settings().overlay_only_while_recording();
        let idle = matches!(self.state.get(), RecordingState::Idle);
        if hide_when_idle && idle {
            self.panel.orderOut(None);
        } else {
            self.panel.orderFrontRegardless();
        }
    }

    /// Move the pill to whichever anchor the settings window just chose.
    pub fn set_position(&self, position: &str) {
        if !is_known_position(position) {
            return;
        }
        *self.view.ivars().position.borrow_mut() = position.to_string();
        self.snap(self.panel.frame().size.width, false);
        self.view.setNeedsDisplay(true);
    }

    pub fn set_state(&self, state: RecordingState) {
        let previous = self.state.get();
        self.state.set(state);
        self.view.ivars().state.set(state);
        if previous != state {
            self.view.ivars().elapsed.set(0.0);
        }

        self.snap(width_for(state), previous != state);
        self.view.setNeedsDisplay(true);

        match state {
            RecordingState::Recording | RecordingState::Transcribing => self.start_animating(),
            _ => self.stop_animating(),
        }
        self.apply_visibility_setting();
    }

    /// Resize and re-anchor. `animated` runs it inside an `NSAnimationContext`
    /// so the width change eases the way the CSS transition does, without
    /// blocking the main thread the way `setFrame:display:animate:` would.
    fn snap(&self, width: f64, animated: bool) {
        let Some(visible) = visible_frame(&self.panel) else {
            return;
        };
        let position = self.view.ivars().position.borrow().clone();
        let (x, y) = anchor_for(&position, width, visible);
        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(width, OVERLAY_HEIGHT));
        if animated {
            crate::ui::animate(0.2, || {
                let animator: Retained<NSPanel> = unsafe { msg_send![&*self.panel, animator] };
                animator.setFrame_display(frame, true);
            });
        } else {
            self.panel.setFrame_display(frame, true);
        }
    }

    /// One 30 Hz timer, and only while something is moving.
    fn start_animating(&self) {
        if self.timer.borrow().is_some() {
            return;
        }
        let view = self.view.clone();
        let block = block2::RcBlock::new(move |_timer: core::ptr::NonNull<NSTimer>| {
            let ivars = view.ivars();
            ivars.elapsed.set(ivars.elapsed.get() + 1.0 / 30.0);
            view.setNeedsDisplay(true);
        });
        // Built unscheduled and added in the common modes, not scheduled in the
        // default mode: a timer in the default mode alone stops firing while a
        // menu is tracking, so opening the menu bar item mid-recording froze
        // the waveform until the menu closed.
        let timer =
            unsafe { NSTimer::timerWithTimeInterval_repeats_block(1.0 / 30.0, true, &block) };
        unsafe {
            NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
        }
        *self.timer.borrow_mut() = Some(timer);
    }

    fn stop_animating(&self) {
        if let Some(timer) = self.timer.borrow_mut().take() {
            timer.invalidate();
        }
        self.view.ivars().elapsed.set(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect {
        x: 0.0,
        y: 25.0,
        width: 1440.0,
        height: 875.0,
    };

    /// The eight anchors have to land on the edges of the *work area*, not the
    /// screen: a pill under the menu bar or behind the Dock is unreachable.
    #[test]
    fn anchors_sit_on_the_work_area_edges() {
        // left-center: hard left, vertically centred.
        let (x, y) = anchor_for("left-center", WIDTH_IDLE, SCREEN);
        assert_eq!(x, 0.0);
        assert_eq!(y, 25.0 + (875.0 - OVERLAY_HEIGHT) / 2.0);

        // top-left is the *top* of the work area in AppKit's upward y.
        let (x, y) = anchor_for("top-left", WIDTH_IDLE, SCREEN);
        assert_eq!(x, 0.0);
        assert_eq!(y, 25.0 + 875.0 - OVERLAY_HEIGHT);

        // bottom-right is the origin corner of the work area, minus the width.
        let (x, y) = anchor_for("bottom-right", WIDTH_RECORDING, SCREEN);
        assert_eq!(x, 1440.0 - WIDTH_RECORDING);
        assert_eq!(y, 25.0);

        // top-center splits the horizontal span.
        let (x, y) = anchor_for("top-center", WIDTH_TRANSCRIBING, SCREEN);
        assert_eq!(x, (1440.0 - WIDTH_TRANSCRIBING) / 2.0);
        assert_eq!(y, 25.0 + 875.0 - OVERLAY_HEIGHT);

        // An anchor nobody defined falls back to the shipped default rather
        // than to (0,0), which would hide the pill behind the Dock.
        assert_eq!(
            anchor_for("nowhere", WIDTH_IDLE, SCREEN),
            anchor_for("left-center", WIDTH_IDLE, SCREEN)
        );
    }

    /// Every anchor must round-trip: drop the pill exactly on an anchor and the
    /// drag handler has to name that same anchor back.
    #[test]
    fn every_anchor_is_its_own_nearest() {
        for (name, _, _) in POSITIONS {
            let origin = anchor_for(name, WIDTH_IDLE, SCREEN);
            assert_eq!(
                nearest_position(origin, WIDTH_IDLE, SCREEN),
                *name,
                "{name} should snap back to itself"
            );
        }
    }

    /// And a drop that is not on an anchor has to pick the near one, not just
    /// any one: without this the round-trip above would pass for a function
    /// that always returned the input.
    #[test]
    fn a_drop_near_an_edge_snaps_to_that_edge() {
        // 20 px in from the left, a third of the way down: nearest is left-center.
        let origin = (SCREEN.x + 20.0, SCREEN.y + SCREEN.height * 0.45);
        assert_eq!(nearest_position(origin, WIDTH_IDLE, SCREEN), "left-center");

        // Top-right corner of the work area.
        let origin = (
            SCREEN.x + SCREEN.width - WIDTH_IDLE,
            SCREEN.y + SCREEN.height,
        );
        assert_eq!(nearest_position(origin, WIDTH_IDLE, SCREEN), "top-right");

        // Middle of the bottom edge.
        let origin = (SCREEN.x + (SCREEN.width - WIDTH_IDLE) / 2.0, SCREEN.y);
        assert_eq!(
            nearest_position(origin, WIDTH_IDLE, SCREEN),
            "bottom-center"
        );
    }

    #[test]
    fn widths_match_the_overlay_stylesheet() {
        assert_eq!(width_for(RecordingState::Idle), 28.0);
        assert_eq!(width_for(RecordingState::Recording), 82.0);
        assert_eq!(width_for(RecordingState::Transcribing), 72.0);
        // Never emitted, and it must not invent a fourth width if it ever is.
        assert_eq!(width_for(RecordingState::Formatting), 28.0);
    }

    /// The radii are the `border-radius` shorthand from `overlay.html`, in the
    /// same order, so the pill's rounded corners always face the screen.
    #[test]
    fn corner_radii_face_away_from_the_screen_edge() {
        assert_eq!(corner_radii("left-center"), [0.0, 10.0, 10.0, 0.0]);
        assert_eq!(corner_radii("right-center"), [10.0, 0.0, 0.0, 10.0]);
        assert_eq!(corner_radii("top-left"), [0.0, 0.0, 10.0, 0.0]);
        assert_eq!(corner_radii("top-center"), [0.0, 0.0, 10.0, 10.0]);
        assert_eq!(corner_radii("top-right"), [0.0, 0.0, 0.0, 10.0]);
        assert_eq!(corner_radii("bottom-left"), [0.0, 10.0, 0.0, 0.0]);
        assert_eq!(corner_radii("bottom-center"), [10.0, 10.0, 0.0, 0.0]);
        assert_eq!(corner_radii("bottom-right"), [10.0, 0.0, 0.0, 0.0]);
        assert_eq!(corner_radii("nowhere"), corner_radii("left-center"));
    }

    #[test]
    fn the_waveform_stays_inside_the_css_keyframes() {
        for step in 0..90 {
            let elapsed = step as f64 / 30.0;
            for index in 0..10 {
                let height = wave_height(index, elapsed);
                assert!(
                    (WAVE_MIN_HEIGHT..=WAVE_MAX_HEIGHT).contains(&height),
                    "bar {index} at {elapsed}s was {height}"
                );
            }
            for index in 0..3 {
                let opacity = dot_opacity(index, elapsed);
                assert!(
                    (0.25..=1.0).contains(&opacity),
                    "dot {index} at {elapsed}s was {opacity}"
                );
            }
        }
        // The bars have to actually move, or the assertions above would hold
        // for a constant.
        assert!((wave_height(0, 0.0) - wave_height(0, 0.225)).abs() > 1.0);
        assert!((dot_opacity(0, 0.0) - dot_opacity(0, 0.4)).abs() > 0.5);
    }
}
