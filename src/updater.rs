//! Updates through Sparkle, which `bundle.sh` embeds in `Contents/Frameworks`. It's loaded at
//! runtime rather than linked, so the bare binary from `cargo run` still starts (without updates).
//! The feed, the public signing key and the defaults (check daily, download and install on quit)
//! are in the bundle's Info.plist; Sparkle keeps the user's choice in its own preferences.

use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2_foundation::{NSBundle, NSString};

pub struct Updater {
    /// An `SPUStandardUpdaterController`, which owns the updater and its standard windows.
    controller: Retained<AnyObject>,
}

impl Updater {
    /// Loads Sparkle from the app bundle and starts its update schedule.
    pub fn start() -> Option<Self> {
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
        let none: Option<&AnyObject> = None;
        let controller: Retained<AnyObject> = unsafe {
            let allocated: Allocated<AnyObject> = msg_send![class, alloc];
            msg_send![allocated, initWithStartingUpdater: true, updaterDelegate: none, userDriverDelegate: none]
        };
        Some(Self { controller })
    }

    /// Checks now, showing Sparkle's window with the result.
    pub fn check(&self) {
        let none: Option<&AnyObject> = None;
        let _: () = unsafe { msg_send![&self.controller, checkForUpdates: none] };
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
