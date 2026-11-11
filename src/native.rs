//! Thin AppKit helpers for the things GPUI doesn't expose: activation policy,
//! overlay window levels, the notch geometry, native glass backdrops and the pasteboard.

use std::path::Path;

use block2::RcBlock;
use gpui::{App, Window};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, ProtocolObject};
use objc2::{MainThreadMarker, Message as _};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy,
    NSAutoresizingMaskOptions, NSEvent, NSGlassEffectView, NSModalResponse, NSPasteboard,
    NSPasteboardWriting, NSScreen, NSSearchField, NSStatusItem, NSTextField, NSView,
    NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
    NSWindow, NSWindowCollectionBehavior, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSFileManager, NSPoint, NSRect, NSSize, NSString, NSURL};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// Rectangle in global, top-left-origin points (the coordinate space ScreenCaptureKit uses).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TopLeftRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct MenuBarGeometry {
    pub screen: TopLeftRect,
    pub menu_bar_height: f64,
    /// The camera housing, when the main screen has one.
    pub notch: Option<TopLeftRect>,
}

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("AppKit helpers must run on the main thread")
}

/// Menu-bar app: no Dock icon, no app menu.
pub fn set_accessory_activation_policy() {
    let app = NSApplication::sharedApplication(mtm());
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
}

/// While the library is open Annuncia is a regular app, in the Dock and in ⌘-Tab.
pub fn set_regular_activation_policy() {
    let app = NSApplication::sharedApplication(mtm());
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
}

/// Annuncia is dark everywhere: glass, scroll bars, window buttons and file dialogs follow suit
/// even when the system is in light mode.
pub fn use_dark_appearance() {
    // SAFETY: reading an AppKit constant.
    let name = unsafe { objc2_app_kit::NSAppearanceNameDarkAqua };
    let appearance = objc2_app_kit::NSAppearance::appearanceNamed(name);
    NSApplication::sharedApplication(mtm()).setAppearance(appearance.as_deref());
}

fn primary_screen_height() -> f64 {
    let screens = NSScreen::screens(mtm());
    screens
        .firstObject()
        .map(|s| s.frame().size.height)
        .unwrap_or(0.)
}

fn to_cocoa(rect: TopLeftRect) -> NSRect {
    let y = primary_screen_height() - (rect.y + rect.h);
    NSRect::new(NSPoint::new(rect.x, y), NSSize::new(rect.w, rect.h))
}

fn from_cocoa(rect: NSRect) -> TopLeftRect {
    TopLeftRect {
        x: rect.origin.x,
        y: primary_screen_height() - (rect.origin.y + rect.size.height),
        w: rect.size.width,
        h: rect.size.height,
    }
}

/// Whether the pointer is inside `rect`. Our overlays never become key, so GPUI doesn't
/// always hear about the pointer leaving them; ask the window server instead.
pub fn pointer_in(rect: TopLeftRect) -> bool {
    let p = NSEvent::mouseLocation();
    let y = primary_screen_height() - p.y;
    p.x >= rect.x && p.x < rect.x + rect.w && y >= rect.y && y < rect.y + rect.h
}

pub fn menu_bar_geometry() -> Option<MenuBarGeometry> {
    let screen = NSScreen::mainScreen(mtm())?;
    let frame = screen.frame();
    let visible = screen.visibleFrame();
    let menu_bar_height =
        (frame.origin.y + frame.size.height) - (visible.origin.y + visible.size.height);
    let inset_top = screen.safeAreaInsets().top;
    let screen_rect = from_cocoa(frame);
    // Use only the side areas' widths, so it doesn't matter which space their origins are in.
    let notch = if inset_top > 0. {
        let left = screen.auxiliaryTopLeftArea().size.width;
        let right = screen.auxiliaryTopRightArea().size.width;
        let w = frame.size.width - left - right;
        (w > 0.).then_some(TopLeftRect {
            x: screen_rect.x + left,
            y: screen_rect.y,
            w,
            h: inset_top,
        })
    } else {
        None
    };
    Some(MenuBarGeometry {
        screen: screen_rect,
        menu_bar_height: menu_bar_height.max(24.),
        notch,
    })
}

fn ns_view(window: &Window) -> Option<Retained<NSView>> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return None;
    };
    // SAFETY: GPUI hands out its live NSView; we only retain it.
    unsafe { Retained::retain(appkit.ns_view.as_ptr().cast::<NSView>()) }
}

fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
    ns_view(window)?.window()
}

/// The CGWindowID ScreenCaptureKit uses to identify this window.
pub fn window_number(window: &Window) -> Option<u32> {
    ns_window(window).map(|w| w.windowNumber() as u32)
}

#[derive(Clone, Copy)]
pub enum OverlayLevel {
    /// Above the menu bar, for the notch HUD.
    AboveMenuBar,
    /// Floating above normal windows (camera bubble, launcher).
    Floating,
}

#[derive(Clone, Copy, Default)]
pub struct WindowSetup {
    pub overlay: Option<OverlayLevel>,
    pub shadow: bool,
    pub frame: Option<TopLeftRect>,
    /// Clip the window (including GPUI's Metal layer) to this corner radius.
    pub corner_radius: Option<f64>,
    /// Native glass behind GPUI's content, with this corner radius.
    pub glass: Option<f64>,
    pub show: bool,
    /// Also make it the key window so it receives keyboard input.
    pub key: bool,
}

/// AppKit calls back into GPUI on frame/visibility changes, so like GPUI's own `resize`
/// these run as a separate main-thread task rather than inside a window update.
fn on_main(cx: &App, f: impl FnOnce() + 'static) {
    cx.foreground_executor().spawn(async move { f() }).detach();
}

pub fn setup_window(window: &Window, cx: &App, setup: WindowSetup) {
    let Some(view) = ns_view(window) else { return };
    on_main(cx, move || {
        let Some(ns) = view.window() else { return };
        if let Some(level) = setup.overlay {
            ns.setStyleMask(NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel);
            ns.setLevel(match level {
                // NSMainMenuWindowLevel (24) + 2: above the menu bar.
                OverlayLevel::AboveMenuBar => 26,
                // NSPopUpMenuWindowLevel.
                OverlayLevel::Floating => 101,
            });
            // GPUI's PopUp windows are NSPanels, which hide whenever the app deactivates; the
            // HUD and bubble must stay up while the user works in the app being recorded.
            ns.setHidesOnDeactivate(false);
            if !setup.key {
                // Clicking the HUD or dragging the bubble shouldn't take focus from that app.
                let _: () = unsafe { objc2::msg_send![&*ns, setBecomesKeyOnlyIfNeeded: true] };
            }
            ns.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::Stationary
                    | NSWindowCollectionBehavior::FullScreenAuxiliary
                    | NSWindowCollectionBehavior::IgnoresCycle,
            );
        }
        ns.setHasShadow(setup.shadow);
        if let Some(radius) = setup.corner_radius {
            clip_corners(&view, radius);
        }
        if let Some(radius) = setup.glass {
            install_glass_backdrop(&view, radius);
        }
        if let Some(rect) = setup.frame {
            ns.setFrame_display(to_cocoa(rect), true);
        }
        if setup.key {
            ns.makeKeyAndOrderFront(None);
        } else if setup.show {
            ns.orderFrontRegardless();
        }
        ns.invalidateShadow();
    });
}

pub fn set_frame(window: &Window, cx: &App, rect: TopLeftRect) {
    let Some(ns) = ns_window(window) else { return };
    on_main(cx, move || {
        ns.setFrame_display(to_cocoa(rect), true);
        ns.invalidateShadow();
    });
}

pub fn set_visible(window: &Window, cx: &App, visible: bool) {
    let Some(ns) = ns_window(window) else { return };
    on_main(cx, move || {
        if visible {
            ns.orderFrontRegardless();
        } else {
            ns.orderOut(None);
        }
    });
}

pub fn frame(window: &Window) -> Option<TopLeftRect> {
    ns_window(window).map(|w| from_cocoa(w.frame()))
}

fn clip_corners(view: &NSView, radius: f64) {
    // SAFETY: plain property read on the main thread.
    let parent = unsafe { view.superview() };
    for v in [Some(view.retain()), parent].into_iter().flatten() {
        v.setWantsLayer(true);
        if let Some(layer) = v.layer() {
            layer.setCornerRadius(radius);
            layer.setMasksToBounds(true);
        }
    }
}

/// Liquid Glass (macOS 26+), or HUD vibrancy on older systems.
fn install_glass_backdrop(view: &NSView, corner_radius: f64) {
    // SAFETY: plain property read on the main thread.
    let Some(container) = (unsafe { view.superview() }) else {
        return;
    };
    let mtm = mtm();
    let bounds = container.bounds();
    let mask =
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable;

    let backdrop: Retained<NSView> = if AnyClass::get(c"NSGlassEffectView").is_some() {
        let glass = NSGlassEffectView::initWithFrame(mtm.alloc(), bounds);
        glass.setCornerRadius(corner_radius);
        Retained::into_super(glass)
    } else {
        let effect = NSVisualEffectView::initWithFrame(mtm.alloc(), bounds);
        effect.setMaterial(NSVisualEffectMaterial::HUDWindow);
        effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        effect.setState(NSVisualEffectState::Active);
        effect.setWantsLayer(true);
        if let Some(layer) = effect.layer() {
            layer.setCornerRadius(corner_radius);
            layer.setMasksToBounds(true);
        }
        Retained::into_super(effect)
    };
    backdrop.setAutoresizingMask(mask);
    container.addSubview_positioned_relativeTo(&backdrop, NSWindowOrderingMode::Below, Some(view));
}

/// A layer under GPUI's (transparent) view that shows the pop-out bubble's composited frames,
/// so the person can rise over transparent space. GPUI keeps drawing the controls on top.
pub fn install_video_layer(window: &Window) -> Option<Retained<objc2_quartz_core::CALayer>> {
    let view = ns_view(window)?;
    // SAFETY: plain property read on the main thread.
    let container = unsafe { view.superview() }?;
    let host = NSView::initWithFrame(mtm().alloc(), container.bounds());
    host.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    host.setWantsLayer(true);
    let layer = objc2_quartz_core::CALayer::new();
    // SAFETY: reading a Core Animation constant.
    layer.setContentsGravity(unsafe { objc2_quartz_core::kCAGravityResize });
    layer.setContentsScale(crate::popout::SCALE);
    host.setLayer(Some(&layer));
    container.addSubview_positioned_relativeTo(&host, NSWindowOrderingMode::Below, Some(&view));
    Some(layer)
}

/// Shows a composited frame on the video layer, without Core Animation's fade.
pub fn show_frame(layer: &objc2_quartz_core::CALayer, frame: &objc2_core_video::CVPixelBuffer) {
    let Some(surface) = objc2_core_video::CVPixelBufferGetIOSurface(Some(frame)) else {
        return;
    };
    // SAFETY: an IOSurfaceRef is toll-free bridged to the IOSurface object a layer accepts as
    // contents; the layer retains it.
    let contents = unsafe {
        &*(objc2_core_foundation::CFRetained::as_ptr(&surface).as_ptr() as *const AnyObject)
    };
    objc2_quartz_core::CATransaction::begin();
    objc2_quartz_core::CATransaction::setDisableActions(true);
    // SAFETY: setting contents on the main thread.
    unsafe { layer.setContents(Some(contents)) };
    objc2_quartz_core::CATransaction::commit();
}

/// The status item's frame, used to drop the launcher panel right under the icon.
pub fn status_item_frame(item: &NSStatusItem) -> Option<TopLeftRect> {
    let button = item.button(mtm())?;
    let window = button.window()?;
    Some(from_cocoa(window.frame()))
}

pub fn copy_text_to_pasteboard(text: &str) {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    // SAFETY: reading an AppKit constant.
    let kind = unsafe { objc2_app_kit::NSPasteboardTypeString };
    pasteboard.setString_forType(&NSString::from_str(text), kind);
}

pub fn copy_file_to_pasteboard(path: &Path) {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    let item: Retained<ProtocolObject<dyn NSPasteboardWriting>> =
        ProtocolObject::from_retained(url);
    pasteboard.writeObjects(&NSArray::from_retained_slice(&[item]));
}

pub fn reveal_in_finder(path: &Path) {
    let _ = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();
}

pub fn open(target: impl AsRef<std::ffi::OsStr>) {
    let _ = std::process::Command::new("open").arg(target).spawn();
}

/// The `.app` Annuncia runs from; `None` for the bare binary from `cargo run`.
pub fn bundle_path() -> Option<std::path::PathBuf> {
    let path = objc2_foundation::NSBundle::mainBundle()
        .bundlePath()
        .to_string();
    path.ends_with(".app").then(|| path.into())
}

/// Opens Annuncia again once this process has exited; the caller then quits. Screen Recording
/// access only takes effect at launch.
pub fn relaunch_after_quit() {
    let Some(target) = bundle_path().or_else(|| std::env::current_exe().ok()) else {
        return;
    };
    // `open` doesn't pass on the environment, so a custom settings folder is handed over.
    let script = r#"while kill -0 "$1" 2>/dev/null; do sleep 0.1; done
if [ ! -d "$2" ]; then "$2" &
elif [ -n "$XDG_CONFIG_HOME" ]; then open --env "XDG_CONFIG_HOME=$XDG_CONFIG_HOME" "$2"
else open "$2"; fi"#;
    let _ = std::process::Command::new("/bin/sh")
        .args(["-c", script, "sh", &std::process::id().to_string()])
        .arg(target)
        .spawn();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginItem {
    Off,
    On,
    /// Registered, but switched off in System Settings › Login Items.
    NeedsApproval,
}

/// Whether Annuncia opens at login; `None` outside the app bundle, where it can't.
pub fn login_item() -> Option<LoginItem> {
    use objc2_service_management::{SMAppService, SMAppServiceStatus};
    bundle_path()?;
    let status = unsafe { SMAppService::mainAppService().status() };
    Some(match status {
        SMAppServiceStatus::Enabled => LoginItem::On,
        SMAppServiceStatus::RequiresApproval => LoginItem::NeedsApproval,
        _ => LoginItem::Off,
    })
}

pub fn set_login_item(on: bool) -> anyhow::Result<()> {
    use objc2_service_management::SMAppService;
    let service = unsafe { SMAppService::mainAppService() };
    let result = unsafe {
        if on {
            service.registerAndReturnError()
        } else {
            service.unregisterAndReturnError()
        }
    };
    result.map_err(|err| anyhow::anyhow!("{}", err.localizedDescription()))
}

pub fn open_login_items_settings() {
    unsafe { objc2_service_management::SMAppService::openSystemSettingsLoginItems() };
}

/// Moves a file to the Trash rather than deleting it outright; returns whether it did.
pub fn trash(path: &Path) -> bool {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .is_ok()
}

/// Hands the current mouse-down to the window server so a borderless window can be dragged.
/// GPUI's `start_window_move` is a no-op on macOS.
pub fn begin_window_drag(window: &Window) {
    let Some(ns) = ns_window(window) else { return };
    if let Some(event) = NSApplication::sharedApplication(mtm()).currentEvent() {
        ns.performWindowDragWithEvent(&event);
    }
}

pub fn app_is_active() -> bool {
    NSApplication::sharedApplication(mtm()).isActive()
}

/// True while one of our windows is key (e.g. the bubble was clicked, not another app).
pub fn app_has_key_window() -> bool {
    NSApplication::sharedApplication(mtm())
        .keyWindow()
        .is_some()
}

/// Asks for a new name in a sheet on `window`, starting from `current`. Sends the trimmed name,
/// or `None` if cancelled.
pub fn prompt_for_name(
    window: &Window,
    message: &str,
    action: &str,
    current: &str,
) -> smol::channel::Receiver<Option<String>> {
    let (tx, rx) = smol::channel::bounded(1);
    let Some(ns) = ns_window(window) else {
        let _ = tx.try_send(None);
        return rx;
    };
    let alert = NSAlert::new(mtm());
    alert.setMessageText(&NSString::from_str(message));
    alert.addButtonWithTitle(&NSString::from_str(action));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    let field = NSTextField::textFieldWithString(&NSString::from_str(current), mtm());
    field.setFrame(NSRect::new(NSPoint::new(0., 0.), NSSize::new(280., 24.)));
    alert.setAccessoryView(Some(&field));
    alert.window().setInitialFirstResponder(Some(&field));
    let handler = RcBlock::new(move |response: NSModalResponse| {
        let name = (response == NSAlertFirstButtonReturn)
            .then(|| field.stringValue().to_string().trim().to_string());
        let _ = tx.try_send(name);
    });
    alert.beginSheetModalForWindow_completionHandler(&ns, Some(&handler));
    rx
}

/// A native search field pinned to the top-right of `window`, `top` and `right` points in from
/// those edges. GPUI has no text input; this one brings editing, IME and the clear button.
pub fn install_search_field(
    window: &Window,
    placeholder: &str,
    width: f64,
    top: f64,
    right: f64,
) -> Option<Retained<NSSearchField>> {
    let view = ns_view(window)?;
    // SAFETY: plain property read on the main thread.
    let container = unsafe { view.superview() }?;
    let bounds = container.bounds();
    let height = 24.;
    let flipped = container.isFlipped();
    let y = if flipped {
        top
    } else {
        bounds.size.height - top - height
    };
    let field = NSSearchField::initWithFrame(
        mtm().alloc(),
        NSRect::new(
            NSPoint::new(bounds.size.width - right - width, y),
            NSSize::new(width, height),
        ),
    );
    field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
    // Stay pinned to the top-right as the window resizes.
    field.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewMinXMargin
            | if flipped {
                NSAutoresizingMaskOptions::ViewMaxYMargin
            } else {
                NSAutoresizingMaskOptions::ViewMinYMargin
            },
    );
    container.addSubview_positioned_relativeTo(&field, NSWindowOrderingMode::Above, Some(&view));
    Some(field)
}

/// Moves keyboard focus to `field`, or back to GPUI's view.
pub fn focus_search_field(window: &Window, field: Option<&NSSearchField>) {
    let (Some(ns), Some(view)) = (ns_window(window), ns_view(window)) else {
        return;
    };
    match field {
        Some(field) => ns.makeFirstResponder(Some(field)),
        None => ns.makeFirstResponder(Some(&view)),
    };
}

/// Selects `paths` in one Finder window (`open -R` takes only one).
pub fn reveal_all_in_finder(paths: &[std::path::PathBuf]) {
    let urls: Vec<Retained<NSURL>> = paths
        .iter()
        .map(|p| NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy())))
        .collect();
    objc2_app_kit::NSWorkspace::sharedWorkspace()
        .activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&urls));
}
