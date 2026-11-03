pub mod bubble;
pub mod editor;
pub mod hud;
pub mod icons;
pub mod launcher;
pub mod library;
pub mod settings_view;
pub mod transcript_view;

use gpui::{
    App, ClickEvent, Div, ElementId, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    hsla, prelude::FluentBuilder as _, px, rgb,
};

/// The accent: electric blue, used only for what's on or selected.
pub const ACCENT: u32 = 0x3d3dff;

/// Black glass: every surface takes the notch's black so the HUD, panel, editor and Settings
/// read as one object. Annuncia is dark everywhere, whatever the system appearance.
#[derive(Clone, Copy)]
pub struct Theme {
    /// Tint laid over the panel's glass, dark enough that content reads over anything.
    pub panel: Hsla,
    /// Background of regular windows (Settings, editor).
    pub window: Hsla,
    /// Cards and grouped rows on the window background.
    pub surface: Hsla,
    /// Borders and the rules between rows.
    pub hairline: Hsla,
    /// Control Center-style module tiles inside the panel.
    pub tile: Hsla,
    pub tile_hover: Hsla,
    /// Icon circle of a module that is switched off.
    pub icon_off: Hsla,
    pub text: Hsla,
    pub text_muted: Hsla,
    pub control: Hsla,
    pub control_hover: Hsla,
    pub control_active: Hsla,
    pub accent: Hsla,
    pub accent_text: Hsla,
    pub record: Hsla,
}

impl Theme {
    /// Kept per window so a future light theme has one place to branch.
    pub fn for_window(_window: &Window) -> Self {
        Self::black()
    }

    pub fn black() -> Self {
        Self {
            panel: hsla(0., 0., 0., 0.84),
            window: hsla(0., 0., 0., 1.),
            surface: hsla(0., 0., 0.055, 1.),
            hairline: hsla(0., 0., 1., 0.08),
            tile: hsla(0., 0., 1., 0.075),
            tile_hover: hsla(0., 0., 1., 0.12),
            icon_off: hsla(0., 0., 1., 0.14),
            text: hsla(0., 0., 0.95, 1.),
            text_muted: hsla(0., 0., 1., 0.48),
            control: hsla(0., 0., 1., 0.07),
            control_hover: hsla(0., 0., 1., 0.11),
            control_active: hsla(0., 0., 1., 0.17),
            accent: rgb(ACCENT).into(),
            accent_text: hsla(0., 0., 1., 1.),
            record: rgb(0xff453a).into(),
        }
    }
}

pub type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// macOS-style switch.
pub fn toggle(
    id: impl Into<ElementId>,
    on: bool,
    theme: Theme,
    on_click: OnClick,
) -> impl IntoElement {
    div()
        .id(id)
        .w(px(32.))
        .h(px(19.))
        .rounded(px(10.))
        .p(px(2.))
        .flex()
        .when(on, |d| d.justify_end().bg(theme.accent))
        .when(!on, |d| d.justify_start().bg(theme.control_active))
        .cursor_pointer()
        .on_click(move |e, w, cx| on_click(e, w, cx))
        .child(
            div()
                .size(px(15.))
                .rounded_full()
                .bg(gpui::white())
                .shadow_sm(),
        )
}

/// Segmented control; `selected` is the index of the active segment.
pub fn segmented(
    id: &str,
    labels: &[&'static str],
    selected: usize,
    theme: Theme,
    on_select: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_select = std::rc::Rc::new(on_select);
    div()
        .flex()
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(8.))
        .bg(theme.control)
        .children(labels.iter().enumerate().map(|(i, label)| {
            let on_select = on_select.clone();
            // Segments size to their labels (and share any extra width), so long ones never spill.
            div()
                .id(SharedString::from(format!("{id}-{i}")))
                .flex_auto()
                .px(px(10.))
                .whitespace_nowrap()
                .flex()
                .justify_center()
                .py(px(5.))
                .rounded(px(6.))
                .text_size(px(12.5))
                .font_weight(gpui::FontWeight::MEDIUM)
                .cursor_pointer()
                .when(i == selected, |d| {
                    d.bg(theme.control_active).text_color(theme.text)
                })
                .when(i != selected, |d| {
                    d.text_color(theme.text_muted)
                        .hover(|s| s.bg(theme.control))
                })
                .on_click(move |_, w, cx| on_select(i, w, cx))
                .child(*label)
        }))
}

/// Rounded-rect button (not a pill), filled with the accent when `primary`.
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    primary: bool,
    theme: Theme,
) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .flex_none()
        .whitespace_nowrap()
        .px(px(12.))
        .py(px(6.))
        .rounded(px(7.))
        .text_size(px(13.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .cursor_pointer()
        .when(primary, |d| {
            d.bg(theme.accent)
                .text_color(theme.accent_text)
                .hover(|s| s.opacity(0.9))
        })
        .when(!primary, |d| {
            d.bg(theme.control)
                .text_color(theme.text)
                .hover(|s| s.bg(theme.control_hover))
        })
        .child(label.into())
}

pub fn keycap(label: impl Into<SharedString>, color: Hsla) -> impl IntoElement {
    div()
        .px(px(5.))
        .py(px(1.))
        .rounded(px(4.))
        .border_1()
        .border_color(color.opacity(0.4))
        .text_size(px(11.))
        .text_color(color)
        .child(label.into())
}
