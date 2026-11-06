//! Updates through Sparkle, which `bundle.sh` embeds in `Contents/Frameworks`. It's loaded at
//! runtime rather than linked, so the bare binary from `cargo run` still starts (without updates).
//! The public signing key and the defaults (check daily, download and install on quit) are in the
//! bundle's Info.plist; Sparkle keeps the user's choice in its own preferences. The feed depends
//! on the update channel, which a delegate hands Sparkle each time it checks. The delegate also
//! takes over an update Sparkle has downloaded, so the app can offer to relaunch into it.

use std::cell::{Cell, RefCell};

use block2::{Block, RcBlock};
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

/// An update Sparkle has downloaded, which installs when Annuncia quits.
#[derive(Clone)]
pub struct ReadyUpdate {
    pub version: String,
    install: RcBlock<dyn Fn()>,
}

impl ReadyUpdate {
    /// Installs the update and relaunches. Sparkle quits the app from inside this.
    pub fn install(&self) {
        self.install.call(());
    }
}

struct DelegateIvars {
    channel: Cell<UpdateChannel>,
    ready: RefCell<Option<ReadyUpdate>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "AnnunciaUpdaterDelegate"]
    #[ivars = DelegateIvars]
    struct Delegate;

    impl Delegate {
        #[unsafe(method_id(feedURLStringForUpdater:))]
        fn feed_url(&self, _updater: &AnyObject) -> Option<Retained<NSString>> {
            Some(NSString::from_str(match self.ivars().channel.get() {
                UpdateChannel::Stable => STABLE_FEED,
                UpdateChannel::Nightly => NIGHTLY_FEED,
            }))
        }

        /// A downloaded update. Taking it (YES) keeps Sparkle from nagging about it later; it
        /// still installs on quit either way.
        #[unsafe(method(updater:willInstallUpdateOnQuit:immediateInstallationBlock:))]
        fn will_install_on_quit(
            &self,
            _updater: &AnyObject,
            item: &AnyObject,
            install: &Block<dyn Fn()>,
        ) -> bool {
            let version: Option<Retained<NSString>> =
                unsafe { msg_send![item, displayVersionString] };
            *self.ivars().ready.borrow_mut() = Some(ReadyUpdate {
                version: version.map(|v| v.to_string()).unwrap_or_default(),
                install: install.copy(),
            });
            true
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
            let this = Delegate::alloc().set_ivars(DelegateIvars {
                channel: Cell::new(channel),
                ready: RefCell::new(None),
            });
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
        self.delegate.ivars().channel.set(channel);
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

    /// The update waiting to install, once Sparkle has downloaded one.
    pub fn ready(&self) -> Option<ReadyUpdate> {
        self.delegate.ivars().ready.borrow().clone()
    }

    fn updater(&self) -> Retained<AnyObject> {
        unsafe { msg_send![&self.controller, updater] }
    }
}
