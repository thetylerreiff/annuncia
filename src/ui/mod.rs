pub mod bubble;
pub mod editor;
pub mod hud;
pub mod icons;
pub mod launcher;
pub mod library;
pub mod onboarding;
pub mod settings_view;
pub mod transcript_view;

use gpui::{
    AnyView, App, AppContext as _, Bounds, ClickEvent, Context, Div, ElementId, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, canvas, div, fill, hsla, point,
    prelude::FluentBuilder as _, px, rgb, size,
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
    /// A lighter tint of the accent for text and glyphs on black, where the accent itself is
    /// only about 3.3:1.
    pub accent_soft: Hsla,
    pub accent_text: Hsla,
    /// Healthy / granted.
    pub ok: Hsla,
    /// Needs attention, but nothing is broken.
    pub warn: Hsla,
    pub record: Hsla,
    /// A starfield behind Settings, the library and the panel, which marks a nightly build.
    pub stars: bool,
}

impl Theme {
    /// Kept per window so a future light theme has one place to branch.
    pub fn for_window(_window: &Window) -> Self {
        static NIGHTLY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let stars = *NIGHTLY.get_or_init(|| {
            crate::updater::built_channel() == crate::settings::UpdateChannel::Nightly
        });
        Self {
            stars,
            ..Self::black()
        }
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
            accent_soft: rgb(0x8c8cff).into(),
            accent_text: hsla(0., 0., 1., 1.),
            ok: rgb(0x30d158).into(),
            warn: rgb(0xffd60a).into(),
            record: rgb(0xff453a).into(),
            stars: false,
        }
    }
}

/// A faint, still starfield filling its parent, for behind a window's content. Each cell of a
/// fixed grid may hold one star at a hashed spot, so the stars stay put as the window resizes.
pub fn starfield() -> impl IntoElement {
    const CELL: f32 = 56.;
    canvas(
        |_, _, _| {},
        |bounds, (), window, _| {
            let columns = (f32::from(bounds.size.width) / CELL).ceil() as u64;
            let rows = (f32::from(bounds.size.height) / CELL).ceil() as u64;
            for row in 0..rows {
                for column in 0..columns {
                    let n = mix(column << 32 | row);
                    // Leave most cells empty, so the grid doesn't show.
                    if unit(n) > 0.5 {
                        continue;
                    }
                    let x = (column as f32 + unit(n >> 16)) * CELL;
                    let y = (row as f32 + unit(n >> 32)) * CELL;
                    let brightness = unit(n >> 48);
                    // Mostly pinpricks; one in ten is larger and brighter.
                    let (side, alpha) = if brightness > 0.9 {
                        (2., 0.7)
                    } else {
                        (1.2, 0.22 + 0.33 * brightness)
                    };
                    // A few lean cool, like real stars.
                    let color = if n & 0b11 == 0 {
                        hsla(218. / 360., 0.9, 0.85, alpha)
                    } else {
                        hsla(0., 0., 1., alpha)
                    };
                    let star = Bounds::new(
                        point(bounds.origin.x + px(x), bounds.origin.y + px(y)),
                        size(px(side), px(side)),
                    );
                    window.paint_quad(fill(star, color).corner_radii(px(side / 2.)));
                }
            }
        },
    )
    .absolute()
    .size_full()
}

/// SplitMix64's finalizer: a well-spread hash of a grid cell.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The low 16 bits as a fraction in 0..=1.
fn unit(bits: u64) -> f32 {
    (bits & 0xffff) as f32 / 65535.
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

/// A tooltip for `.tooltip(...)` on any element with an id: `.tooltip(tip("Stop recording"))`.
pub fn tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| {
        let text = text.clone();
        cx.new(|_| Tip { text, key: None }).into()
    }
}

/// Like [`tip`], with the shortcut in a keycap after the text.
pub fn tip_with_key(
    text: impl Into<SharedString>,
    key: impl Into<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    let key = key.into();
    move |_, cx| {
        let (text, key) = (text.clone(), key.clone());
        cx.new(|_| Tip {
            text,
            key: Some(key),
        })
        .into()
    }
}

/// The view behind [`tip`]: a small label of black glass.
struct Tip {
    text: SharedString,
    key: Option<SharedString>,
}

impl Render for Tip {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_window(window);
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(7.))
            .py(px(4.))
            .rounded(px(6.))
            .bg(hsla(0., 0., 0.1, 0.96))
            .border_1()
            .border_color(theme.control_active)
            .text_size(px(11.5))
            .text_color(theme.text)
            .child(self.text.clone())
            .when_some(self.key.clone(), |d, key| {
                d.child(keycap(key, theme.text_muted))
            })
    }
}
