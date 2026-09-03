//! Stock AppKit controls, laid out by hand.
//!
//! No auto layout anywhere in this crate: frames are shorter to read than
//! constraints and there is nothing to solve at run time. Windows that are a
//! two-column form are a fixed size and set their frames outright; the main
//! window's pages are handed the size of the pane they were given and spring
//! off it with autoresizing masks. `NSSplitViewController` does use auto layout
//! to place its own two panes, but that stops at the pane -- no view here mixes
//! the two models.

pub mod card;
pub mod dictate;
pub mod history;
pub mod main_window;
pub mod onboarding;
pub mod plugins;
pub mod recorder;
pub mod settings;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAnimationContext, NSApplication, NSBezelStyle, NSButton, NSComboBox, NSControl, NSFont,
    NSPopUpButton, NSScrollView, NSSecureTextField, NSSwitch, NSTextAlignment, NSTextField,
    NSTextView, NSView, NSWindow, NSWindowCollectionBehavior,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// Height of one form row.
pub const ROW: f64 = 24.0;
/// Vertical gap between rows.
pub const GAP: f64 = 10.0;
/// Width of the label column.
pub const LABEL_WIDTH: f64 = 132.0;
/// Where the control column starts.
pub const CONTROL_X: f64 = LABEL_WIDTH + 10.0;

/// Bring `window` forward from a menu bar click, the way the Tauri host does.
///
/// The Tauri equivalent is `WebviewWindow::show()` followed by `set_focus()`,
/// and `set_focus` on macOS ends in `activateIgnoringOtherApps:YES` plus
/// `makeKeyAndOrderFront:`. The first Milestone A build called only
/// `makeKeyAndOrderFront:` and the cooperative `NSApplication::activate()`, and
/// for an `LSUIElement` app invoked from a status item that is not enough: the
/// app is not the active app, so the cooperative activate is declined, and the
/// window opens unseen. Three calls fix it, and each is doing separate work:
///
/// - `MoveToActiveSpace` so the window follows the user to whichever Space is
///   in front. Without it, a window ordered front while another Space is active
///   opens on the Space it was last on, which looks exactly like nothing
///   happening.
/// - `FullScreenAuxiliary` so it can also open over a full-screen app. That is
///   the reachable case here rather than a wrong one: the status item is
///   clickable while another app is full screen, so the window the click asks
///   for has to be able to join that Space. Without it macOS either swaps
///   Spaces under the user or leaves the window behind.
/// - `orderFrontRegardless` for the ordering an inactive app is otherwise
///   refused, and `activateIgnoringOtherApps:` for the activation. Deprecated
///   since macOS 14 in favour of the cooperative `activate`, still functional,
///   and still what a menu bar app needs, which is why it goes through
///   `msg_send!` rather than the deprecated binding.
pub fn present_window(window: &NSWindow, name: &str) {
    crate::trace!("show window={}", name);
    // The Dock icon first, then the window. An accessory app cannot take focus
    // the way a regular one can, so activating before the switch leaves the
    // window on screen but behind whatever the user was already in.
    crate::app::refresh_dock_presence(true);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::MoveToActiveSpace
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    window.makeKeyAndOrderFront(None);
    window.orderFrontRegardless();
    if let Some(mtm) = MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: a no-argument-beyond-BOOL void method on NSApplication, sent
        // on the main thread.
        let _: () = unsafe { msg_send![&*app, activateIgnoringOtherApps: true] };
    }
}

/// Order a window out and give the Dock icon back if it was the last one.
///
/// Windows here are hidden rather than closed, so this is what closing means.
/// It is the counterpart to [`present_window`], and both sides have to run
/// through the pair, or the app keeps a Dock icon for a window nobody can see.
pub fn dismiss_window(window: &NSWindow, name: &str) {
    crate::trace!("hide window={}", name);
    window.orderOut(None);
    crate::app::refresh_dock_presence(false);
}

/// Run `body` inside an animation group of `duration` seconds. Unlike
/// `setFrame:display:animate:` this does not block the main thread, which
/// matters because the hotkey path runs on it.
pub fn animate(duration: f64, body: impl Fn()) {
    let block = block2::RcBlock::new(move |context: core::ptr::NonNull<NSAnimationContext>| {
        // SAFETY: AppKit hands us a live context for the duration of the block.
        unsafe { context.as_ref() }.setDuration(duration);
        body();
    });
    NSAnimationContext::runAnimationGroup(&block);
}

/// A top-down cursor over a form's content view.
pub struct Form {
    pub view: Retained<NSView>,
    y: f64,
    width: f64,
}

impl Form {
    pub fn new(mtm: MainThreadMarker, width: f64, height: f64) -> Self {
        let view = {
            NSView::initWithFrame(
                NSView::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, height)),
            )
        };
        Self {
            view,
            y: height - 18.0,
            width,
        }
    }

    /// A labelled row: the label frame and the control frame beside it.
    pub fn row(&mut self, height: f64) -> (NSRect, NSRect) {
        self.y -= height;
        let label = NSRect::new(
            NSPoint::new(0.0, self.y + (height - 17.0) / 2.0),
            NSSize::new(LABEL_WIDTH, 17.0),
        );
        let control = NSRect::new(
            NSPoint::new(CONTROL_X, self.y),
            NSSize::new(self.width - CONTROL_X, height),
        );
        self.y -= GAP;
        (label, control)
    }

    /// A row with no label column, spanning the whole width.
    pub fn full(&mut self, height: f64) -> NSRect {
        self.y -= height;
        let frame = NSRect::new(NSPoint::new(0.0, self.y), NSSize::new(self.width, height));
        self.y -= GAP;
        frame
    }

    /// A control column row with nothing in the label column, for a button or a
    /// status line that belongs to the row above it.
    pub fn control_only(&mut self, height: f64) -> NSRect {
        self.y -= height;
        let frame = NSRect::new(
            NSPoint::new(CONTROL_X, self.y),
            NSSize::new(self.width - CONTROL_X, height),
        );
        self.y -= GAP;
        frame
    }

    /// A wrapped hint under the row above it, as tall as its text needs.
    ///
    /// The fixed-height variant truncated: these sentences are longer than the
    /// control column is wide, and a label that cannot wrap simply loses its
    /// tail -- off the right of a window that, until now, could not even be
    /// widened to read it.
    pub fn note_row(&mut self, mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
        let width = self.width - CONTROL_X;
        let field = note(
            mtm,
            text,
            NSRect::new(NSPoint::new(CONTROL_X, 0.0), NSSize::new(width, 14.0)),
        );
        wrap(&field, width);
        let height = field.frame().size.height;
        self.y -= height;
        field.setFrameOrigin(NSPoint::new(CONTROL_X, self.y));
        self.y -= GAP;
        self.add(&field);
        field
    }

    pub fn add(&self, view: &NSView) {
        self.view.addSubview(view);
    }
}

pub fn label(mtm: MainThreadMarker, text: &str, frame: NSRect) -> Retained<NSTextField> {
    let field = { NSTextField::labelWithString(&NSString::from_str(text), mtm) };
    {
        field.setFrame(frame);
        field.setAlignment(NSTextAlignment::Right);
        field.setFont(Some(&NSFont::systemFontOfSize(
            NSFont::smallSystemFontSize(),
        )));
    }
    field
}

/// A muted line of explanatory text under a control.
/// Let a label wrap inside `width`, and grow it to whatever height that needs.
///
/// `labelWithString:` gives a single line that truncates. Wrapping has to be
/// asked for on the cell, and the frame resized afterwards, or the extra lines
/// are laid out underneath the visible one and clipped.
pub fn wrap(field: &NSTextField, width: f64) {
    allow_wrapping(field, width);
    let fitted = field.sizeThatFits(NSSize::new(width, f64::MAX));
    let origin = field.frame().origin;
    field.setFrame(NSRect::new(
        origin,
        NSSize::new(width, fitted.height.ceil()),
    ));
}

/// Wrapping without the resize, for a label whose text arrives later and whose
/// frame was reserved for it. Growing that one to fit an empty string would
/// leave nothing to write into.
pub fn allow_wrapping(field: &NSTextField, width: f64) {
    field.setUsesSingleLineMode(false);
    field.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByWordWrapping);
    field.setPreferredMaxLayoutWidth(width);
}

pub fn note(mtm: MainThreadMarker, text: &str, frame: NSRect) -> Retained<NSTextField> {
    let field = { NSTextField::labelWithString(&NSString::from_str(text), mtm) };
    {
        field.setFrame(frame);
        field.setFont(Some(&NSFont::systemFontOfSize(10.0)));
        field.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
    }
    field
}

pub fn text_field(mtm: MainThreadMarker, frame: NSRect, tag: isize) -> Retained<NSTextField> {
    let field = { NSTextField::initWithFrame(NSTextField::alloc(mtm), frame) };
    {
        field.setTag(tag);
        field.setFont(Some(&NSFont::systemFontOfSize(
            NSFont::smallSystemFontSize(),
        )));
    }
    field
}

pub fn secure_field(
    mtm: MainThreadMarker,
    frame: NSRect,
    tag: isize,
) -> Retained<NSSecureTextField> {
    let field = { NSSecureTextField::initWithFrame(NSSecureTextField::alloc(mtm), frame) };
    {
        field.setTag(tag);
        field.setFont(Some(&NSFont::systemFontOfSize(
            NSFont::smallSystemFontSize(),
        )));
    }
    field
}

pub fn popup(
    mtm: MainThreadMarker,
    frame: NSRect,
    tag: isize,
    titles: &[&str],
) -> Retained<NSPopUpButton> {
    let button =
        { NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), frame, false) };
    {
        button.setTag(tag);
        button.setFont(Some(&NSFont::systemFontOfSize(
            NSFont::smallSystemFontSize(),
        )));
        for title in titles {
            button.addItemWithTitle(&NSString::from_str(title));
        }
    }
    button
}

/// An editable popup: the fetched models are offered, but any model id can be
/// typed, which is what the web settings screen allows.
pub fn combo(mtm: MainThreadMarker, frame: NSRect, tag: isize) -> Retained<NSComboBox> {
    let box_ = { NSComboBox::initWithFrame(NSComboBox::alloc(mtm), frame) };
    {
        box_.setTag(tag);
        box_.setCompletes(true);
        box_.setFont(Some(&NSFont::systemFontOfSize(
            NSFont::smallSystemFontSize(),
        )));
    }
    box_
}

pub fn switch_control(mtm: MainThreadMarker, frame: NSRect, tag: isize) -> Retained<NSSwitch> {
    let switch = { NSSwitch::initWithFrame(NSSwitch::alloc(mtm), frame) };
    switch.setTag(tag);
    switch
}

pub fn button(mtm: MainThreadMarker, frame: NSRect, title: &str, tag: isize) -> Retained<NSButton> {
    let button = { NSButton::initWithFrame(NSButton::alloc(mtm), frame) };
    {
        button.setTitle(&NSString::from_str(title));
        button.setBezelStyle(NSBezelStyle::Push);
        button.setTag(tag);
        button.setFont(Some(&NSFont::systemFontOfSize(
            NSFont::smallSystemFontSize(),
        )));
    }
    button
}

/// One option of a radio group. AppKit groups radio buttons by superview and
/// action, so every option in a group has to be added to the same view and
/// wired to the same selector; the tag says which one was picked.
pub fn radio(mtm: MainThreadMarker, frame: NSRect, title: &str, tag: isize) -> Retained<NSButton> {
    let button = NSButton::initWithFrame(NSButton::alloc(mtm), frame);
    button.setButtonType(objc2_app_kit::NSButtonType::Radio);
    button.setTitle(&NSString::from_str(title));
    button.setTag(tag);
    button.setFont(Some(&NSFont::systemFontOfSize(
        NSFont::smallSystemFontSize(),
    )));
    button
}

/// A scrollable text view, for the dictionary.
pub fn text_view(
    mtm: MainThreadMarker,
    frame: NSRect,
) -> (Retained<NSScrollView>, Retained<NSTextView>) {
    let scroll = { NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame) };
    let content = NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(frame.size.width, frame.size.height),
    );
    let view = { NSTextView::initWithFrame(NSTextView::alloc(mtm), content) };
    {
        scroll.setHasVerticalScroller(true);
        scroll.setBorderType(objc2_app_kit::NSBorderType::BezelBorder);
        view.setFont(Some(&NSFont::systemFontOfSize(
            NSFont::smallSystemFontSize(),
        )));
        view.setRichText(false);
        view.setAutomaticQuoteSubstitutionEnabled(false);
        view.setAutomaticSpellingCorrectionEnabled(false);
        scroll.setDocumentView(Some(&view));
    }
    (scroll, view)
}

/// Wire a control to `action` on `target`.
pub fn wire(control: &NSControl, target: &AnyObject, action: objc2::runtime::Sel) {
    unsafe {
        control.setTarget(Some(target));
        control.setAction(Some(action));
    }
}
