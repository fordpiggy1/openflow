//! The Dictate page: the web build's main screen, which the native host never
//! had.
//!
//! Everything here already existed in `src/App.tsx`; the strings are that
//! screen's, verbatim, because the two hosts are meant to be the same app. What
//! is new is that a native window now has a way in that is not the menu bar:
//! the big button drives the same `hotkey_pressed` / `hotkey_released` pair the
//! global shortcut does, so holding it is holding the shortcut, down to the
//! silence gate and the live preview on the pill.
//!
//! The button is a subclass rather than a plain `NSButton` because a button
//! cannot express "hold". `NSButton`'s action fires once, on mouse-up, and its
//! `mouseDown:` runs a tracking loop that never returns until the mouse is
//! released -- so the press half of a press-and-hold is unreachable through the
//! ordinary target/action path. [`HoldButton`] overrides both halves and does
//! not call super, which is what makes the down edge and the up edge separate
//! events.
//!
//! It answers the keyboard for the same reason the web screen does. A control
//! whose only gesture is "hold the mouse down on it" cannot be operated from
//! the keyboard at all, and `AXPress` -- which is what assistive technology and
//! every scripted click send -- fires an action this button does not have, so
//! it would do nothing. Space and Return press and release it, `isARepeat`
//! drops the auto-repeat of a held key so the press is not delivered twice, and
//! the Dictate page takes first responder when it comes forward so the key
//! reaches the button without a Tab first.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBezelStyle, NSButton, NSColor, NSControl, NSEvent, NSFont,
    NSImage, NSImageSymbolConfiguration, NSImageView, NSLevelIndicator, NSLevelIndicatorStyle,
    NSTextAlignment, NSTextField, NSView,
};
use objc2_foundation::{NSObject, NSPoint, NSRect, NSSize, NSString, NSTimer};

use openflow_core::audio::{meter_decay, meter_fraction};
use openflow_core::engine::{Engine, RecordingState};

use crate::hotkeys;
use crate::ui::card::{Card, GAP, MARGIN, PADDING};
use crate::ui::{allow_wrapping, note};

/// The recorder block's total height: glyph, three lines of copy, the button,
/// the cancel button and the hint, with the gaps between them. Kept as one
/// number so the block can be centred in a card of any height; the layout below
/// walks down from the top of it and has to add up to this.
const BLOCK_HEIGHT: f64 = 56.0
    + 18.0
    + 14.0
    + 6.0
    + 30.0
    + 8.0
    + 34.0
    + 20.0
    + 40.0
    + 10.0
    + METER_HEIGHT
    + 12.0
    + 24.0
    + 14.0
    + 14.0;

/// The level meter under the record button: the width of the button it belongs
/// to, and thin enough to read as a readout rather than a second control.
const METER_WIDTH: f64 = 220.0;
const METER_HEIGHT: f64 = 8.0;
/// How often the meter re-reads the microphone. Thirty a second is the rate a
/// moving bar needs to look continuous; the read behind it is one atomic load,
/// so the cost is the redraw and nothing else.
const METER_INTERVAL: f64 = 1.0 / 30.0;

/// The transcript's own height inside the result card: three lines at the
/// system font. The preview is cut at `RESULT_CHARS`, which is about that.
const RESULT_LINES: f64 = 54.0;
/// Height of the card that shows the last result: a caption, the gap under it,
/// the three lines, and the padding round all of it.
const RESULT_HEIGHT: f64 = PADDING + 14.0 + 6.0 + RESULT_LINES + PADDING;
/// Where the last result is cut. The full text is one click away, on the
/// clipboard, so the card only has to be recognisable.
const RESULT_CHARS: usize = 220;

// ── The hold button ───────────────────────────────────────

define_class!(
    // SAFETY: `NSButton` is designed for subclassing, this class adds no ivars
    // and implements no Drop, and both methods are ones AppKit already defines
    // with this signature.
    #[unsafe(super(NSButton))]
    #[thread_kind = MainThreadOnly]
    #[name = "OpenFlowHoldButton"]
    pub struct HoldButton;

    impl HoldButton {
        /// Deliberately does not call super. `NSButton`'s `mouseDown:` runs its
        /// own tracking loop and only returns once the button has been
        /// released, which would collapse the press and the release into one
        /// event and lose the hold.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            if !self.isEnabled() {
                return;
            }
            self.setHighlighted(true);
            self.send(sel!(holdBegan:));
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            self.setHighlighted(false);
            self.send(sel!(holdEnded:));
        }

        /// Focusable, so Space and Return can reach it. A disabled button is
        /// skipped, which is what keeps the focus ring off it while a
        /// transcription is running.
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            self.isEnabled()
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if !is_activation_key(event) {
                // Everything else is somebody else's: Tab out, arrow keys to
                // the sidebar, Cmd-anything to the menu.
                let _: () = unsafe { msg_send![super(self), keyDown: event] };
                return;
            }
            // The auto-repeat of a held key. The press already happened, and
            // delivering it again would start a second capture.
            if event.isARepeat() || !self.isEnabled() {
                return;
            }
            self.setHighlighted(true);
            self.send(sel!(holdBegan:));
        }

        #[unsafe(method(keyUp:))]
        fn key_up(&self, event: &NSEvent) {
            if !is_activation_key(event) {
                let _: () = unsafe { msg_send![super(self), keyUp: event] };
                return;
            }
            self.setHighlighted(false);
            self.send(sel!(holdEnded:));
        }
    }
);

/// Space or Return, the two keys the web screen's `record-button` listens for.
/// Read from the key code rather than the characters so a non-Latin keyboard
/// layout answers the same.
fn is_activation_key(event: &NSEvent) -> bool {
    matches!(event.keyCode(), KEY_SPACE | KEY_RETURN | KEY_ENTER)
}

const KEY_RETURN: u16 = 36;
const KEY_SPACE: u16 = 49;
/// The numeric keypad's Enter, which macOS reports separately.
const KEY_ENTER: u16 = 76;

impl HoldButton {
    /// Send `selector` to whatever this button's target is. The target is
    /// `NSControl`'s ordinary weak property, so the page owns the button and
    /// the button does not own the page.
    fn send(&self, selector: objc2::runtime::Sel) {
        let Some(target) = self.target() else {
            return;
        };
        // SAFETY: both selectors take one `id` and return void, and the only
        // object ever wired up as this button's target implements them.
        let _: () = unsafe { msg_send![&*target, performSelector: selector, withObject: self] };
    }
}

// ── The page ──────────────────────────────────────────────

struct Controls {
    eyebrow: Retained<NSTextField>,
    title: Retained<NSTextField>,
    body: Retained<NSTextField>,
    /// Shown only while recording; see `set_state`.
    meter: Retained<NSLevelIndicator>,
    record: Retained<HoldButton>,
    cancel: Retained<NSButton>,
    hint: Retained<NSTextField>,
    result: Retained<NSButton>,
    result_caption: Retained<NSTextField>,
}

pub struct DictateIvars {
    engine: Arc<Engine>,
    view: Retained<NSView>,
    controls: Controls,
    /// The full text behind the truncated card, so clicking it copies all of
    /// what was said rather than what fits.
    last: RefCell<Option<String>>,
    /// Runs only while recording. Held so it can be invalidated: a repeating
    /// timer retains its target, so one left running would keep this page alive
    /// and keep redrawing a meter nobody is looking at.
    meter_timer: RefCell<Option<Retained<NSTimer>>>,
    /// What the meter is currently showing, which is not what the microphone
    /// last reported: the bar eases downwards. See `audio::meter_decay`.
    meter_shown: Cell<f32>,
}

define_class!(
    // SAFETY: NSObject imposes no subclassing requirements; this class holds
    // only ivars and implements no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OpenFlowDictatePage"]
    #[ivars = DictateIvars]
    pub struct DictatePage;

    impl DictatePage {
        /// One frame of the meter: read the microphone, ease the bar towards it,
        /// draw. Called by `meter_timer` and by nothing else.
        ///
        /// The reading is an atomic load, so the capture is never waiting on
        /// this. What it costs is a redraw of an eight-point bar, thirty times a
        /// second, and only while the button is held.
        #[unsafe(method(tickMeter:))]
        fn tick_meter(&self, _timer: &NSTimer) {
            let ivars = self.ivars();
            let target = meter_fraction(ivars.engine.input_level());
            let shown = meter_decay(ivars.meter_shown.get(), target);
            ivars.meter_shown.set(shown);
            ivars.controls.meter.setDoubleValue(shown as f64);
        }

        /// The same entry point the global shortcut uses, so the button and the
        /// hotkey cannot drift apart: silence gate, live preview and insert
        /// method are all decided downstream of here.
        #[unsafe(method(holdBegan:))]
        fn hold_began(&self, _sender: &AnyObject) {
            self.ivars().engine.hotkey_pressed();
        }

        #[unsafe(method(holdEnded:))]
        fn hold_ended(&self, _sender: &AnyObject) {
            self.ivars().engine.hotkey_released();
        }

        #[unsafe(method(cancelTranscription:))]
        fn cancel_transcription(&self, _sender: &NSControl) {
            let _ = self.ivars().engine.cancel_transcription();
        }

        /// Clipboard only, never a keystroke: the web screen's result button is
        /// `copyToClipboard`, and the user is looking at this window rather
        /// than at the app they would want the text typed into.
        #[unsafe(method(copyLast:))]
        fn copy_last(&self, _sender: &NSControl) {
            let text = self.ivars().last.borrow().clone();
            let Some(text) = text else {
                return;
            };
            let caption = match self.ivars().engine.copy_text(&text) {
                Ok(()) => "Copied to clipboard".to_string(),
                Err(error) => error,
            };
            self.ivars()
                .controls
                .result_caption
                .setStringValue(&NSString::from_str(&caption));
        }
    }
);

impl DictatePage {
    /// Build the page into a view of `size`, the content pane the main window
    /// has to give it.
    pub fn new(
        app: &std::rc::Rc<crate::app::App>,
        mtm: MainThreadMarker,
        size: NSSize,
    ) -> Retained<Self> {
        let engine = Arc::clone(app.engine());
        let (view, controls) = build_content(mtm, size);

        let this = Self::alloc(mtm).set_ivars(DictateIvars {
            engine,
            view,
            controls,
            last: RefCell::new(None),
            meter_timer: RefCell::new(None),
            meter_shown: Cell::new(0.0),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let controls = &this.ivars().controls;
        let target: &AnyObject = this.as_ref();
        // The hold button carries no action of its own: its overrides send
        // `holdBegan:` and `holdEnded:` to whatever the target is.
        unsafe { controls.record.setTarget(Some(target)) };
        crate::ui::wire(&controls.cancel, target, sel!(cancelTranscription:));
        crate::ui::wire(&controls.result, target, sel!(copyLast:));

        this.set_state(RecordingState::Idle);
        this.load();
        this
    }

    /// The view the main window installs in its content pane.
    pub fn view(&self) -> Retained<NSView> {
        self.ivars().view.clone()
    }

    /// Put the keyboard on the record button. Called when the page comes
    /// forward, so Space works without tabbing to it first -- the web screen's
    /// button is focusable for the same reason.
    pub fn focus_record(&self) {
        let ivars = self.ivars();
        if let Some(window) = ivars.view.window() {
            window.makeFirstResponder(Some(&ivars.controls.record));
        }
    }

    /// Re-read what the page shows when it is not being driven by an event: the
    /// bindings, which Settings can change, and the newest transcription.
    pub fn load(&self) {
        let ivars = self.ivars();
        let settings = ivars.engine.settings();
        let record = binding_text(settings, "record");
        let recopy = binding_text(settings, "recopy");
        ivars
            .controls
            .hint
            .setStringValue(&NSString::from_str(&format!(
                "{} works from any app  ·  {} pastes again",
                record, recopy
            )));

        let newest = ivars
            .engine
            .history(1)
            .ok()
            .and_then(|rows| rows.into_iter().next());
        match newest {
            Some(row) => {
                let text = row.formatted_text.unwrap_or(row.raw_text);
                self.set_last(&text, "Your most recent transcription");
            }
            None => {
                *ivars.last.borrow_mut() = None;
                ivars.controls.result.setTitle(&NSString::from_str(
                    "Your first transcription will settle here.",
                ));
                ivars.controls.result.setEnabled(false);
                ivars
                    .controls
                    .result_caption
                    .setStringValue(&NSString::from_str(""));
            }
        }
    }

    /// Show `text` on the result card, with `caption` above it.
    pub fn set_last(&self, text: &str, caption: &str) {
        let ivars = self.ivars();
        *ivars.last.borrow_mut() = Some(text.to_string());
        ivars
            .controls
            .result
            .setTitle(&NSString::from_str(&preview_of(text)));
        ivars.controls.result.setEnabled(true);
        ivars
            .controls
            .result_caption
            .setStringValue(&NSString::from_str(caption));
    }

    /// The three states the web screen draws, with its copy.
    ///
    /// `Formatting` is folded into `Transcribing` for the same reason the pill
    /// folds it: the pipeline never emits it, and inventing a fourth panel here
    /// would be inventing a state the engine does not have.
    pub fn set_state(&self, state: RecordingState) {
        let controls = &self.ivars().controls;
        let (eyebrow, title, body, action, enabled, cancel) = match state {
            RecordingState::Recording => (
                "Listening now",
                "Keep talking\u{2026}",
                "Your audio is captured only while you hold the button.",
                "Release to finish",
                true,
                false,
            ),
            RecordingState::Transcribing | RecordingState::Formatting => (
                "Turning speech into text",
                "One moment\u{2026}",
                "Your provider is transcribing and formatting the result.",
                "Transcribing\u{2026}",
                false,
                true,
            ),
            RecordingState::Idle => (
                "Ready when you are",
                "Hold to speak",
                "Release when you\u{2019}re done. OpenFlow cleans it up and pastes it for you.",
                "Hold to record",
                true,
                false,
            ),
        };
        controls
            .eyebrow
            .setStringValue(&NSString::from_str(eyebrow));
        controls.title.setStringValue(&NSString::from_str(title));
        controls.body.setStringValue(&NSString::from_str(body));
        controls.record.setTitle(&NSString::from_str(action));
        controls.record.setEnabled(enabled);
        controls.cancel.setHidden(!cancel);
        self.set_metering(matches!(state, RecordingState::Recording));
    }

    /// Start or stop the meter with the recording it belongs to.
    ///
    /// Idempotent in both directions: `set_state` is called on every transition
    /// and more than once for some of them, and a second timer for the same bar
    /// would double its refresh rate and leak the first.
    fn set_metering(&self, on: bool) {
        let ivars = self.ivars();
        let mut slot = ivars.meter_timer.borrow_mut();
        if on == slot.is_some() {
            return;
        }
        match slot.take() {
            Some(timer) => {
                // Invalidate, not just drop: the run loop holds the timer, and
                // a repeating timer holds its target, so dropping our handle
                // stops nothing and keeps this page alive.
                timer.invalidate();
                ivars.meter_shown.set(0.0);
                ivars.controls.meter.setDoubleValue(0.0);
                ivars.controls.meter.setHidden(true);
            }
            None => {
                ivars.meter_shown.set(0.0);
                ivars.controls.meter.setDoubleValue(0.0);
                ivars.controls.meter.setHidden(false);
                // SAFETY: the selector is defined on this class above, takes
                // the timer as its only argument, and the timer is invalidated
                // before this page could go away.
                let timer = unsafe {
                    NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                        METER_INTERVAL,
                        self,
                        sel!(tickMeter:),
                        None,
                        true,
                    )
                };
                *slot = Some(timer);
            }
        }
    }
}

/// One line of the result card, cut so the card keeps its shape.
fn preview_of(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let preview: String = flat.chars().take(RESULT_CHARS).collect();
    if flat.chars().count() > RESULT_CHARS {
        format!("{}\u{2026}", preview)
    } else {
        preview
    }
}

/// The binding for `action` as the recorder spells it. Same helper Settings
/// uses, kept separate rather than shared because the two screens are allowed
/// to disagree about what to say when nothing is bound.
fn binding_text(settings: &openflow_core::settings::Settings, action: &str) -> String {
    settings
        .shortcut(action)
        .map(|shortcut| hotkeys::describe(&shortcut))
        .unwrap_or_else(|_| "Not set".to_string())
}

// ── Layout ────────────────────────────────────────────────

/// Everything on this page is centred in its card, so the springs are all
/// four-way: the horizontal margins keep a control centred as the window
/// widens, and the vertical ones share the extra height out rather than letting
/// the block drift to one edge.
const CENTRED: NSAutoresizingMaskOptions = NSAutoresizingMaskOptions(
    NSAutoresizingMaskOptions::ViewMinXMargin.0
        | NSAutoresizingMaskOptions::ViewMaxXMargin.0
        | NSAutoresizingMaskOptions::ViewMinYMargin.0
        | NSAutoresizingMaskOptions::ViewMaxYMargin.0,
);

fn build_content(mtm: MainThreadMarker, size: NSSize) -> (Retained<NSView>, Controls) {
    let view = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), size),
    );
    let inner = size.width - MARGIN * 2.0;

    // ── The result card, along the bottom ──
    let result_card = Card::new(
        mtm,
        NSRect::new(
            NSPoint::new(MARGIN, MARGIN),
            NSSize::new(inner, RESULT_HEIGHT),
        ),
    );
    result_card.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMaxYMargin,
    );
    let result_width = inner - PADDING * 2.0;

    let result_caption = note(
        mtm,
        "",
        NSRect::new(
            NSPoint::new(PADDING, RESULT_HEIGHT - PADDING - 14.0),
            NSSize::new(result_width, 14.0),
        ),
    );
    result_caption.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin,
    );

    // A borderless button so the whole card reads as one clickable surface,
    // which is what the web screen's `last-result` is.
    let result = NSButton::initWithFrame(
        NSButton::alloc(mtm),
        NSRect::new(
            // Directly under the caption, not filling what the card has left.
            // A button centres its title in whatever frame it is given, so a
            // frame the height of the card leaves two lines of transcript
            // floating in the middle of it with air above and below.
            NSPoint::new(PADDING, RESULT_HEIGHT - PADDING - 14.0 - 6.0 - RESULT_LINES),
            NSSize::new(result_width, RESULT_LINES),
        ),
    );
    result.setBordered(false);
    result.setAlignment(NSTextAlignment::Left);
    result.setFont(Some(&NSFont::systemFontOfSize(NSFont::systemFontSize())));
    result.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin,
    );
    // The cell wraps rather than truncating to one line: the preview is two
    // lines of the transcription and the point is to recognise it.
    if let Some(cell) = result.cell() {
        cell.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByWordWrapping);
        cell.setWraps(true);
    }

    result_card.addSubview(&result);
    result_card.addSubview(&result_caption);

    // ── The recorder card, filling what is left ──
    let recorder_bottom = MARGIN + RESULT_HEIGHT + GAP;
    let recorder_height = (size.height - MARGIN - recorder_bottom).max(0.0);
    let recorder = Card::new(
        mtm,
        NSRect::new(
            NSPoint::new(MARGIN, recorder_bottom),
            NSSize::new(inner, recorder_height),
        ),
    );
    recorder.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );

    // Laid out downwards, centred both ways. Starting at the top of the card
    // instead would drop all the slack in a tall window below the shortcut
    // hint, which is most of the card.
    let centre = |width: f64| (inner - width) / 2.0;
    let mut y = ((recorder_height + BLOCK_HEIGHT) / 2.0).min(recorder_height - PADDING);

    let glyph = NSImageView::initWithFrame(
        NSImageView::alloc(mtm),
        NSRect::new(
            NSPoint::new(centre(56.0), y - 56.0),
            NSSize::new(56.0, 56.0),
        ),
    );
    if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str("mic.fill"),
        Some(&NSString::from_str("Microphone")),
    ) {
        // `NSFontWeight` is a bare `CGFloat`; 0.0 is the regular weight.
        let config = NSImageSymbolConfiguration::configurationWithPointSize_weight(44.0, 0.0);
        glyph.setImage(Some(&image));
        glyph.setSymbolConfiguration(Some(&config));
        glyph.setContentTintColor(Some(&NSColor::controlAccentColor()));
    }
    glyph.setAutoresizingMask(CENTRED);
    y -= 56.0 + 18.0;

    let eyebrow = note(
        mtm,
        "",
        NSRect::new(
            NSPoint::new(PADDING, y - 14.0),
            NSSize::new(inner - PADDING * 2.0, 14.0),
        ),
    );
    eyebrow.setAlignment(NSTextAlignment::Center);
    eyebrow.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable
            | NSAutoresizingMaskOptions::ViewMinYMargin
            | NSAutoresizingMaskOptions::ViewMaxYMargin,
    );
    y -= 14.0 + 6.0;

    let title = NSTextField::labelWithString(&NSString::from_str(""), mtm);
    title.setFrame(NSRect::new(
        NSPoint::new(PADDING, y - 30.0),
        NSSize::new(inner - PADDING * 2.0, 30.0),
    ));
    title.setAlignment(NSTextAlignment::Center);
    // 0.3 is `NSFontWeightSemibold`, which is the weight the web screen's h1
    // resolves to.
    title.setFont(Some(&NSFont::systemFontOfSize_weight(24.0, 0.3)));
    title.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable
            | NSAutoresizingMaskOptions::ViewMinYMargin
            | NSAutoresizingMaskOptions::ViewMaxYMargin,
    );
    y -= 30.0 + 8.0;

    // The body is the longest string on the page and the one most likely to
    // need two lines at a narrow width, so it wraps rather than truncating.
    let body_width = (inner - PADDING * 2.0 - 60.0).max(160.0);
    let body = note(
        mtm,
        "",
        NSRect::new(
            NSPoint::new(centre(body_width), y - 34.0),
            NSSize::new(body_width, 34.0),
        ),
    );
    body.setAlignment(NSTextAlignment::Center);
    body.setFont(Some(&NSFont::systemFontOfSize(
        NSFont::smallSystemFontSize(),
    )));
    allow_wrapping(&body, body_width);
    body.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable
            | NSAutoresizingMaskOptions::ViewMinYMargin
            | NSAutoresizingMaskOptions::ViewMaxYMargin,
    );
    y -= 34.0 + 20.0;

    let record = HoldButton::new(
        mtm,
        NSRect::new(
            NSPoint::new(centre(220.0), y - 40.0),
            NSSize::new(220.0, 40.0),
        ),
    );
    record.setBezelStyle(NSBezelStyle::Push);
    record.setControlSize(objc2_app_kit::NSControlSize::Large);
    // The accent fill is what makes this read as the primary action, which is
    // what the web screen's `record-button` is. A plain push button of this
    // size reads as a placeholder next to nothing else on the card.
    record.setBezelColor(Some(&NSColor::controlAccentColor()));
    record.setFont(Some(&NSFont::systemFontOfSize_weight(15.0, 0.3)));
    record.setAutoresizingMask(CENTRED);
    y -= 40.0 + 10.0;

    // Directly under the button that starts it, the width of that button. A
    // level indicator rather than a view drawing its own bar: this crate lays
    // out stock controls, and the capacity style already draws the rounded
    // capsule and clips the fill to it.
    let meter = NSLevelIndicator::initWithFrame(
        NSLevelIndicator::alloc(mtm),
        NSRect::new(
            NSPoint::new(centre(METER_WIDTH), y - METER_HEIGHT),
            NSSize::new(METER_WIDTH, METER_HEIGHT),
        ),
    );
    meter.setLevelIndicatorStyle(NSLevelIndicatorStyle::ContinuousCapacity);
    meter.setMinValue(0.0);
    meter.setMaxValue(1.0);
    meter.setDoubleValue(0.0);
    // A capacity indicator is editable by default -- clicking one sets it --
    // and this one is a readout, not an input.
    meter.setEditable(false);
    meter.setFillColor(Some(&NSColor::controlAccentColor()));
    // Hidden until there is something to show. It still occupies its row in
    // `BLOCK_HEIGHT`, the way the cancel button does, so the block does not
    // jump when recording starts.
    meter.setHidden(true);
    meter.setAutoresizingMask(CENTRED);
    y -= METER_HEIGHT + 12.0;

    let cancel = crate::ui::button(
        mtm,
        NSRect::new(
            NSPoint::new(centre(170.0), y - 24.0),
            NSSize::new(170.0, 24.0),
        ),
        "Cancel transcription",
        0,
    );
    cancel.setHidden(true);
    cancel.setAutoresizingMask(CENTRED);
    y -= 24.0 + 14.0;

    let hint = note(
        mtm,
        "",
        NSRect::new(
            NSPoint::new(PADDING, y - 14.0),
            NSSize::new(inner - PADDING * 2.0, 14.0),
        ),
    );
    hint.setAlignment(NSTextAlignment::Center);
    hint.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable
            | NSAutoresizingMaskOptions::ViewMinYMargin
            | NSAutoresizingMaskOptions::ViewMaxYMargin,
    );

    recorder.addSubview(&glyph);
    recorder.addSubview(&eyebrow);
    recorder.addSubview(&title);
    recorder.addSubview(&body);
    recorder.addSubview(&record);
    recorder.addSubview(&meter);
    recorder.addSubview(&cancel);
    recorder.addSubview(&hint);

    view.addSubview(&recorder);
    view.addSubview(&result_card);

    (
        view,
        Controls {
            eyebrow,
            title,
            body,
            meter,
            record,
            cancel,
            hint,
            result,
            result_caption,
        },
    )
}

impl HoldButton {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm);
        // SAFETY: `initWithFrame:` is `NSView`'s designated initialiser, which
        // `NSButton` inherits.
        unsafe { msg_send![this, initWithFrame: frame] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The card shows one line, so newlines and runs of spaces collapse.
    #[test]
    fn the_result_preview_is_one_flat_line() {
        assert_eq!(preview_of("hello  there\nfriend"), "hello there friend");
        assert_eq!(preview_of(""), "");
    }

    /// A long transcription is cut, and the cut is marked.
    #[test]
    fn a_long_result_is_marked_where_it_was_cut() {
        let long = "a".repeat(RESULT_CHARS + 5);
        let preview = preview_of(&long);
        assert_eq!(preview.chars().count(), RESULT_CHARS + 1);
        assert!(preview.ends_with('\u{2026}'));
    }

    /// The centring constant has to be the sum of the steps `build_content`
    /// walks down, or the block sits off-centre by whatever the two disagree
    /// by. Listed here in the same order the layout uses them, as literals: a
    /// row written as the constant it came from would agree with itself.
    #[test]
    fn the_block_height_is_the_sum_of_the_rows() {
        let rows = [
            56.0, 18.0, // glyph, gap
            14.0, 6.0, // eyebrow, gap
            30.0, 8.0, // title, gap
            34.0, 20.0, // body, gap
            40.0, 10.0, // record button, gap
            8.0, 12.0, // level meter, gap
            24.0, 14.0, // cancel button, gap
            14.0, // hint
        ];
        assert_eq!(rows.iter().sum::<f64>(), BLOCK_HEIGHT);
    }

    /// Exactly at the limit nothing is cut, so no ellipsis is added.
    #[test]
    fn a_result_at_the_limit_is_not_marked() {
        let exact = "b".repeat(RESULT_CHARS);
        assert_eq!(preview_of(&exact), exact);
    }
}
