//! A transcript as timestamped paragraphs of clickable words, shared by the editor and the
//! library: the word being spoken is highlighted, and clicking a word or a timestamp seeks.

use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, div, img,
    prelude::FluentBuilder as _, px,
};

use crate::transcript::{Piece, Transcript, timestamp};
use crate::ui::Theme;

/// The paragraph holding piece `current`, given `Transcript::paragraph_pieces`.
pub fn paragraph_of(paragraphs: &[Vec<usize>], current: Option<usize>) -> Option<usize> {
    let current = current?;
    paragraphs.iter().position(|p| {
        p.first().is_some_and(|f| *f <= current) && p.last().is_some_and(|l| *l >= current)
    })
}

/// `paragraphs` comes from `transcript.paragraph_pieces()`; `current` is the piece being
/// spoken. `is_cut` dims words an edit removes. `seek` runs on the view when a word or
/// timestamp is clicked.
#[allow(clippy::too_many_arguments)]
pub fn paragraphs<V: 'static>(
    transcript: &Transcript,
    paragraphs: &[Vec<usize>],
    current: Option<usize>,
    is_cut: impl Fn(&Piece) -> bool,
    scroll: &ScrollHandle,
    theme: Theme,
    cx: &mut Context<V>,
    seek: fn(&mut V, f64, &mut Context<V>),
) -> AnyElement {
    div()
        .id("transcript")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .track_scroll(scroll)
        .px(px(14.))
        .pb(px(14.))
        .flex()
        .flex_col()
        .gap(px(12.))
        .children(paragraphs.iter().enumerate().map(|(p, indices)| {
            let start = transcript.pieces[indices[0]].start;
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .id(SharedString::from(format!("paragraph-{p}")))
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text_muted)
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme.accent))
                        .on_click(cx.listener(move |this, _, _, cx| seek(this, start, cx)))
                        .child(timestamp(start)),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .text_size(px(13.))
                        .line_height(px(20.))
                        .children(indices.iter().map(|&i| {
                            let piece = &transcript.pieces[i];
                            let at = piece.start;
                            div()
                                .id(SharedString::from(format!("word-{i}")))
                                .px(px(2.))
                                .rounded(px(4.))
                                .cursor_pointer()
                                .when(current == Some(i), |d| d.bg(theme.accent.opacity(0.28)))
                                .when(current != Some(i), |d| {
                                    d.hover(|s| s.bg(theme.control_hover))
                                })
                                .when(is_cut(piece), |d| {
                                    d.text_color(theme.text_muted.opacity(0.6))
                                })
                                .on_click(cx.listener(move |this, _, _, cx| seek(this, at, cx)))
                                .child(piece.text.clone())
                        })),
                )
        }))
        .into_any_element()
}

/// An empty state: the dithered angel, faint, above a short line of text.
pub fn dithered_note(text: impl Into<SharedString>, theme: Theme) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(12.))
        .px(px(20.))
        .child(img("images/icon.png").size(px(64.)).opacity(0.55))
        .child(
            div()
                .max_w(px(240.))
                .text_size(px(12.5))
                .text_color(theme.text_muted)
                .text_center()
                .child(text.into()),
        )
        .into_any_element()
}
