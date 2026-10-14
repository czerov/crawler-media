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
            if let Err(error) = store.ack_scan_probe_obligation(ob.ledger_id) {
                tracing::warn!(
                    %error,
                    ledger_id = %ob.ledger_id,
                    "台账行已不存在，确认探测义务失败（该义务将被反复重派）"
                );
            }
            acked += 1;
            continue;
        };

        let enqueued_or_active = match state
            .probe
            .request_probe(&row, crate::probe_manager::policy::ProbeRequestOrigin::Detail)
        {
            Ok(
                crate::probe_manager::policy::ProbeRequestResult::Queued { .. }
                | crate::probe_manager::policy::ProbeRequestResult::AlreadyRunning
                | crate::probe_manager::policy::ProbeRequestResult::Complete,
            ) => true,
            _ => false,
        };

        if enqueued_or_active {
            let store = state.store.lock();
            match store.ack_scan_probe_obligation(ob.ledger_id) {
                Ok(()) => acked += 1,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        ledger_id = %ob.ledger_id,
                        "确认探测义务失败（该义务将被反复重派）"
                    );
                }
            }
        }
    }

    Ok(acked)
}

/// Start background dispatcher task checking obligations every 5 seconds.
pub fn start_dispatcher(state: ApiState) {
    tokio::spawn(async move {
        // Initial dispatch on startup
        if let Err(error) = dispatch_once(state.clone()).await {
            tracing::error!(%error, "探测义务调度器启动首轮派发失败");
        }
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            if let Err(e) = dispatch_once(state.clone()).await {
                tracing::error!(error = %e, "Probe obligation dispatcher failed");
            }
        }
    });
}
