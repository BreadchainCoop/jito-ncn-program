use std::time::Duration;

use crate::{
    getters::get_guaranteed_epoch_and_slot,
    handler::CliHandler,
    instructions::{crank_snapshot_unupdated, get_or_create_snapshot},
    keeper::{
        keeper_metrics::{emit_error, emit_heartbeat},
        keeper_state::KeeperState,
    },
};
use anyhow::Result;
use log::info;
use solana_metrics::set_host_id;
use std::process::Command;
use tokio::time::sleep;

/// Main entry point for the NCN (Network Coordinated Node) keeper
///
/// The keeper is responsible for managing snapshot operations:
/// 1. Ensuring snapshot and vote counter accounts exist
/// 2. Fetching snapshot state from on-chain
/// 3. Determining which operators need snapshotting
/// 4. Taking snapshots for operators that need updates
///
/// The keeper runs in a continuous loop, monitoring the snapshot state and
/// taking snapshots when operators are due for updates.
///
/// # Arguments
/// * `handler` - CLI handler containing RPC client and configuration
/// * `loop_timeout_ms` - Timeout between main loop iterations when stalled
/// * `error_timeout_ms` - Timeout after errors before retrying
pub async fn startup_ncn_keeper(
    handler: &CliHandler,
    loop_timeout_ms: u64,
    error_timeout_ms: u64,
) -> Result<()> {
    let mut state: KeeperState = KeeperState::default();
    let mut current_keeper_epoch = handler.epoch;
    let mut tick = 0;

    // Set up metrics host identification
    let hostname_cmd = Command::new("hostname")
        .output()
        .expect("Failed to execute hostname command");

    let hostname = String::from_utf8_lossy(&hostname_cmd.stdout)
        .trim()
        .to_string();

    set_host_id(format!("ncn-program-keeper_{}", hostname));

    // PHASE 0: INITIALIZATION
    // Ensure required accounts exist before starting the main loop
    info!("\n\n0. Initializing required accounts\n");

    // Create snapshot account if it doesn't exist
    let result = get_or_create_snapshot(handler, current_keeper_epoch).await;
    if check_and_timeout_error(
        "Create Snapshot".to_string(),
        &result,
        error_timeout_ms,
        current_keeper_epoch,
    )
    .await
    {
        return Err(anyhow::anyhow!("Failed to create snapshot account"));
    }

    info!("Required accounts initialized successfully");

    loop {
        // PHASE 1: EPOCH PROGRESSION LOGIC
        // Update the epoch if needed
        {
            info!(
                "\n\n1. Check Epoch Progression - {}\n",
                current_keeper_epoch
            );
            let (current_epoch, _) = get_guaranteed_epoch_and_slot(handler).await;

            if current_epoch != current_keeper_epoch {
                info!(
                    "\n\nPROGRESS EPOCH: {} -> {}\n\n",
                    current_keeper_epoch, current_epoch
                );
                current_keeper_epoch = current_epoch;
            }
        }

        // PHASE 2: KEEPER STATE UPDATE
        // Fetch and update the keeper's internal state for snapshot management
        {
            info!(
                "\n\n2. Fetch and Update Snapshot State - {}\n",
                current_keeper_epoch
            );

            // If the epoch has changed, fetch the new state
            if state.epoch != current_keeper_epoch {
                let result = state.fetch(handler, current_keeper_epoch).await;

                if check_and_timeout_error(
                    "Update Keeper State".to_string(),
                    &result,
                    error_timeout_ms,
                    current_keeper_epoch,
                )
                .await
                {
                    continue;
                }
            } else {
                // Otherwise, just update the existing snapshot state
                let result = state.update_state(handler).await;

                if check_and_timeout_error(
                    "Update Snapshot State".to_string(),
                    &result,
                    error_timeout_ms,
                    current_keeper_epoch,
                )
                .await
                {
                    continue;
                }
            }

            info!(
                "Snapshot state updated - should_snapshot: {}, operators_to_snapshot: {}",
                state.get_should_snapshot(),
                state.get_operators_to_snapshot().len()
            );
        }

        // PHASE 3: SNAPSHOT OPERATIONS
        // Check if snapshotting is needed and execute if required
        {
            info!(
                "\n\n3. Check and Execute Snapshots - {}\n",
                current_keeper_epoch
            );

            if state.get_should_snapshot() {
                info!(
                    "Snapshotting needed for {} operators",
                    state.get_operators_to_snapshot().len()
                );

                let result = crank_snapshot_unupdated(handler, current_keeper_epoch, true).await;

                if check_and_timeout_error(
                    "Crank Snapshot".to_string(),
                    &result,
                    error_timeout_ms,
                    current_keeper_epoch,
                )
                .await
                {
                    continue;
                }

                info!("Snapshot operations completed successfully");
            } else {
                info!("No snapshotting needed - all operators are up to date");
            }
        }

        // MAIN LOOP TIMEOUT
        // Wait before the next iteration and emit a heartbeat
        info!("\n\n -- Loop Timeout -- {}\n", current_keeper_epoch);
        timeout_keeper(loop_timeout_ms).await;
        emit_heartbeat(tick).await;
        tick += 1;
    }
}

/// Handles errors consistently across the keeper loop
///
/// This function:
/// 1. Logs errors with context
/// 2. Emits error metrics for monitoring
/// 3. Applies a timeout before allowing retry
///
/// # Arguments
/// * `title` - Description of the operation that failed
/// * `result` - The result to check for errors
/// * `error_timeout_ms` - How long to wait after an error
/// * `keeper_epoch` - Current epoch for error context
///
/// # Returns
/// `true` if an error occurred and was handled, `false` if no error
#[allow(clippy::future_not_send)]
async fn check_and_timeout_error<T>(
    title: String,
    result: &Result<T>,
    error_timeout_ms: u64,
    keeper_epoch: u64,
) -> bool {
    if let Err(e) = result {
        let error = format!("{:?}", e);
        let message = format!("Error: [{}] \n{}\n\n", title, error);

        log::error!("{}", message);
        emit_error(title, error, message, keeper_epoch).await;
        timeout_error(error_timeout_ms).await;
        true
    } else {
        false
    }
}

/// Applies a timeout after an error occurs
///
/// This prevents rapid retry attempts that could overwhelm the system
/// or hit rate limits on the RPC endpoint.
///
/// # Arguments
/// * `duration_ms` - Timeout duration in milliseconds
async fn timeout_error(duration_ms: u64) {
    info!("Error Timeout for {}s", duration_ms as f64 / 1000.0);
    sleep(Duration::from_millis(duration_ms)).await;
}

/// Applies the main keeper loop timeout
///
/// This timeout occurs when the keeper has completed all work for the current
/// epoch and is waiting for external conditions to change (e.g., new epoch,
/// operator votes, etc.).
///
/// # Arguments
/// * `duration_ms` - Timeout duration in milliseconds
async fn timeout_keeper(duration_ms: u64) {
    info!("Keeper Timeout for {}s", duration_ms as f64 / 1000.0);
    sleep(Duration::from_millis(duration_ms)).await;
}
