//! A pane's notification, posted to macOS as Kil through `UNUserNotificationCenter`, and the
//! click on it, which names the pane to go to.
//!
//! `osascript`'s `display notification` can't do this: it posts under Script Editor, so a
//! click opened Script Editor. A notification has to come from Kil's own process to belong
//! to Kil. The pane is named in the request's identifier, which comes back with the click.
#![expect(
    unsafe_code,
    reason = "Objective-C: a delegate class and completion blocks; every block carries a SAFETY comment"
)]

use std::sync::atomic::{AtomicU64, Ordering};

use block2::{DynBlock, RcBlock};
use futures::channel::mpsc;
use objc2::{
    AnyThread, DefinedClass, define_class, msg_send,
    rc::Retained,
    runtime::{Bool, ProtocolObject},
};
use objc2_foundation::{NSBundle, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
    UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

/// Starts the identifier of every notification Kil posts: `kil-pane.<pane id>.<number>`.
/// Pane ids hold only letters, digits and `-` (`restore::is_valid_id`), so the dots are
/// unambiguous.
const IDENTIFIER_PREFIX: &str = "kil-pane.";

/// Makes every notification's identifier different, so one doesn't replace another.
static POSTED: AtomicU64 = AtomicU64::new(0);

/// The pane a notification identifier names. The id is the pane's own, which lasts across
/// launches, so a notification from before a restart still goes to the same pane.
fn pane_of(identifier: &str) -> Option<&str> {
    let (pane, _number) = identifier
        .strip_prefix(IDENTIFIER_PREFIX)?
        .rsplit_once('.')?;
    Some(pane)
}

/// The center aborts the process when it has no bundle (an unbundled `cargo run`), and
/// there is nothing to be a notification of.
fn available() -> bool {
    !cfg!(test) && NSBundle::mainBundle().bundleIdentifier().is_some()
}

struct Clicks(mpsc::UnboundedSender<String>);

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements, and the class has no `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "KilNotificationDelegate"]
    #[ivars = Clicks]
    struct Delegate;

    // SAFETY: nothing beyond the protocol's own contract.
    unsafe impl NSObjectProtocol for Delegate {}

    // SAFETY: both methods have the protocol's signatures, and call their handler once.
    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        /// Without this, macOS shows nothing while Kil is in front, and a pane in another
        /// tab is exactly when Kil posts one.
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            done: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            done.call((
                UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List,
            ));
        }

        /// Any response is a click: Kil sets no actions, so only the default one exists. The
        /// system has brought Kil forward by now, or started it.
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            done: &DynBlock<dyn Fn()>,
        ) {
            let identifier = response.notification().request().identifier().to_string();
            if let Some(pane) = pane_of(&identifier) {
                // The app may be gone already: nobody left to tell.
                let _ = self.ivars().0.unbounded_send(pane.to_owned());
            }
            done.call(());
        }
    }
);

impl Delegate {
    fn new(clicks: mpsc::UnboundedSender<String>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Clicks(clicks));
        // SAFETY: `init` of `NSObject`, the superclass, on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

/// Makes Kil the center's delegate, so it hears the click on its notifications and they show
/// while Kil is in front. Gives the id of each pane whose notification is clicked. Call it
/// as the app starts: a click that starts Kil is delivered once the delegate is set.
pub fn init() -> Option<mpsc::UnboundedReceiver<String>> {
    if !available() {
        return None;
    }
    let (clicks, heard) = mpsc::unbounded();
    let delegate = Delegate::new(clicks);
    UNUserNotificationCenter::currentNotificationCenter()
        .setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // The center holds its delegate weakly, and Kil needs it until it quits.
    std::mem::forget(delegate);
    Some(heard)
}

/// Shows `body` under `title` for `pane` (the pane's id); clicking it names the pane to
/// [`init`]'s receiver. The first one asks macOS for permission, and none shows if it's
/// refused.
pub fn post(pane: &str, title: &str, body: &str) {
    if !available() {
        return;
    }
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    let identifier = format!(
        "{IDENTIFIER_PREFIX}{pane}.{}",
        POSTED.fetch_add(1, Ordering::Relaxed)
    );
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(&identifier),
        &content,
        None,
    );
    let add = RcBlock::new(move |granted: Bool, error: *mut NSError| {
        if !granted.as_bool() {
            // SAFETY: the center passes nil, or an `NSError` that lives through this call.
            match unsafe { error.as_ref() } {
                Some(error) => log::warn!(
                    "notifications aren't allowed: {}",
                    error.localizedDescription()
                ),
                None => log::debug!("notifications are off for Kil in System Settings"),
            }
            return;
        }
        let failed = RcBlock::new(|error: *mut NSError| {
            // SAFETY: the center passes nil, or an `NSError` that lives through this call.
            if let Some(error) = unsafe { error.as_ref() } {
                log::error!(
                    "a notification wasn't posted: {}",
                    error.localizedDescription()
                );
            }
        });
        // The center can be used from any thread, and the request is never changed.
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, Some(&failed));
    });
    // Once answered, this calls back at once, and without asking again.
    UNUserNotificationCenter::currentNotificationCenter()
        .requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert, &add);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_should_name_the_pane_whose_notification_it_was() {
        assert_eq!(pane_of("kil-pane.19f3a2b-4.17"), Some("19f3a2b-4"));
    }

    #[test]
    fn a_posted_identifier_should_name_its_pane_back() {
        let identifier = format!("{IDENTIFIER_PREFIX}{}.{}", "19f3a2b-4", 0);
        assert_eq!(pane_of(&identifier), Some("19f3a2b-4"));
    }

    #[test]
    fn a_notification_not_from_a_pane_should_name_none() {
        assert_eq!(pane_of("something-else.1"), None);
        assert_eq!(pane_of("kil-pane.no-number"), None);
    }
}
