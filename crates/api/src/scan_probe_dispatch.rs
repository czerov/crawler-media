//! Dispatch pending probe obligations bounded at 100 entries per round.

use std::sync::Arc;

use crate::management::ApiState;

/// Attempt to dispatch up to 100 pending probe obligations.
///
/// Returns the number of dispatched and acknowledged obligations.
pub async fn dispatch_once(state: ApiState) -> Result<usize, String> {
    tokio::task::spawn_blocking(move || dispatch_once_sync(&state))
        .await
        .map_err(|e| e.to_string())?
}

fn dispatch_once_sync(state: &ApiState) -> Result<usize, String> {
    let obligations = {
        let store = state.store.lock();
        store
            .scan_probe_obligations(100)
            .map_err(|e| e.to_string())?
    };

    if obligations.is_empty() {
        return Ok(0);
    }

    let mut acked = 0;
    for ob in obligations {
        let row_opt = {
            let store = state.store.lock();
            store.get_ledger(&ob.ledger_id.to_string()).map_err(|e| e.to_string())?
        };

        let Some(row) = row_opt else {
            // Ledger row no longer exists (pruned/deleted), ack obligation immediately
            let store = state.store.lock();
            let _ = store.ack_scan_probe_obligation(ob.ledger_id);
            acked += 1;
            continue;
        };

        let _ = crate::http::library::ensure_probe_enqueued(state, &row);
        let store = state.store.lock();
        if store.ack_scan_probe_obligation(ob.ledger_id).is_ok() {
            acked += 1;
        }
    }

    Ok(acked)
}

/// Start background dispatcher task checking obligations every 5 seconds.
pub fn start_dispatcher(state: ApiState) {
    tokio::spawn(async move {
        // Initial dispatch on startup
        let _ = dispatch_once(state.clone()).await;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            if let Err(e) = dispatch_once(state.clone()).await {
                tracing::error!(error = %e, "Probe obligation dispatcher failed");
            }
        }
    });
}
