use block2::{DynBlock, RcBlock};
use objc2::{
    ClassType, define_class, msg_send,
    rc::{Retained, autoreleasepool},
    runtime::{Bool, ProtocolObject},
};
use objc2_foundation::{NSArray, NSBundle, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationSound,
    UNTimeIntervalNotificationTrigger, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

const REST_END_IDENTIFIER: &str = "clipper-gym-rest-end";

define_class!(
    #[unsafe(super = NSObject)]
    pub struct NotificationDelegate;

    unsafe impl NSObjectProtocol for NotificationDelegate {}

    unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }
    }
);

pub fn initialize(identifier: &str) -> Option<Retained<NotificationDelegate>> {
    if NSBundle::mainBundle().bundleIdentifier()?.to_string() != identifier {
        return None;
    }
    let delegate: Retained<NotificationDelegate> =
        unsafe { msg_send![NotificationDelegate::class(), new] };
    UNUserNotificationCenter::currentNotificationCenter()
        .setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    Some(delegate)
}

pub async fn request_permission() -> bool {
    static PERMISSION: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
    *PERMISSION.get_or_init(ask_permission).await
}

async fn ask_permission() -> bool {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    autoreleasepool(|_| {
        let completion = RcBlock::new(move |granted: Bool, error: *mut NSError| {
            let _ = sender.send(granted.as_bool() && error.is_null());
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &completion,
            );
    });
    receiver.recv().await.unwrap_or(false)
}

pub fn show(message: &str) {
    post("clipper-break-reminder", message, false);
}

pub fn show_alarm(alarm: &clipper_app_types::AlarmView) {
    let identifier = format!(
        "clipper-alarm-{}-{}-{}",
        alarm.item_id, alarm.occurrence_key, alarm.fire_at_millis
    );
    post(&identifier, &alarm.label, true);
}

pub fn schedule_rest_end(after_seconds: f64, title: &str, body: &str) {
    autoreleasepool(|_| {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        content.setSound(Some(&UNNotificationSound::defaultSound()));
        let trigger = UNTimeIntervalNotificationTrigger::triggerWithTimeInterval_repeats(
            after_seconds,
            false,
        );
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(REST_END_IDENTIFIER),
            &content,
            Some(&trigger),
        );
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, None);
    });
}

pub fn cancel_rest_end() {
    autoreleasepool(|_| {
        UNUserNotificationCenter::currentNotificationCenter()
            .removePendingNotificationRequestsWithIdentifiers(&NSArray::from_retained_slice(&[
                NSString::from_str(REST_END_IDENTIFIER),
            ]));
    });
}

fn post(identifier: &str, message: &str, sound: bool) {
    autoreleasepool(|_| {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str("Clipper"));
        content.setBody(&NSString::from_str(message));
        if sound {
            content.setSound(Some(&UNNotificationSound::defaultSound()));
        }
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(identifier),
            &content,
            None,
        );
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, None);
    });
}
