use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, Mutex},
};

use clipper_app_types::RunningWorkView;
use futures_util::future::{AbortHandle, AbortRegistration, Abortable};
use tokio::sync::watch;

use crate::api_client::ClientError;

tokio::task_local! {
    static CURRENT_WORK: (Arc<SessionWork>, u64);
}

struct Work {
    label: Option<String>,
    abort: AbortHandle,
}

#[derive(Default)]
struct Registry {
    stopped: bool,
    next_id: u64,
    work: BTreeMap<u64, Work>,
}

pub(crate) struct SessionWork {
    registry: Mutex<Registry>,
    changed: watch::Sender<()>,
}

struct WorkGuard {
    session: Arc<SessionWork>,
    id: u64,
}

impl Drop for WorkGuard {
    fn drop(&mut self) {
        self.session.registry.lock().unwrap().work.remove(&self.id);
        self.session.changed.send_replace(());
    }
}

impl SessionWork {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            registry: Mutex::new(Registry::default()),
            changed: watch::channel(()).0,
        })
    }

    pub(crate) fn is_current(self: &Arc<Self>) -> bool {
        CURRENT_WORK
            .try_with(|(session, _)| Arc::ptr_eq(session, self))
            .unwrap_or(false)
    }

    pub(crate) fn set_label(&self, expected: &str, label: String) {
        if let Some(id) = self.caller()
            && let Some(work) = self.registry.lock().unwrap().work.get_mut(&id)
            && work.label.as_deref() == Some(expected)
        {
            work.label = Some(label);
        }
    }

    pub(crate) async fn run<T>(
        self: &Arc<Self>,
        label: Option<String>,
        future: impl Future<Output = Result<T, ClientError>>,
    ) -> Result<T, ClientError> {
        if self.is_current() {
            return future.await;
        }
        let (guard, registration) = self.register(label)?;
        let result = CURRENT_WORK
            .scope(
                (self.clone(), guard.id),
                Abortable::new(future, registration),
            )
            .await
            .map_err(|_| ClientError::WorkCancelled)?;
        drop(guard);
        result
    }

    fn register(
        self: &Arc<Self>,
        label: Option<String>,
    ) -> Result<(WorkGuard, AbortRegistration), ClientError> {
        let mut registry = self.registry.lock().unwrap();
        if registry.stopped {
            return Err(ClientError::WorkCancelled);
        }
        let (abort, registration) = AbortHandle::new_pair();
        registry.next_id += 1;
        let id = registry.next_id;
        registry.work.insert(id, Work { label, abort });
        Ok((
            WorkGuard {
                session: self.clone(),
                id,
            },
            registration,
        ))
    }

    #[cfg(not(target_family = "wasm"))]
    pub(crate) async fn blocking<T: Send + 'static>(
        self: &Arc<Self>,
        operation: impl FnOnce() -> Result<T, ClientError> + Send + 'static,
    ) -> Result<T, ClientError> {
        let (guard, registration) = self.register(None)?;
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            if registration.handle().is_aborted() {
                return Err(ClientError::WorkCancelled);
            }
            operation()
        })
        .await
        .map_err(ClientError::Task)?
    }

    pub(crate) fn stop(&self, cancel_running_work: bool) -> Result<(), Vec<RunningWorkView>> {
        let mut registry = self.registry.lock().unwrap();
        let running: Vec<_> = registry
            .work
            .values()
            .filter_map(|work| {
                work.label.as_ref().map(|label| RunningWorkView {
                    label: label.clone(),
                })
            })
            .collect();
        if !cancel_running_work && !running.is_empty() {
            return Err(running);
        }
        registry.stopped = true;
        let caller = self.caller();
        for (id, work) in &registry.work {
            if Some(*id) != caller {
                work.abort.abort();
            }
        }
        Ok(())
    }

    fn caller(&self) -> Option<u64> {
        CURRENT_WORK
            .try_with(|(session, id)| std::ptr::eq(session.as_ref(), self).then_some(*id))
            .ok()
            .flatten()
    }

    pub(crate) async fn wait(&self) {
        let caller = self.caller();
        let mut changed = self.changed.subscribe();
        loop {
            changed.borrow_and_update();
            if self
                .registry
                .lock()
                .unwrap()
                .work
                .keys()
                .all(|id| Some(*id) == caller)
            {
                return;
            }
            if changed.changed().await.is_err() {
                return;
            }
        }
    }
}
