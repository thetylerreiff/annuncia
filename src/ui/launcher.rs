//! The panel that drops from the menu bar icon: pick what to record and which inputs to use.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AppContext as _, Context, Div, Entity, FocusHandle, Focusable, FontWeight,
    Hsla, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, WindowHandle, canvas, div,
    hsla, img, prelude::FluentBuilder as _, px, relative,
};

use crate::app::{self, Annuncia};
use crate::native::{self, OverlayLevel, TopLeftRect, WindowSetup};
use crate::settings::{CaptureMode, tilde};
use crate::ui::icons::icon;
use crate::ui::{Theme, keycap};

const WIDTH: f64 = 320.;
const RADIUS: f64 = 18.;
/// Corner radius of the module tiles inside the panel.
const TILE: f32 = 14.;
/// How often the microphone meter moves on.
const METER_FRAME: Duration = Duration::from_millis(40);
/// A microphone that has delivered nothing for this long reads "No signal".
const SILENCE: Duration = Duration::from_secs(3);
/// How long the window list lights up when Record is asked for with no window picked.
const FLASH: Duration = Duration::from_millis(700);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Picker {
    Camera,
    Microphone,
}

pub struct Launcher {
    state: Entity<Annuncia>,
    focus: FocusHandle,
    picker: Option<Picker>,
    origin: (f64, f64),
    /// Window height last applied; the window follows the content's measured height.
    applied: Rc<Cell<f32>>,
    /// The microphone meter, while the microphone is being metered.
    meter: Option<Meter>,
    /// The window list is lit up until then.
    flash_until: Option<Instant>,
}

struct Meter {
    /// Eased level, 0 to 1.
    level: f32,
    /// When the microphone last delivered any sound.
    heard: Instant,
    silent: bool,
}

pub fn open(
    state: Entity<Annuncia>,
    anchor: Option<TopLeftRect>,
    cx: &mut App,
) -> Option<WindowHandle<Launcher>> {
    let geometry = native::menu_bar_geometry()?;
    let screen = geometry.screen;
    let center = anchor.map_or(screen.x + screen.w - 200., |a| a.x + a.w / 2.);
    let x = (center - WIDTH / 2.).clamp(screen.x + 8., screen.x + screen.w - WIDTH - 8.);
    let y = screen.y + geometry.menu_bar_height + 6.;
    let rect = TopLeftRect {
        x,
        y,
        w: WIDTH,
        h: 360.,
    };
    cx.open_window(app::overlay_options(rect, true), |window, cx| {
        native::setup_window(
            window,
            cx,
            WindowSetup {
                shadow: true,
                corner_radius: Some(RADIUS),
                glass: Some(RADIUS),
                key: true,
                ..app::overlay_setup(OverlayLevel::Floating, rect)
            },
        );
        cx.new(|cx| Launcher::new(state, (x, y), window, cx))
    })
    .inspect_err(|err| eprintln!("annuncia: launcher window: {err}"))
    .ok()
}

impl Launcher {
    fn new(
        state: Entity<Annuncia>,
        origin: (f64, f64),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        app::observe_state(&state, window, cx);
        let focus = cx.focus_handle();
        window.focus(&focus);

        // Close when another app takes focus; clicks on our own bubble keep it open.
        let weak_state = state.downgrade();
        cx.observe_window_activation(window, move |_, window, cx| {
            if window.is_window_active() {
                return;
            }
            let weak_state = weak_state.clone();
            cx.spawn(async move |_, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
                let _ = cx.update(|cx| {
                    if !native::app_has_key_window() {
                        let _ = weak_state.update(cx, |s, cx| s.close_launcher(cx));
                    }
                });
            })
            .detach();
        })
        .detach();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(METER_FRAME).await;
                if this.update(cx, |this, cx| this.step_meter(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        Self {
            state,
            focus,
            picker: None,
            origin,
            applied: Rc::new(Cell::new(0.)),
            meter: None,
            flash_until: None,
        }
    }

    /// Record was asked for in Window mode with no window picked: light up the list to pick
    /// from.
    pub fn ask_for_window(&mut self, cx: &mut Context<Self>) {
        let until = Instant::now() + FLASH;
        self.flash_until = Some(until);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FLASH).await;
            let _ = this.update(cx, |this, cx| {
                // A later ask keeps it lit for its own full time.
                if this.flash_until == Some(until) {
                    this.flash_until = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Eases the meter toward the microphone's level the way the avatar's pulse does: quick to
    /// rise with each syllable, slower to fall.
    fn step_meter(&mut self, cx: &mut Context<Self>) {
        let reading = self
            .state
            .read(cx)
            .mic_level
            .as_ref()
            .map(|m| (m.level(), m.has_signal()));
        let Some((level, signal)) = reading else {
            if self.meter.take().is_some() {
                cx.notify();
            }
            return;
        };
        let now = Instant::now();
        let meter = self.meter.get_or_insert(Meter {
            level: 0.,
            heard: now,
            silent: false,
        });
        if signal {
            meter.heard = now;
        }
        let rate = if level > meter.level { 0.7 } else { 0.3 };
        let before = meter.level;
        meter.level += (level - meter.level) * rate;
        let silent = now.duration_since(meter.heard) >= SILENCE;
        if (meter.level - before).abs() > 0.002 || silent != meter.silent {
            meter.silent = silent;
            cx.notify();
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => self.state.update(cx, |s, cx| s.close_launcher(cx)),
            "enter" => self.state.update(cx, |s, cx| s.start_recording(cx)),
            _ => {}
        }
    }
}

impl Focusable for Launcher {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Launcher {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_window(window);

        let this = cx.entity();
        let state = self.state.read(cx);
        let settings = &state.settings;
        let mode = settings.capture_mode;
        let handle = self.state.clone();

        let picked = state
            .selected_window
            .and_then(|id| state.windows.iter().find(|w| w.id == id));
        // In Window mode Record waits for a pick, so say so here, where the pick is made.
        let (window_subtitle, window_subtitle_color) = match picked {
            Some(w) => (w.app.clone(), theme.text_muted),
            None if mode == CaptureMode::Window => {
                ("Pick a window to record".to_string(), theme.record)
            }
            None => ("Pick a window".to_string(), theme.text_muted),
        };
        let modes = div()
            .flex()
            .gap(px(8.))
            .child(self.mode_tile(
                CaptureMode::Screen,
                "display",
                "Full screen",
                "Main display".into(),
                theme.text_muted,
                mode,
                theme,
            ))
            .child(self.mode_tile(
                CaptureMode::Window,
                "window",
                "Window",
                window_subtitle,
                window_subtitle_color,
                mode,
                theme,
            ));
        let windows = (mode == CaptureMode::Window)
            .then(|| self.window_list(state, self.flash_until.is_some(), theme));

        let camera_name = settings
            .camera_device
            .as_ref()
            .and_then(|id| state.cameras.iter().find(|c| &c.id == id))
            .or(state.cameras.first())
            .map_or("No camera".to_string(), |c| c.name.clone());
        let mic_name = settings
            .mic_device
            .as_ref()
            .and_then(|id| state.microphones.iter().find(|(mid, _)| mid == id))
            .or(state.microphones.first())
            .map_or("Default microphone".to_string(), |(_, name)| name.clone());

        let camera_tile = self.device_tile(
            "camera",
            "Camera",
            device_line(camera_name, theme).into_any_element(),
            settings.camera_enabled,
            Picker::Camera,
            theme,
            &this,
            |s| s.camera_enabled = !s.camera_enabled,
        );
        let camera_options = (self.picker == Some(Picker::Camera)).then(|| {
            self.options(
                state
                    .cameras
                    .iter()
                    .map(|c| (c.id.clone(), c.name.clone()))
                    .collect(),
                settings.camera_device.clone(),
                theme,
                &this,
                |s, id| s.camera_device = id,
            )
        });
        // A meter under the name while the microphone is metered, so a silent one shows before
        // the recording rather than after.
        let mic_detail = match &self.meter {
            Some(meter) => div()
                .child(if meter.silent {
                    device_line("No signal".into(), theme)
                } else {
                    device_line(mic_name, theme)
                })
                .child(level_bar(meter.level, theme))
                .into_any_element(),
            None => device_line(mic_name, theme).into_any_element(),
        };
        let mic_tile = self.device_tile(
            "mic",
            "Microphone",
            mic_detail,
            settings.mic_enabled,
            Picker::Microphone,
            theme,
            &this,
            |s| s.mic_enabled = !s.mic_enabled,
        );
        let mic_options = (self.picker == Some(Picker::Microphone)).then(|| {
            self.options(
                state.microphones.clone(),
                settings.mic_device.clone(),
                theme,
                &this,
                |s, id| s.mic_device = id,
            )
        });

        let system_audio = tile(theme)
            .id("system-audio")
            .flex_1()
            .h(px(48.))
            .px(px(10.))
            .gap(px(9.))
            .cursor_pointer()
            .hover(|s| s.bg(theme.tile_hover))
            .on_click({
                let handle = handle.clone();
                move |_, _, cx| {
                    handle.update(cx, |s, cx| {
                        s.update_settings(cx, |s| s.system_audio = !s.system_audio)
                    })
                }
            })
            .child(icon_circle(settings.system_audio, "speaker", theme))
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .child("System audio"),
            );

        let can_start = mode == CaptureMode::Screen || state.selected_window.is_some();
        let white = hsla(0., 0., 1., 1.);
        let record = div()
            .id("start")
            .flex_1()
            .h(px(48.))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(7.))
            .rounded(px(TILE))
            .bg(theme.record)
            .text_color(white)
            .text_size(px(13.))
            .font_weight(FontWeight::SEMIBOLD)
            // With no window picked it's inert; the Window tile says what's missing.
            .when(can_start, |d| {
                let handle = handle.clone();
                d.cursor_pointer()
                    .hover(|s| s.opacity(0.9))
                    .on_click(move |_, _, cx| handle.update(cx, |s, cx| s.start_recording(cx)))
            })
            .when(!can_start, |d| d.opacity(0.45))
            .child(div().size(px(10.)).rounded_full().bg(white))
            .child("Record")
            .child(keycap(
                crate::ui::hud::toggle_hint(state),
                white.opacity(0.85),
            ));

        let save_dir = settings.save_dir.clone();
        let footer = div()
            .flex()
            .items_center()
            .gap(px(2.))
            .pl(px(4.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.5))
                    .text_color(theme.text_muted)
                    .child(format!("Saves to {}", tilde(&save_dir))),
            )
            .child(icon_button("library", "library", theme).on_click({
                let handle = handle.clone();
                move |_, _, cx| handle.update(cx, |s, cx| s.open_library(cx))
            }))
            .child(icon_button("settings", "gear", theme).on_click({
                let handle = handle.clone();
                move |_, _, cx| handle.update(cx, |s, cx| s.open_settings(cx))
            }));

        let (applied, origin) = (self.applied.clone(), self.origin);
        let content = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(12.))
            .child(modes)
            .children(windows)
            .child(camera_tile)
            .children(camera_options)
            .child(mic_tile)
            .children(mic_options)
            .child(div().flex().gap(px(8.)).child(system_audio).child(record))
            .child(footer)
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let height: f32 = bounds.size.height.into();
                        if (height - applied.get()).abs() > 0.5 {
                            applied.set(height);
                            native::set_frame(
                                window,
                                cx,
                                TopLeftRect {
                                    x: origin.0,
                                    y: origin.1,
                                    w: WIDTH,
                                    h: f64::from(height),
                                },
                            );
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            );

        div()
            .id("launcher")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .relative()
            .bg(theme.panel)
            .text_color(theme.text)
            .when(theme.stars, |d| d.child(super::starfield()))
            .child(content)
    }
}

/// A Control Center module: a rounded tile one step lighter than the panel.
fn tile(theme: Theme) -> Div {
    div().flex().items_center().rounded(px(TILE)).bg(theme.tile)
}

/// The round icon that leads each module, filled with the accent while it's on.
fn icon_circle(on: bool, name: &'static str, theme: Theme) -> Div {
    div()
        .size(px(28.))
        .flex_none()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(if on { theme.accent } else { theme.icon_off })
        .child(icon(
            name,
            15.,
            if on { theme.accent_text } else { theme.text },
        ))
}

/// The muted line under a device tile's label.
fn device_line(text: String, theme: Theme) -> Div {
    div()
        .text_size(px(11.5))
        .text_color(theme.text_muted)
        .truncate()
        .child(text)
}

/// A thin accent bar as long as the microphone is loud.
fn level_bar(level: f32, theme: Theme) -> Div {
    div()
        .mt(px(4.))
        .h(px(3.))
        .w_full()
        .rounded_full()
        .bg(theme.icon_off)
        .child(
            div()
                .h_full()
                .w(relative(level.clamp(0., 1.)))
                .rounded_full()
                .bg(theme.accent),
        )
}

fn icon_button(id: &'static str, name: &'static str, theme: Theme) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .size(px(26.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .cursor_pointer()
        .hover(|s| s.bg(theme.tile_hover))
        .child(icon(name, 15., theme.text_muted))
}

impl Launcher {
    fn mode_tile(
        &self,
        mode: CaptureMode,
        icon_name: &'static str,
        title: &'static str,
        subtitle: String,
        subtitle_color: Hsla,
        current: CaptureMode,
        theme: Theme,
    ) -> impl IntoElement {
        let handle = self.state.clone();
        let selected = mode == current;
        tile(theme)
            .id(title)
            .flex_1()
            .min_w_0()
            .flex_col()
            .items_start()
            .gap(px(8.))
            .p(px(10.))
            .cursor_pointer()
            .hover(|s| s.bg(theme.tile_hover))
            .on_click(move |_, _, cx| {
                handle.update(cx, |s, cx| s.update_settings(cx, |s| s.capture_mode = mode))
            })
            .child(icon_circle(selected, icon_name, theme))
            .child(
                div()
                    .w_full()
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(subtitle_color)
                            .truncate()
                            .child(subtitle),
                    ),
            )
    }

    fn window_list(&self, state: &Annuncia, flashing: bool, theme: Theme) -> AnyElement {
        let handle = self.state.clone();
        let list = div()
            .id("windows")
            .max_h(px(208.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(2.))
            .p(px(4.))
            .rounded(px(TILE))
            .bg(theme.tile)
            .when(flashing, |d| d.bg(theme.record.opacity(0.18)));
        if state.windows.is_empty() {
            let message = if state.windows_loading {
                "Finding windows…"
            } else {
                "No windows to record"
            };
            return list
                .child(
                    div()
                        .px(px(8.))
                        .py(px(6.))
                        .text_size(px(12.))
                        .text_color(theme.text_muted)
                        .child(message),
                )
                .into_any_element();
        }
        list.children(state.windows.iter().map(|w| {
            let selected = state.selected_window == Some(w.id);
            let id = w.id;
            let handle = handle.clone();
            div()
                .id(SharedString::from(format!("window-{id}")))
                .px(px(8.))
                .py(px(6.))
                .rounded(px(10.))
                .cursor_pointer()
                .when(selected, |d| d.bg(theme.accent.opacity(0.22)))
                .when(!selected, |d| d.hover(|s| s.bg(theme.tile_hover)))
                .flex()
                .items_center()
                .gap(px(8.))
                .on_click(move |_, _, cx| {
                    handle.update(cx, |s, cx| {
                        s.selected_window = Some(id);
                        cx.notify();
                    })
                })
                // An empty square when there's no icon, so the names still line up.
                .child(match w.icon.clone() {
                    Some(path) => img(path).size(px(18.)).flex_none().into_any_element(),
                    None => div().size(px(18.)).flex_none().into_any_element(),
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(px(12.5))
                                .font_weight(FontWeight::MEDIUM)
                                .truncate()
                                .child(w.app.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(theme.text_muted)
                                .truncate()
                                .child(w.title.clone()),
                        ),
                )
        }))
        .into_any_element()
    }

    /// The icon switches the input on and off; the rest of the tile opens the device list.
    #[allow(clippy::too_many_arguments)]
    fn device_tile(
        &self,
        id: &'static str,
        label: &'static str,
        detail: AnyElement,
        enabled: bool,
        picker: Picker,
        theme: Theme,
        this: &Entity<Self>,
        flip: impl Fn(&mut crate::settings::Settings) + 'static,
    ) -> AnyElement {
        let this = this.clone();
        let handle = self.state.clone();
        let open = self.picker == Some(picker);
        tile(theme)
            .gap(px(10.))
            .pl(px(10.))
            .child(
                div()
                    .id(SharedString::from(format!("{id}-toggle")))
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.85))
                    .on_click(move |_, _, cx| {
                        handle.update(cx, |s, cx| s.update_settings(cx, &flip))
                    })
                    .child(icon_circle(enabled, id, theme)),
            )
            .child(
                div()
                    .id(SharedString::from(format!("{id}-picker")))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .py(px(9.))
                    .pr(px(10.))
                    .cursor_pointer()
                    .on_click(move |_, _, cx| {
                        this.update(cx, |this, cx| {
                            this.picker = if open { None } else { Some(picker) };
                            cx.notify();
                        })
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(label),
                            )
                            .child(detail),
                    )
                    .child(icon(
                        if open {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        },
                        14.,
                        theme.text_muted,
                    )),
            )
            .into_any_element()
    }

    fn options(
        &self,
        devices: Vec<(String, String)>,
        selected: Option<String>,
        theme: Theme,
        this: &Entity<Self>,
        set: impl Fn(&mut crate::settings::Settings, Option<String>) + 'static,
    ) -> AnyElement {
        let set = Rc::new(set);
        div()
            .flex()
            .flex_col()
            .gap(px(1.))
            .p(px(4.))
            .rounded(px(TILE))
            .bg(theme.tile)
            .children(devices.into_iter().enumerate().map(|(i, (id, name))| {
                let is_selected =
                    selected.as_deref() == Some(id.as_str()) || (selected.is_none() && i == 0);
                let set = set.clone();
                let handle = self.state.clone();
                let this = this.clone();
                div()
                    .id(SharedString::from(format!("option-{i}")))
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(10.))
                    .flex()
                    .justify_between()
                    .text_size(px(12.5))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.tile_hover))
                    .on_click(move |_, _, cx| {
                        let id = id.clone();
                        let set = set.clone();
                        handle.update(cx, |s, cx| s.update_settings(cx, |s| set(s, Some(id))));
                        this.update(cx, |this, cx| {
                            this.picker = None;
                            cx.notify();
                        })
                    })
                    .child(div().truncate().child(name))
                    .when(is_selected, |d| {
                        d.child(div().text_color(theme.accent).child("✓"))
                    })
            }))
            .into_any_element()
    }
}
