//! First-run setup: asks for Screen Recording (required), the microphone and camera (optional),
//! offers a transcription model, and shows where Annuncia lives, since a menu bar app has no
//! window of its own to open on launch.

use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, FontWeight, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowOptions, div, img,
    prelude::FluentBuilder as _, px, relative, size,
};

use crate::app::{self, Annuncia};
use crate::hotkeys::{HotkeyAction, display_binding};
use crate::models::Transcription;
use crate::native;
use crate::permissions::{Access, Permission};
use crate::ui::icons::icon;
use crate::ui::settings_view::{login_row, row, section};
use crate::ui::{Theme, button, keycap, starfield};

const PADDING: f32 = 40.;
/// The strip under the traffic lights that drags the window.
const HEADER: f32 = 44.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    /// The welcome and the permissions, together.
    Permissions,
    Transcription,
    Ready,
}

impl Step {
    const ALL: [Step; 3] = [Step::Permissions, Step::Transcription, Step::Ready];

    fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }
}

pub struct Onboarding {
    state: Entity<Annuncia>,
    step: Step,
    /// Last read for each of `Permission::ALL`; macOS doesn't announce changes, so it's polled.
    access: [Access; 3],
    /// Screen Recording was asked for, in this run or an earlier one. macOS only reports it
    /// granted after a relaunch, and only prompts once, so from then on the row offers
    /// System Settings and a relaunch.
    screen_requested: bool,
}

/// `focus` must be false unless the app is active (see `app::when_active`).
pub fn open(
    state: Entity<Annuncia>,
    focus: bool,
    cx: &mut App,
) -> Option<WindowHandle<Onboarding>> {
    let bounds = Bounds::centered(None, size(px(600.), px(560.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Set up Annuncia".into()),
            appears_transparent: true,
            traffic_light_position: Some(gpui::point(px(16.), px(16.))),
        }),
        window_background: WindowBackgroundAppearance::Opaque,
        is_resizable: false,
        is_minimizable: false,
        focus,
        show: true,
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        let weak = state.downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            let _ = weak.update(cx, |s, cx| s.onboarding_closed(false, cx));
            true
        });
        cx.new(|cx| {
            app::observe_state(&state, window, cx);
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(500))
                        .await;
                    if this
                        .update(cx, |this: &mut Onboarding, cx| this.refresh(cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
            let settings = &state.read(cx).settings;
            let (screen_requested, onboarded) = (settings.asked_for_screen, settings.onboarded);
            let access = read_access();
            // Usually back from the relaunch Screen Recording needed: macOS's second prompt.
            if access[0] == Access::Granted {
                Permission::confirm_direct_capture();
            }
            // Back from that relaunch with everything allowed: carry on. Opened again from
            // Settings, it starts at the top as asked.
            if all_granted(&access) && !onboarded {
                advance_soon(cx);
            }
            Onboarding {
                state,
                step: Step::Permissions,
                access,
                screen_requested,
            }
        })
    })
    .inspect_err(|err| eprintln!("annuncia: setup window: {err}"))
    .ok()
}

fn read_access() -> [Access; 3] {
    Permission::ALL.map(Permission::access)
}

fn all_granted(access: &[Access; 3]) -> bool {
    access.iter().all(|a| *a == Access::Granted)
}

/// Moves on from the permissions once they're all allowed, after a beat so the last one is
/// seen turning to Allowed.
fn advance_soon(cx: &mut Context<Onboarding>) {
    cx.spawn(async move |this, cx| {
        cx.background_executor()
            .timer(Duration::from_millis(700))
            .await;
        let _ = this.update(cx, |this, cx| {
            if this.step == Step::Permissions && all_granted(&this.access) {
                this.go(Step::Transcription, cx);
            }
        });
    })
    .detach();
}

impl Onboarding {
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let access = read_access();
        if access == self.access {
            return;
        }
        for (i, permission) in Permission::ALL.into_iter().enumerate() {
            if access[i] == Access::Granted && self.access[i] != Access::Granted {
                if permission == Permission::Screen {
                    Permission::confirm_direct_capture();
                }
                self.state
                    .update(cx, |s, cx| s.permission_granted(permission, cx));
            }
        }
        if all_granted(&access) && !all_granted(&self.access) {
            advance_soon(cx);
        }
        self.access = access;
        cx.notify();
    }

    fn access(&self, permission: Permission) -> Access {
        let i = Permission::ALL
            .iter()
            .position(|p| *p == permission)
            .unwrap_or(0);
        self.access[i]
    }

    fn go(&mut self, step: Step, cx: &mut Context<Self>) {
        self.step = step;
        cx.notify();
    }

    fn request(&mut self, permission: Permission, cx: &mut Context<Self>) {
        if permission == Permission::Screen {
            self.screen_requested = true;
            self.state.update(cx, |s, cx| {
                s.update_settings(cx, |s| s.asked_for_screen = true)
            });
        }
        // The poll picks up the answer.
        permission.request(|_| {});
        cx.notify();
    }

    fn next_enabled(&self) -> bool {
        self.step != Step::Permissions || self.access(Permission::Screen) == Access::Granted
    }

    fn next(&mut self, cx: &mut Context<Self>) {
        match self.step {
            Step::Permissions if self.next_enabled() => self.go(Step::Transcription, cx),
            Step::Permissions => {}
            Step::Transcription => self.go(Step::Ready, cx),
            Step::Ready => self.state.update(cx, |s, cx| s.finish_onboarding(cx)),
        }
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        let i = self.step.index();
        if i > 0 {
            self.go(Step::ALL[i - 1], cx);
        }
    }

    // MARK: steps

    fn permissions(&self, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        let screen = self.access(Permission::Screen) == Access::Granted;
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(img("images/icon.png").size(px(56.)).mb(px(8.)))
            .child(title("Welcome to Annuncia"))
            .child(subtitle(
                "Record your screen with your voice and a camera bubble. Allow what you'll use; \
                 Annuncia only captures anything while you're recording.",
                theme,
            ))
            .child(
                div().mt(px(16.)).child(
                    section(theme)
                        .child(self.permission_row(Permission::Screen, theme, cx))
                        .child(self.permission_row(Permission::Microphone, theme, cx))
                        .child(self.permission_row(Permission::Camera, theme, cx)),
                ),
            )
            .when(!screen, |d| {
                d.child(
                    div()
                        .mt(px(4.))
                        .text_size(px(12.))
                        .text_color(theme.text_muted)
                        .child("Screen Recording is the one Annuncia can't record without."),
                )
            })
            .into_any_element()
    }

    fn permission_row(
        &self,
        permission: Permission,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let access = self.access(permission);
        let (name, icon_name, purpose) = match permission {
            Permission::Screen => ("Screen Recording", "display", "To record your screen"),
            Permission::Microphone => ("Microphone", "mic", "For your voiceover. Optional"),
            Permission::Camera => ("Camera", "camera", "For the camera bubble. Optional"),
        };
        let relaunch = permission == Permission::Screen && self.screen_requested;
        let detail: SharedString = match access {
            Access::Granted if permission == Permission::Screen => {
                "If macOS asks to let Annuncia skip its window picker, choose Allow".into()
            }
            Access::Granted => purpose.into(),
            _ if relaunch => {
                "Turn on Annuncia in System Settings, then reopen it so macOS applies it".into()
            }
            Access::Denied => "Turned off in System Settings".into(),
            Access::Undetermined => purpose.into(),
        };

        let url = permission.settings_url();
        let control = match access {
            Access::Granted => allowed(theme).into_any_element(),
            _ if relaunch => div()
                .flex()
                .gap(px(6.))
                .child(
                    button(
                        SharedString::from(format!("{name}-settings")),
                        "Open Settings",
                        false,
                        theme,
                    )
                    .on_click(move |_, _, _| native::open(url)),
                )
                .child(button("relaunch", "Quit & Reopen", true, theme).on_click(
                    cx.listener(|this, _, _, cx| this.state.update(cx, |s, cx| s.relaunch(cx))),
                ))
                .into_any_element(),
            Access::Denied => button(
                SharedString::from(format!("{name}-settings")),
                "Open Settings",
                false,
                theme,
            )
            .on_click(move |_, _, _| native::open(url))
            .into_any_element(),
            Access::Undetermined => button(
                SharedString::from(format!("{name}-allow")),
                "Allow",
                true,
                theme,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.request(permission, cx)))
            .into_any_element(),
        };

        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(14.))
            .py(px(12.))
            .child(
                div()
                    .size(px(30.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.))
                    .bg(if access == Access::Granted {
                        theme.accent
                    } else {
                        theme.icon_off
                    })
                    .child(icon(icon_name, 16., theme.text)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_size(px(13.)).child(name))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(theme.text_muted)
                            .child(detail),
                    ),
            )
            .child(control)
    }

    fn transcription(&self, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let chosen = state.settings.transcription;
        let download = state
            .download
            .as_ref()
            .filter(|d| d.model == chosen)
            .map(|d| d.fraction());
        let error = state.download_error.clone();

        let status: Option<AnyElement> = match (chosen, download, error) {
            (Transcription::Off, ..) => None,
            (_, Some(fraction), _) => {
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(div().text_size(px(12.)).text_color(theme.text_muted).child(
                            format!(
                                "Downloading {}: {}%. It carries on in the background, so you \
                                 can keep going.",
                                chosen.model_name(),
                                (fraction * 100.) as u32
                            ),
                        ))
                        .child(progress(fraction, theme))
                        .into_any_element(),
                )
            }
            (_, None, Some(error)) => Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(12.))
                            .text_color(theme.record)
                            .child(error),
                    )
                    .child(
                        button("retry-download", "Try again", false, theme).on_click(cx.listener(
                            |this, _, _, cx| this.state.update(cx, |s, cx| s.retry_download(cx)),
                        )),
                    )
                    .into_any_element(),
            ),
            _ if chosen.is_installed() => Some(
                div()
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .child(format!("{} is downloaded and ready.", chosen.model_name()))
                    .into_any_element(),
            ),
            _ => None,
        };

        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(title("Transcribe your recordings?"))
            .child(subtitle(
                "Transcripts are made on this Mac, and nothing leaves it. They make the library \
                 searchable and let the editor cut filler words.",
                theme,
            ))
            .child(
                div()
                    .mt(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(self.model_option(Transcription::Off, chosen, theme, cx))
                    .child(self.model_option(Transcription::English, chosen, theme, cx))
                    .child(self.model_option(Transcription::Multilingual, chosen, theme, cx)),
            )
            .children(status.map(|s| div().mt(px(8.)).child(s)))
            .into_any_element()
    }

    fn model_option(
        &self,
        model: Transcription,
        chosen: Transcription,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let size = model
            .artifact()
            .map(|a| format!("{} MB", a.bytes / 1_000_000))
            .unwrap_or_default();
        let (name, detail) = match model {
            Transcription::Off => (
                "Not now",
                "You can turn it on later in Settings".to_string(),
            ),
            Transcription::English => (
                "English",
                format!("Parakeet, a {size} download. Fast, and timed word by word"),
            ),
            Transcription::Multilingual => (
                "Other languages",
                format!("Whisper, a {size} download. About 100 languages"),
            ),
        };
        let selected = model == chosen;
        div()
            .id(SharedString::from(format!("model-{name}")))
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(14.))
            .py(px(11.))
            .rounded(px(12.))
            .border_1()
            .border_color(if selected {
                theme.accent
            } else {
                theme.hairline
            })
            .bg(theme.surface)
            .cursor_pointer()
            .hover(|s| s.bg(theme.tile))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.state
                    .update(cx, |s, cx| s.set_transcription(model, cx))
            }))
            .child(radio(selected, theme))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_size(px(13.)).child(name))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(theme.text_muted)
                            .child(detail),
                    ),
            )
            .when(model.is_installed(), |d| {
                d.child(
                    div()
                        .text_size(px(11.5))
                        .text_color(theme.text_muted)
                        .child("Downloaded"),
                )
            })
    }

    fn ready(&self, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let hotkeys = &state.settings.hotkeys;
        let shortcuts: Vec<_> = [HotkeyAction::ToggleRecording, HotkeyAction::OpenLauncher]
            .into_iter()
            .filter(|action| !action.binding(hotkeys).is_empty())
            .map(|action| {
                row(
                    action.label(),
                    None,
                    keycap(display_binding(action.binding(hotkeys)), theme.text),
                    theme,
                )
            })
            .collect();
        let downloading = state.download.as_ref().map(|d| {
            format!(
                "{} is still downloading ({}%). Recordings are transcribed once it's done.",
                d.model.model_name(),
                (d.fraction() * 100.) as u32
            )
        });
        let login = login_row(&self.state, theme, cx);

        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(title("Annuncia lives in your menu bar"))
            .child(subtitle(
                "It has no Dock icon. Click its icon at the top of the screen to start a \
                 recording, or use a shortcut from anywhere.",
                theme,
            ))
            .when(!shortcuts.is_empty(), |d| {
                d.child(div().mt(px(16.)).child(section(theme).children(shortcuts)))
            })
            .children(login.map(|row| div().mt(px(8.)).child(section(theme).child(row))))
            .children(downloading.map(|text| {
                div()
                    .mt(px(8.))
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .child(text)
            }))
            .into_any_element()
    }

    fn footer(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let next_label = match self.step {
            Step::Ready => "Done",
            _ => "Continue",
        };
        let enabled = self.next_enabled();
        div()
            .flex()
            .items_center()
            .justify_between()
            .px(px(PADDING))
            .pb(px(28.))
            .child(div().flex().gap(px(6.)).children(Step::ALL.map(|step| {
                div().size(px(6.)).rounded_full().bg(if step == self.step {
                    theme.accent
                } else {
                    theme.control_active
                })
            })))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .when(self.step != Step::Permissions, |d| {
                        d.child(
                            button("back", "Back", false, theme)
                                .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
                        )
                    })
                    .child(
                        button("next", next_label, true, theme)
                            .when(!enabled, |b| b.opacity(0.4).cursor_default())
                            .on_click(cx.listener(|this, _, _, cx| this.next(cx))),
                    ),
            )
    }
}

impl Render for Onboarding {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_window(window);
        let content = match self.step {
            Step::Permissions => self.permissions(theme, cx),
            Step::Transcription => self.transcription(theme, cx),
            Step::Ready => self.ready(theme, cx),
        };
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.window)
            .text_color(theme.text)
            .when(theme.stars, |d| d.child(starfield()))
            .child(
                div()
                    .h(px(HEADER))
                    .flex_none()
                    .on_mouse_down(MouseButton::Left, |_, window, _| {
                        native::begin_window_drag(window)
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .px(px(PADDING))
                    .pt(px(12.))
                    .child(content),
            )
            .child(self.footer(theme, cx))
    }
}

fn title(text: &'static str) -> impl IntoElement {
    div()
        .text_size(px(24.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text)
}

fn subtitle(text: &'static str, theme: Theme) -> gpui::Div {
    div()
        .text_size(px(14.))
        .line_height(relative(1.45))
        .text_color(theme.text_muted)
        .child(text)
}

fn allowed(theme: Theme) -> impl IntoElement {
    div()
        .px(px(10.))
        .py(px(4.))
        .rounded(px(7.))
        .bg(theme.accent.opacity(0.18))
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.text)
        .child("Allowed")
}

fn radio(on: bool, theme: Theme) -> impl IntoElement {
    div()
        .size(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(if on { theme.accent } else { theme.text_muted })
        .when(on, |d| {
            d.child(div().size(px(8.)).rounded_full().bg(theme.accent))
        })
}

fn progress(fraction: f32, theme: Theme) -> impl IntoElement {
    div()
        .h(px(4.))
        .w_full()
        .rounded(px(2.))
        .bg(theme.control_active)
        .child(
            div()
                .h_full()
                .w(relative(fraction.clamp(0., 1.)))
                .rounded(px(2.))
                .bg(theme.accent),
        )
}
