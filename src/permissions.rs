//! The privacy permissions Annuncia asks for: Screen Recording (required), and the microphone
//! and camera (each optional; without one, that part of a recording is off).

use objc2::runtime::Bool;
use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureDevice, AVMediaType, AVMediaTypeAudio, AVMediaTypeVideo,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Granted,
    Denied,
    /// Never asked, so asking shows the system prompt.
    Undetermined,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    Screen,
    Microphone,
    Camera,
}

impl Permission {
    pub const ALL: [Permission; 3] = [
        Permission::Screen,
        Permission::Microphone,
        Permission::Camera,
    ];

    /// macOS can't say whether Screen Recording was refused or never asked, so it reads
    /// `Undetermined` until it's granted. Once it is, macOS only reports it after a relaunch.
    pub fn access(self) -> Access {
        match self {
            Permission::Screen => {
                if objc2_core_graphics::CGPreflightScreenCaptureAccess() {
                    Access::Granted
                } else {
                    Access::Undetermined
                }
            }
            Permission::Microphone | Permission::Camera => {
                let Some(media) = self.media_type() else {
                    return Access::Denied;
                };
                match unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) } {
                    AVAuthorizationStatus::Authorized => Access::Granted,
                    AVAuthorizationStatus::NotDetermined => Access::Undetermined,
                    _ => Access::Denied,
                }
            }
        }
    }

    /// Shows the system prompt; `done` runs on an arbitrary thread. Screen Recording's prompt
    /// only points at System Settings, so for it `done` reports whether it was already granted.
    pub fn request(self, done: impl Fn(bool) + Send + Sync + 'static) {
        if self == Permission::Screen {
            done(objc2_core_graphics::CGRequestScreenCaptureAccess());
            return;
        }
        let Some(media) = self.media_type() else {
            done(false);
            return;
        };
        let block = block2::RcBlock::new(move |granted: Bool| done(granted.as_bool()));
        unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &block) };
    }

    /// With Screen Recording allowed, macOS separately asks once (and again now and then) before
    /// an app lists windows and screens itself rather than through the system picker, as the
    /// recorder does. Doing that early brings its prompt up during setup, not mid-recording.
    pub fn confirm_direct_capture() {
        std::thread::spawn(|| {
            let _ = screencapturekit::prelude::SCShareableContent::get();
        });
    }

    /// The pane of System Settings that lists apps with this permission.
    pub fn settings_url(self) -> &'static str {
        match self {
            Permission::Screen => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
            }
            Permission::Microphone => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
            }
            Permission::Camera => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Camera"
            }
        }
    }

    fn media_type(self) -> Option<&'static AVMediaType> {
        unsafe {
            match self {
                Permission::Screen => None,
                Permission::Microphone => AVMediaTypeAudio,
                Permission::Camera => AVMediaTypeVideo,
            }
        }
    }
}
