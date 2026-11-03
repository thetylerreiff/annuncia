//! Updates through Sparkle, which `bundle.sh` embeds in `Contents/Frameworks`. It's loaded at
//! runtime rather than linked, so the bare binary from `cargo run` still starts (without updates).
//! The public signing key and the defaults (check daily, download and install on quit) are in the
//! bundle's Info.plist; Sparkle keeps the user's choice in its own preferences. The feed depends
//! on the update channel, which a delegate hands Sparkle each time it checks.

use std::cell::Cell;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::{AnyThread as _, DefinedClass as _, define_class, msg_send};
use objc2_foundation::{NSBundle, NSString};

use crate::settings::UpdateChannel;

const STABLE_FEED: &str = "https://annuncia.thetylerreiff.co/appcast.xml";
/// Kept with the nightly build on its rolling GitHub release, so nightlies never touch main.
const NIGHTLY_FEED: &str =
    "https://github.com/thetylerreiff/annuncia/releases/download/nightly/appcast.xml";

/// The channel this build was published on, from its Info.plist.
pub fn built_channel() -> UpdateChannel {
    let key = NSString::from_str("AnnunciaUpdateChannel");
    let value = NSBundle::mainBundle().objectForInfoDictionaryKey(&key);
    match value.and_then(|v| v.downcast::<NSString>().ok()) {
        Some(channel) if channel.to_string() == "nightly" => UpdateChannel::Nightly,
        _ => UpdateChannel::Stable,
    }
}

/// The version shown to people: the bundle's, which says which nightly it is.
pub fn display_version() -> String {
    let key = NSString::from_str("CFBundleShortVersionString");
    NSBundle::mainBundle()
        .objectForInfoDictionaryKey(&key)
        .and_then(|v| v.downcast::<NSString>().ok())
        .map(|v| v.to_string())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "AnnunciaUpdaterDelegate"]
    #[ivars = Cell<UpdateChannel>]
    struct Delegate;

    impl Delegate {
        #[unsafe(method_id(feedURLStringForUpdater:))]
        fn feed_url(&self, _updater: &AnyObject) -> Option<Retained<NSString>> {
            Some(NSString::from_str(match self.ivars().get() {
                UpdateChannel::Stable => STABLE_FEED,
                UpdateChannel::Nightly => NIGHTLY_FEED,
            }))
        }
    }
);

pub struct Updater {
    /// An `SPUStandardUpdaterController`, which owns the updater and its standard windows.
    controller: Retained<AnyObject>,
    /// Sparkle holds its delegate weakly.
    delegate: Retained<Delegate>,
}

impl Updater {
    /// Loads Sparkle from the app bundle and starts its update schedule.
    pub fn start(channel: UpdateChannel) -> Option<Self> {
        let frameworks = NSBundle::mainBundle().privateFrameworksPath()?;
        let path =
            frameworks.stringByAppendingPathComponent(&NSString::from_str("Sparkle.framework"));
        let sparkle = NSBundle::bundleWithPath(&path)?;
        // Loading runs the framework's initializers; Sparkle's are safe on the main thread.
        if !unsafe { sparkle.load() } {
            eprintln!("annuncia: couldn't load Sparkle; updates are off");
            return None;
        }
        let class = AnyClass::get(c"SPUStandardUpdaterController")?;
        let delegate: Retained<Delegate> = {
            let this = Delegate::alloc().set_ivars(Cell::new(channel));
            unsafe { msg_send![super(this), init] }
        };
        let none: Option<&AnyObject> = None;
        let controller: Retained<AnyObject> = unsafe {
            let allocated: Allocated<AnyObject> = msg_send![class, alloc];
            msg_send![allocated, initWithStartingUpdater: true, updaterDelegate: &*delegate, userDriverDelegate: none]
        };
        Some(Self {
            controller,
            delegate,
        })
    }

    /// Checks now, showing Sparkle's window with the result.
    pub fn check(&self) {
        let none: Option<&AnyObject> = None;
        let _: () = unsafe { msg_send![&self.controller, checkForUpdates: none] };
    }

    /// Switches feeds, and looks at the new one right away when updates are automatic. Sparkle
    /// never goes back a version, so leaving Nightly keeps the nightly until Stable passes it.
    pub fn set_channel(&self, channel: UpdateChannel) {
        self.delegate.ivars().set(channel);
        if self.automatic() {
            let updater = self.updater();
            let _: () = unsafe { msg_send![&updater, checkForUpdatesInBackground] };
        }
    }

    /// Whether updates are checked for, downloaded and installed in the background.
    pub fn automatic(&self) -> bool {
        let updater = self.updater();
        unsafe { msg_send![&updater, automaticallyDownloadsUpdates] }
    }

    /// Off means no checks at all until "Check for Updates…".
    pub fn set_automatic(&self, on: bool) {
        let updater = self.updater();
        unsafe {
            let _: () = msg_send![&updater, setAutomaticallyChecksForUpdates: on];
            let _: () = msg_send![&updater, setAutomaticallyDownloadsUpdates: on];
        }
    }

    fn updater(&self) -> Retained<AnyObject> {
        unsafe { msg_send![&self.controller, updater] }
    }
}
