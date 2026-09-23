//! Whose work a call to a server is: the person's, or the sync loops'.
//! A task-local, so work a caller wraps carries its priority into every
//! call inside it without passing it through each function. Each adapter
//! maps it to its own pacing; the Google adapter maps it to Gmail's quota
//! bucket.

/// Anything the person asked for, and anything the assistant does on
/// their behalf, is foreground; the sync loops that backfill the window
/// and poll for changes are background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Foreground,
    Background,
}

tokio::task_local! {
    static AMBIENT: Priority;
}

/// Runs `work` as background work, so the calls inside it wait behind the
/// person's. A task `work` spawns does not inherit this and counts as
/// foreground, so wrap the loop rather than the runtime.
pub async fn background<F: Future>(work: F) -> F::Output {
    AMBIENT.scope(Priority::Background, work).await
}

/// The priority of the work running now: foreground, unless a caller up
/// the stack wrapped it in [`background`].
pub fn priority() -> Priority {
    AMBIENT.try_with(|p| *p).unwrap_or(Priority::Foreground)
}

#[cfg(test)]
mod tests {
    use super::{Priority, background, priority};

    #[tokio::test]
    async fn work_is_foreground_unless_wrapped() {
        assert_eq!(priority(), Priority::Foreground);
        assert_eq!(background(async { priority() }).await, Priority::Background);
    }
}
