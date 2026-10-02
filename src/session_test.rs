//! Ownership for an editor's asynchronous connection test. A finished, replaced
//! or closed test must never reopen authentication UI or update a later draft.
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use tokio::task::AbortHandle;

static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub(crate) struct TestTicket {
    pub(crate) id: u64,
    snapshot: u64,
    active: Arc<AtomicBool>,
}

impl TestTicket {
    pub(crate) fn matches_snapshot(&self, snapshot: u64) -> bool {
        self.snapshot == snapshot
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub(crate) fn finish(&self) -> bool {
        self.active.swap(false, Ordering::AcqRel)
    }
}

#[derive(Default)]
pub(crate) struct EditorTest {
    ticket: Option<TestTicket>,
    task: Option<AbortHandle>,
}

impl EditorTest {
    /// Call stop first so the UI can dismiss prompts owned by the old id.
    pub(crate) fn begin(&mut self, snapshot: u64) -> TestTicket {
        self.stop();
        let ticket = TestTicket {
            id: NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed),
            snapshot,
            active: Arc::new(AtomicBool::new(true)),
        };
        self.ticket = Some(ticket.clone());
        ticket
    }

    pub(crate) fn matches_snapshot(&self, snapshot: u64) -> bool {
        self.ticket
            .as_ref()
            .is_some_and(|ticket| ticket.matches_snapshot(snapshot))
    }

    pub(crate) fn attach(&mut self, task: AbortHandle) {
        self.task = Some(task);
    }

    pub(crate) fn stop(&mut self) -> Option<u64> {
        let id = self.ticket.take().map(|ticket| {
            ticket.finish();
            ticket.id
        });
        if let Some(task) = self.task.take() {
            task.abort();
        }
        id
    }
}

impl Drop for EditorTest {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaced_saved_cancelled_and_reopened_tests_reject_late_results() {
        let mut test = EditorTest::default();
        let first = test.begin(1);
        let second = test.begin(1);
        assert!(!first.is_active());
        assert!(!first.finish());
        assert!(second.is_active());
        assert!(test.matches_snapshot(1));
        assert!(!test.matches_snapshot(2));
        assert_eq!(test.stop(), Some(second.id)); // Save / Cancel / editor reopen
        assert!(!second.is_active());
        assert!(!test.matches_snapshot(1));
        let third = test.begin(1);
        assert_ne!(second.id, third.id);
        assert!(third.finish());
        assert!(!third.finish()); // duplicate completion cannot update the UI
        assert!(!second.finish());
    }

    #[tokio::test]
    async fn stopping_or_dropping_editor_aborts_only_its_test() {
        let mut test = EditorTest::default();
        let ticket = test.begin(1);
        let task = tokio::spawn(std::future::pending::<()>());
        test.attach(task.abort_handle());
        let unrelated = tokio::spawn(async { 42 });
        drop(test);
        assert!(!ticket.is_active());
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(unrelated.await.unwrap(), 42);
    }
}
