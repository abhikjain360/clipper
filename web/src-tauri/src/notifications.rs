use std::{
    future::Future,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use block2::{DynBlock, RcBlock};
use objc2::{
    ClassType, define_class, msg_send,
    rc::{Retained, autoreleasepool},
    runtime::{Bool, ProtocolObject},
};
use objc2_foundation::{NSArray, NSBundle, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationSettings,
    UNNotificationSound, UNTimeIntervalNotificationTrigger, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

const REST_END_IDENTIFIER: &str = "clipper-gym-rest-end";
static ENABLED: AtomicBool = AtomicBool::new(false);
static REQUESTED: AtomicBool = AtomicBool::new(false);

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
    ENABLED.store(true, Ordering::Relaxed);
    Some(delegate)
}

pub async fn request_permission() -> bool {
    check_permission(&REQUESTED, authorization_status(), ask_permission).await
}

pub async fn allowed() -> bool {
    authorization_status().await.is_some_and(is_allowed)
}

fn is_allowed(status: UNAuthorizationStatus) -> bool {
    matches!(
        status,
        UNAuthorizationStatus::Authorized
            | UNAuthorizationStatus::Provisional
            | UNAuthorizationStatus::Ephemeral
    )
}

async fn check_permission<F, R>(
    requested: &AtomicBool,
    status: impl Future<Output = Option<UNAuthorizationStatus>>,
    ask: F,
) -> bool
where
    F: FnOnce() -> R,
    R: Future<Output = bool>,
{
    match status.await {
        Some(status) if is_allowed(status) => true,
        Some(UNAuthorizationStatus::NotDetermined) if !requested.swap(true, Ordering::Relaxed) => {
            ask().await
        }
        _ => false,
    }
}

async fn authorization_status() -> Option<UNAuthorizationStatus> {
    if !ENABLED.load(Ordering::Relaxed) {
        return None;
    }
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    autoreleasepool(|_| {
        let completion = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            let status = unsafe { settings.as_ref() }.authorizationStatus();
            let _ = sender.send(status);
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .getNotificationSettingsWithCompletionHandler(&completion);
    });
    response(receiver).await
}

async fn ask_permission() -> bool {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
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
    response(receiver).await.unwrap_or(false)
}

async fn response<T>(mut receiver: tokio::sync::mpsc::UnboundedReceiver<T>) -> Option<T> {
    tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .ok()
        .flatten()
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

#[cfg(test)]
mod tests {
    use std::{cell::Cell, future::ready};

    use super::*;

    #[tokio::test]
    async fn permission_changes_resume_delivery_without_another_prompt() {
        let requested = AtomicBool::new(false);
        let prompts = Cell::new(0);
        for (status, allowed) in [
            (None, false),
            (Some(UNAuthorizationStatus::NotDetermined), false),
            (Some(UNAuthorizationStatus::NotDetermined), false),
            (Some(UNAuthorizationStatus::Denied), false),
            (None, false),
            (Some(UNAuthorizationStatus::Authorized), true),
            (Some(UNAuthorizationStatus::Denied), false),
            (Some(UNAuthorizationStatus::Authorized), true),
        ] {
            assert_eq!(
                check_permission(&requested, ready(status), || {
                    prompts.set(prompts.get() + 1);
                    ready(false)
                })
                .await,
                allowed
            );
        }
        assert_eq!(prompts.get(), 1);
    }

    #[tokio::test]
    async fn previously_denied_permission_does_not_prompt() {
        let requested = AtomicBool::new(false);
        for status in [
            UNAuthorizationStatus::Denied,
            UNAuthorizationStatus::Authorized,
        ] {
            assert_eq!(
                check_permission(&requested, ready(Some(status)), || async {
                    panic!("unexpected prompt")
                })
                .await,
                is_allowed(status)
            );
        }
    }

    #[tokio::test]
    async fn an_unanswered_prompt_times_out_and_later_authorization_resumes_delivery() {
        let requested = AtomicBool::new(false);
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        assert!(
            !check_permission(
                &requested,
                ready(Some(UNAuthorizationStatus::NotDetermined)),
                || async { response(receiver).await.unwrap_or(false) }
            )
            .await
        );
        assert!(sender.send(true).is_err());
        assert!(
            check_permission(
                &requested,
                ready(Some(UNAuthorizationStatus::Authorized)),
                || async { panic!("unexpected prompt") }
            )
            .await
        );
    }
}
