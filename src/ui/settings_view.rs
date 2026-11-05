use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, PathPromptOptions,
    Render, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    img, prelude::FluentBuilder as _, px,
};

use crate::app::{self, Annuncia};
use crate::cleanup::Cleanup;
use crate::hotkeys::{HotkeyAction, display_binding, keystroke_to_binding};
use crate::models::Transcription;
use crate::settings::{self, BubbleSize, Codec, Settings, UpdateChannel, tilde};
use crate::ui::icons::icon;
use crate::ui::{Theme, button, segmented, toggle};

pub struct SettingsView {
    state: Entity<Annuncia>,
    focus: FocusHandle,
    /// Shortcut currently being recorded.
    capturing: Option<HotkeyAction>,
    page: Page,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    General,
    Recording,
    AfterRecording,
    Camera,
    Shortcuts,
    About,
}

impl Page {
    const ALL: [Page; 6] = [
        Page::General,
        Page::Recording,
        Page::AfterRecording,
        Page::Camera,
        Page::Shortcuts,
        Page::About,
    ];

    fn title(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Recording => "Recording",
            Page::AfterRecording => "After recording",
            Page::Camera => "Camera bubble",
            Page::Shortcuts => "Shortcuts",
            Page::About => "About",
        }
    }
}

/// Asks the library window to go back from Settings to the recordings.
pub struct Back;

impl EventEmitter<Back> for SettingsView {}

impl SettingsView {
    /// Settings is a view of the library window, not a window of its own.
    pub fn new(state: Entity<Annuncia>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        app::observe_state(&state, window, cx);
        SettingsView {
            state,
            focus: cx.focus_handle(),
            capturing: None,
            page: Page::General,
        }
    }

    pub fn show_page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        cx.notify();
    }

    /// Stops recording a shortcut, if one was being recorded, so ours work again.
    pub fn release_hotkeys(&mut self, cx: &mut Context<Self>) {
        if self.capturing.take().is_some() {
            self.state.update(cx, |s, cx| s.resume_hotkeys(cx));
        }
    }
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SettingsView {
    fn update(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut Settings)) {
        self.state.update(cx, |s, cx| s.update_settings(cx, f));
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(action) = self.capturing else { return };
        let ks = &event.keystroke;
        let plain = !(ks.modifiers.platform
            || ks.modifiers.alt
            || ks.modifiers.control
            || ks.modifiers.shift);
        let binding = match ks.key.as_str() {
            "escape" if plain => None,
            "backspace" | "delete" if plain => Some(String::new()),
            _ => match keystroke_to_binding(ks) {
                Some(binding) => Some(binding),
                // Bare keys: keep listening.
                None => return,
            },
        };
        self.capturing = None;
        if let Some(binding) = binding {
            self.update(cx, |s| *action.binding_mut(&mut s.hotkeys) = binding);
        }
        self.state.update(cx, |s, cx| s.resume_hotkeys(cx));
        cx.notify();
    }

    fn choose_save_dir(&self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        let state = self.state.clone();
        cx.spawn(async move |_, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(dir) = paths.into_iter().next()
            {
                let _ = state.update(cx, |s, cx| s.update_settings(cx, |s| s.save_dir = dir));
            }
        })
        .detach();
    }

    fn choose_avatar(&self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Use as avatar".into()),
        });
        let state = self.state.clone();
        cx.spawn(async move |_, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = state.update(cx, |s, cx| {
                    s.update_settings(cx, |s| s.avatar_image = Some(path))
                });
            }
        })
        .detach();
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_window(window);
        let settings = self.state.read(cx).settings.clone();
        let problems = self.state.read(cx).hotkey_problems.clone();

        let general = section(theme)
            .child(row(
                "Save recordings to",
                Some(tilde(&settings.save_dir)),
                div()
                    .flex()
                    .gap(px(6.))
                    .child(
                        button("choose-dir", "Choose…", false, theme)
                            .on_click(cx.listener(|this, _, _, cx| this.choose_save_dir(cx))),
                    )
                    .child(button("reveal-dir", "Show", false, theme).on_click({
                        let dir = settings.save_dir.clone();
                        move |_, _, _| {
                            let _ = std::fs::create_dir_all(&dir);
                            crate::native::open(&dir);
                        }
                    })),
                theme,
            ))
            .child(row(
                "Countdown",
                None,
                self.choice(
                    "countdown",
                    &["Off", "3 s", "5 s"],
                    match settings.countdown_secs {
                        0 => 0,
                        s if s <= 3 => 1,
                        _ => 2,
                    },
                    theme,
                    cx,
                    |s, i| s.countdown_secs = [0, 3, 5][i],
                ),
                theme,
            ))
            .children(self.updates_row(theme, cx))
            .children(self.channel_row(theme, cx))
            .child(row(
                "Settings file",
                Some(tilde(&settings::settings_path())),
                button("open-config", "Show", false, theme).on_click(|_, _, _| {
                    crate::native::open(settings::config_dir());
                }),
                theme,
            ));

        let recording = section(theme)
            .child(row(
                "Frame rate",
                None,
                self.choice(
                    "fps",
                    &["30 fps", "60 fps"],
                    usize::from(settings.fps >= 60),
                    theme,
                    cx,
                    |s, i| s.fps = [30, 60][i],
                ),
                theme,
            ))
            .child(row(
                "Video format",
                Some("HEVC files are smaller; H.264 plays everywhere".into()),
                self.choice(
                    "codec",
                    &["H.264", "HEVC"],
                    usize::from(settings.codec == Codec::Hevc),
                    theme,
                    cx,
                    |s, i| s.codec = [Codec::H264, Codec::Hevc][i],
                ),
                theme,
            ))
            .child(row(
                "Show cursor",
                None,
                self.switch("cursor", settings.show_cursor, theme, cx, |s| {
                    s.show_cursor = !s.show_cursor
                }),
                theme,
            ))
            .child(row(
                "Record system audio",
                Some("Sound from apps, alongside your microphone".into()),
                self.switch("system-audio", settings.system_audio, theme, cx, |s| {
                    s.system_audio = !s.system_audio
                }),
                theme,
            ));

        let after = section(theme)
            .child(row(
                "Open the editor",
                Some("Trim and cut a recording as soon as it's saved".into()),
                self.switch("open-editor", settings.open_editor, theme, cx, |s| {
                    s.open_editor = !s.open_editor
                }),
                theme,
            ))
            .child(self.transcription_row(settings.transcription, theme, cx))
            .child(self.cleanup_row(settings.cleanup, settings.transcription, theme));

        let avatar_label = settings
            .avatar_image
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned());
        let camera = section(theme)
            .child(row(
                "Size",
                None,
                self.choice(
                    "bubble",
                    &["Small", "Medium", "Large"],
                    settings.bubble_size as usize,
                    theme,
                    cx,
                    |s, i| {
                        s.bubble_size =
                            [BubbleSize::Small, BubbleSize::Medium, BubbleSize::Large][i]
                    },
                ),
                theme,
            ))
            .child(row(
                "Pop out of the bubble",
                Some("Your head rises above the circle while the rest stays inside it".into()),
                self.switch("popout", settings.bubble_popout, theme, cx, |s| {
                    s.bubble_popout = !s.bubble_popout
                }),
                theme,
            ))
            .child(row(
                "Avatar when the camera is off",
                Some(
                    "Marbled ink that moves with your voice, or a photo. Off hides the bubble"
                        .into(),
                ),
                self.switch("avatar", settings.avatar_enabled, theme, cx, |s| {
                    s.avatar_enabled = !s.avatar_enabled
                }),
                theme,
            ))
            .children(settings.avatar_enabled.then(|| {
                row(
                    "Avatar photo",
                    Some(
                        avatar_label
                            .clone()
                            .unwrap_or_else(|| "None, so the marbled ink shows".into()),
                    ),
                    div()
                        .flex()
                        .gap(px(6.))
                        .child(
                            button("choose-avatar", "Choose…", false, theme)
                                .on_click(cx.listener(|this, _, _, cx| this.choose_avatar(cx))),
                        )
                        .when(avatar_label.is_some(), |d| {
                            d.child(button("clear-avatar", "Clear", false, theme).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.update(cx, |s| s.avatar_image = None)
                                }),
                            ))
                        }),
                    theme,
                )
            }));

        let mut shortcuts = section(theme);
        for action in HotkeyAction::ALL {
            let capturing = self.capturing == Some(action);
            let label: SharedString = if capturing {
                "Press keys…".into()
            } else {
                display_binding(action.binding(&settings.hotkeys)).into()
            };
            shortcuts = shortcuts.child(row(
                action.label(),
                None,
                button(
                    SharedString::from(format!("hotkey-{action:?}")),
                    label,
                    capturing,
                    theme,
                )
                .min_w(px(96.))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.capturing = Some(action);
                    window.focus(&this.focus);
                    // Registered shortcuts would swallow the keypress before we see it.
                    this.state.update(cx, |s, _| s.suspend_hotkeys());
                    cx.notify();
                })),
                theme,
            ));
        }
        let shortcuts = shortcuts
            .child(
                div()
                    .px(px(14.))
                    .py(px(10.))
                    .text_size(px(11.5))
                    .text_color(theme.text_muted)
                    .child(
                        "Click a shortcut, then press the new keys. Esc cancels, Delete clears.",
                    ),
            )
            .children(problems.into_iter().map(|p| {
                div()
                    .px(px(14.))
                    .py(px(8.))
                    .text_size(px(11.5))
                    .text_color(theme.record)
                    .child(p)
            }));

        let page = match self.page {
            Page::General => general.into_any_element(),
            Page::Recording => recording.into_any_element(),
            Page::AfterRecording => after.into_any_element(),
            Page::Camera => camera.into_any_element(),
            Page::Shortcuts => shortcuts.into_any_element(),
            Page::About => about(theme),
        };

        div()
            .id("settings")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .relative()
            .flex()
            .bg(theme.window)
            .text_color(theme.text)
            .when(theme.stars, |d| d.child(super::starfield()))
            .child(self.sidebar(theme, cx))
            .child(
                div()
                    .id("page")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(16.))
                            .pt(px(44.))
                            .px(px(26.))
                            .pb(px(26.))
                            .child(
                                div()
                                    .text_size(px(24.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(self.page.title()),
                            )
                            .child(page),
                    ),
            )
    }
}

impl SettingsView {
    /// Page list over the dithered angel, fading into the black below.
    fn sidebar(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(200.))
            .flex_none()
            .h_full()
            .relative()
            .overflow_hidden()
            .bg(gpui::hsla(0., 0., 0.02, 1.))
            .border_r_1()
            .border_color(theme.hairline)
            .child(
                img("images/sidebar.png")
                    .absolute()
                    .top_0()
                    .left_0()
                    .w_full()
                    .h(px(130.))
                    .opacity(0.32),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .pt(px(52.))
                    .px(px(10.))
                    .child(
                        div()
                            .id("back")
                            .flex()
                            .items_center()
                            .gap(px(2.))
                            .mb(px(10.))
                            .pl(px(4.))
                            .pr(px(8.))
                            .py(px(5.))
                            .rounded(px(7.))
                            .cursor_pointer()
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            // Full strength: it sits over the dithered art.
                            .text_color(theme.text)
                            .hover(|s| s.bg(theme.control_hover))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.release_hotkeys(cx);
                                cx.emit(Back);
                            }))
                            .child(icon("chevron-left", 15., theme.text))
                            .child("Library"),
                    )
                    .children(Page::ALL.into_iter().map(|page| {
                        let selected = page == self.page;
                        div()
                            .id(page.title())
                            .px(px(10.))
                            .py(px(6.))
                            .rounded(px(7.))
                            .text_size(px(13.))
                            .cursor_pointer()
                            .when(selected, |d| d.bg(gpui::hsla(0., 0., 1., 0.1)))
                            .when(!selected, |d| {
                                d.text_color(theme.text_muted)
                                    .hover(|s| s.bg(theme.control).text_color(theme.text))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.page = page;
                                cx.notify();
                            }))
                            .child(page.title())
                    })),
            )
    }
}

/// The icon, name, version and license.
fn about(theme: Theme) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(14.))
        .child(img("images/icon.png").size(px(48.)).flex_none())
        .child(
            div()
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Annuncia"),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.text_muted)
                        .child(format!(
                            "Version {} · MIT License",
                            crate::updater::display_version()
                        )),
                ),
        )
        .into_any_element()
}

impl SettingsView {
    /// Updates, when running from the app bundle (Sparkle isn't there otherwise). The switch
    /// reads Sparkle's own preference, so it redraws this view itself.
    fn updates_row(&self, theme: Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let automatic = self.state.read(cx).updater.as_ref()?.automatic();
        let state = self.state.clone();
        let control = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(
                button("check-updates", "Check Now", false, theme)
                    .on_click(move |_, _, cx| state.update(cx, |s, cx| s.check_for_updates(cx))),
            )
            .child(toggle(
                "auto-update",
                automatic,
                theme,
                Box::new(cx.listener(move |this, _, _, cx| {
                    if let Some(updater) = &this.state.read(cx).updater {
                        updater.set_automatic(!automatic);
                    }
                    cx.notify();
                })),
            ));
        Some(
            row(
                "Install updates automatically",
                Some("Checks once a day and installs when you quit Annuncia".into()),
                control,
                theme,
            )
            .into_any_element(),
        )
    }

    /// Stable or Nightly, when there's an updater to switch.
    fn channel_row(&self, theme: Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.state.read(cx);
        state.updater.as_ref()?;
        let nightly = state.update_channel() == UpdateChannel::Nightly;
        Some(
            row(
                "Update channel",
                Some("Nightly is built from main every night it changes".into()),
                self.choice(
                    "channel",
                    &["Stable", "Nightly"],
                    usize::from(nightly),
                    theme,
                    cx,
                    |s, i| {
                        s.update_channel = Some([UpdateChannel::Stable, UpdateChannel::Nightly][i])
                    },
                ),
                theme,
            )
            .into_any_element(),
        )
    }

    /// Transcribe: which local model (or off), with the model's download state underneath.
    fn transcription_row(
        &self,
        model: Transcription,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = self.state.read(cx);
        let download = state
            .download
            .as_ref()
            .filter(|d| d.model == model)
            .map(|d| d.fraction());
        let error = state.download_error.clone();
        let size = model
            .artifact()
            .map(|a| format!("{} MB", a.bytes / 1_000_000))
            .unwrap_or_default();

        let detail: SharedString = match (model, download, &error) {
            (Transcription::Off, ..) => "Turn on to get a transcript of every recording".into(),
            (_, Some(fraction), _) => format!(
                "Downloading {}, {size}: {}%",
                model.model_name(),
                (fraction * 100.) as u32
            )
            .into(),
            (_, None, Some(error)) => error.clone(),
            _ if model.is_installed() => {
                format!("Runs on this Mac with {}, {size}", model.model_name()).into()
            }
            _ => format!("Needs {}, a {size} download", model.model_name()).into(),
        };

        let handle = self.state.clone();
        let picker = div().flex_none().child(segmented(
            "transcription",
            &["Off", "English", "Other languages"],
            match model {
                Transcription::Off => 0,
                Transcription::English => 1,
                Transcription::Multilingual => 2,
            },
            theme,
            move |i, _, cx| {
                let model = [
                    Transcription::Off,
                    Transcription::English,
                    Transcription::Multilingual,
                ][i];
                handle.update(cx, |s, cx| s.set_transcription(model, cx));
            },
        ));

        let handle = self.state.clone();
        let action = match (model, download, &error) {
            (Transcription::Off, ..) => None,
            (_, Some(_), _) => Some(button("model-cancel", "Cancel", false, theme).on_click(
                move |_, _, cx| {
                    handle.update(cx, |s, cx| {
                        s.cancel_download();
                        s.update_settings(cx, |s| s.transcription = Transcription::Off);
                    })
                },
            )),
            _ if model.is_installed() => Some(
                button("model-remove", "Remove", false, theme)
                    .on_click(move |_, _, cx| handle.update(cx, |s, cx| s.remove_model(cx))),
            ),
            _ => Some(
                button("model-download", "Download", false, theme)
                    .on_click(move |_, _, cx| handle.update(cx, |s, cx| s.retry_download(cx))),
            ),
        };

        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(14.))
            .py(px(11.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(16.))
                    .child(div().text_size(px(13.)).child("Transcribe"))
                    .child(picker),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(if error.is_some() && download.is_none() {
                                        theme.record
                                    } else {
                                        theme.text_muted
                                    })
                                    .child(detail),
                            )
                            .when_some(download, |d, fraction| {
                                d.child(
                                    div()
                                        .h(px(4.))
                                        .w_full()
                                        .rounded(px(2.))
                                        .bg(theme.control_active)
                                        .child(
                                            div()
                                                .h_full()
                                                .w(gpui::relative(fraction.clamp(0., 1.)))
                                                .rounded(px(2.))
                                                .bg(theme.accent),
                                        ),
                                )
                            }),
                    )
                    .children(action),
            )
            .into_any_element()
    }

    /// Clean up: what the editor trims on its own when it opens.
    fn cleanup_row(
        &self,
        cleanup: Cleanup,
        transcription: Transcription,
        theme: Theme,
    ) -> AnyElement {
        let detail: String = match cleanup {
            Cleanup::Off => "Trim dead air and long pauses as soon as the editor opens".into(),
            Cleanup::PausesAndFillers if transcription != Transcription::English => {
                "Filler words need Transcribe set to English; pauses are still trimmed".into()
            }
            Cleanup::Pauses => {
                "Shortens pauses over a second and trims dead air. Restore any cut on the timeline"
                    .into()
            }
            Cleanup::PausesAndFillers => {
                "Also cuts um, uh and similar fillers. Restore any cut on the timeline".into()
            }
        };
        let handle = self.state.clone();
        let picker = div().flex_none().child(segmented(
            "cleanup",
            &["Off", "Pauses", "Pauses and filler words"],
            match cleanup {
                Cleanup::Off => 0,
                Cleanup::Pauses => 1,
                Cleanup::PausesAndFillers => 2,
            },
            theme,
            move |i, _, cx| {
                let cleanup = [Cleanup::Off, Cleanup::Pauses, Cleanup::PausesAndFillers][i];
                handle.update(cx, |s, cx| s.update_settings(cx, |s| s.cleanup = cleanup));
            },
        ));
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .px(px(14.))
            .py(px(11.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(16.))
                    .child(div().text_size(px(13.)).child("Clean up"))
                    .child(picker),
            )
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(theme.text_muted)
                    .child(detail),
            )
            .into_any_element()
    }

    fn choice(
        &self,
        id: &str,
        labels: &[&'static str],
        selected: usize,
        theme: Theme,
        _cx: &mut Context<Self>,
        set: impl Fn(&mut Settings, usize) + 'static,
    ) -> AnyElement {
        let state = self.state.clone();
        let bubble = id == "bubble";
        div()
            .w(px(64. * labels.len() as f32))
            .child(segmented(id, labels, selected, theme, move |i, _, cx| {
                state.update(cx, |s, cx| {
                    s.update_settings(cx, |s| set(s, i));
                    if bubble {
                        s.resize_bubble(cx);
                    }
                })
            }))
            .into_any_element()
    }

    fn switch(
        &self,
        id: &'static str,
        on: bool,
        theme: Theme,
        _cx: &mut Context<Self>,
        flip: impl Fn(&mut Settings) + 'static,
    ) -> AnyElement {
        let state = self.state.clone();
        toggle(
            id,
            on,
            theme,
            Box::new(move |_, _, cx| state.update(cx, |s, cx| s.update_settings(cx, &flip))),
        )
        .into_any_element()
    }
}

fn section(theme: Theme) -> Card {
    Card {
        theme,
        rows: Vec::new(),
    }
}

/// A card of rows with inset rules between them. GPUI clips children to rectangles, not rounded
/// corners, so the card paints the surface itself and its rows stay transparent.
#[derive(IntoElement)]
struct Card {
    theme: Theme,
    rows: Vec<AnyElement>,
}

impl Card {
    fn child(mut self, row: impl IntoElement) -> Self {
        self.rows.push(row.into_any_element());
        self
    }

    fn children(mut self, rows: impl IntoIterator<Item = impl IntoElement>) -> Self {
        self.rows
            .extend(rows.into_iter().map(IntoElement::into_any_element));
        self
    }
}

impl RenderOnce for Card {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let theme = self.theme;
        div()
            .flex()
            .flex_col()
            .rounded(px(12.))
            .border_1()
            .border_color(theme.hairline)
            .bg(theme.surface)
            .children(self.rows.into_iter().enumerate().flat_map(move |(i, row)| {
                let rule = (i > 0).then(|| {
                    div()
                        .h(px(1.))
                        .mx(px(14.))
                        .bg(theme.hairline)
                        .into_any_element()
                });
                rule.into_iter().chain(std::iter::once(row))
            }))
    }
}

fn row(
    label: &'static str,
    detail: Option<String>,
    control: impl IntoElement,
    theme: Theme,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(16.))
        .px(px(14.))
        .py(px(11.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(div().text_size(px(13.)).child(label))
                .when_some(detail, |d, detail| {
                    d.child(
                        div()
                            .text_size(px(11.5))
                            .text_color(theme.text_muted)
                            .child(detail),
                    )
                }),
        )
        .child(control)
}
