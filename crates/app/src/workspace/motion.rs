//! Quiet, explicit motion on top of GPUI's animation wrapper.
//!
//! GPUI has no CSS transitions. A property moves only when the element is wrapped in
//! [`AnimationExt::with_animation`]: the animator receives a 0..=1 delta each frame and returns a
//! restyled element. Entering a surface is a oneshot fade or slide; live progress is a repeating
//! pulse or a measured bar. Unmounting skips an exit animation — keeping a closed panel mounted
//! just to reverse the delta is how this would grow a second lifecycle, and the quiet page does
//! not need it.
//!
//! The wrappers below are the only animation entry points the shell should grow. A new motion
//! belongs here, named for the feeling, not for the property it tweaks.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, Div, ElementId, IntoElement, ParentElement as _,
    Styled as _, div, ease_in_out, ease_out_quint, pulsating_between, px, relative,
};

use super::tokens::WorkspaceTokens;

const ENTER: Duration = Duration::from_millis(240);
const PULSE: Duration = Duration::from_millis(1600);
const SWEEP: Duration = Duration::from_millis(1400);

/// Fade a newly mounted surface in. The id must change when the *occasion* changes — a second
/// recording reuses "capture-bar" as a selector, so the animation id carries the session.
///
/// The wrapper is `flex_none` so a capture bar or Ask column keeps its caller's size. Do not use
/// this around a block that still needs to *learn* its width from a parent; [`fade_in_fill`] is
/// that path.
pub(crate) fn fade_in(id: impl Into<ElementId>, child: impl IntoElement) -> AnyElement {
    div()
        .flex_none()
        .child(child)
        .with_animation(
            id,
            Animation::new(ENTER).with_easing(ease_out_quint()),
            |this, delta| this.opacity(delta),
        )
        .into_any_element()
}

/// Fade in a block that already carries its own width (`w_full`, a cap, or both).
///
/// Animating the block itself avoids a `flex_none` wrapper, which would shrink-wrap Home to its
/// labels and leave Record a call the width of the words.
pub(crate) fn fade_in_fill(id: impl Into<ElementId>, child: Div) -> impl IntoElement {
    child.with_animation(
        id,
        Animation::new(ENTER).with_easing(ease_out_quint()),
        |this, delta| this.opacity(delta),
    )
}

/// Fade plus a short rise, for sheets that appear over a scrim.
pub(crate) fn rise_in(id: impl Into<ElementId>, child: impl IntoElement) -> AnyElement {
    div()
        .child(child)
        .with_animation(
            id,
            Animation::new(ENTER).with_easing(ease_out_quint()),
            |this, delta| this.opacity(delta).mt(px(18.0 * (1.0 - delta))),
        )
        .into_any_element()
}

/// Opacity on the recording dot so it breathes without a second colour.
pub(crate) fn live_pulse(dot: Div) -> impl IntoElement {
    dot.with_animation(
        "live-pulse",
        Animation::new(PULSE)
            .repeat()
            .with_easing(pulsating_between(0.38, 1.0)),
        |this, delta| this.opacity(delta),
    )
}

/// Determinate bar for a known fraction, 0..=1. Width follows the measurement; no looping.
pub(crate) fn measured_progress(fraction: f32, tokens: WorkspaceTokens) -> AnyElement {
    let fraction = fraction.clamp(0.0, 1.0);
    track(tokens)
        .child(
            div()
                .h_full()
                .rounded_full()
                .bg(tokens.accent)
                .w(relative(fraction)),
        )
        .into_any_element()
}

/// Indeterminate bar: a segment sweeps while work is happening and we cannot say how much is left.
pub(crate) fn writing_progress(id: impl Into<ElementId>, tokens: WorkspaceTokens) -> AnyElement {
    track(tokens)
        .child(
            div()
                .h_full()
                .rounded_full()
                .bg(tokens.accent)
                .with_animation(
                    id,
                    Animation::new(SWEEP).repeat().with_easing(ease_in_out),
                    |this, delta| this.w(relative(0.36)).ml(relative(delta * 0.64)),
                ),
        )
        .into_any_element()
}

fn track(tokens: WorkspaceTokens) -> Div {
    div()
        .h(px(3.0))
        .w_full()
        .rounded_full()
        .overflow_hidden()
        .bg(tokens.accent_wash)
}

#[cfg(test)]
mod tests {
    use super::{ENTER, PULSE, SWEEP};

    #[test]
    fn motion_stays_quiet() {
        assert!(ENTER.as_millis() < 400, "enter must not linger");
        assert!(PULSE.as_millis() > ENTER.as_millis());
        assert!(SWEEP.as_millis() < 2_000);
    }
}
