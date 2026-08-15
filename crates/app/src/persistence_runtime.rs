use std::{future::Future, sync::LazyLock};

use tokio::runtime::Runtime;

#[expect(
    clippy::expect_used,
    reason = "the process-wide persistence runtime is startup infrastructure and cannot recover without an executor"
)]
static PERSISTENCE_RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .thread_name("sotto-persistence")
        .enable_all()
        .build()
        .expect("Sotto persistence runtime must start")
});

/// Bridges synchronous GPUI controller boundaries to the async persistence contract.
///
/// Async workers await `Store` directly. This bridge exists only for synchronous UI actions that
/// already run away from rendering and need an immediate result to update controller state.
pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(|| PERSISTENCE_RUNTIME.block_on(future))
    } else {
        PERSISTENCE_RUNTIME.block_on(future)
    }
}
