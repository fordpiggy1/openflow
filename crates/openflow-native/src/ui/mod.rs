//! Shared AppKit helpers.

/// Run `body` inside an animation group of `duration` seconds. Unlike
/// `setFrame:display:animate:` this does not block the main thread, which
/// matters because the hotkey path runs on it.
pub fn animate(duration: f64, body: impl Fn()) {
    let block = block2::RcBlock::new(
        move |context: core::ptr::NonNull<objc2_app_kit::NSAnimationContext>| {
            // SAFETY: AppKit hands us a live context for the duration of the block.
            unsafe { context.as_ref() }.setDuration(duration);
            body();
        },
    );
    objc2_app_kit::NSAnimationContext::runAnimationGroup(&block);
}
