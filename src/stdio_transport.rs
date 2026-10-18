//! Splits stdin into a queue of user turns and per-`request_id` permission replies,
//! so the two can arrive in any order.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncRead, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::ndjson;
use crate::protocol::{ControlResult, Inbound, UserMessage};

pub type PendingRequests = Arc<Mutex<HashMap<String, oneshot::Sender<ControlResult>>>>;

pub struct StdinRouter {
    pub user_rx: mpsc::Receiver<UserMessage>,
    pub pending: PendingRequests,
}

/// Resolved by the matching `control_response`, or dropped if stdin closes first.
pub fn register_pending(
    pending: &PendingRequests,
    request_id: String,
) -> oneshot::Receiver<ControlResult> {
    let (tx, rx) = oneshot::channel();
    pending.lock().unwrap().insert(request_id, tx);
    rx
}

pub fn spawn_reader<R>(reader: R) -> StdinRouter
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let (user_tx, user_rx) = mpsc::channel::<UserMessage>(16);
    let pending: PendingRequests = Arc::new(Mutex::new(HashMap::new()));
    let pending_for_task = pending.clone();

    tokio::spawn(async move {
        let mut buf = BufReader::new(reader);
        loop {
            match ndjson::read_line(&mut buf).await {
                Ok(Some(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    route_line(&line, &user_tx, &pending_for_task).await;
                }
                Ok(None) => break, // EOF
                Err(err) => {
                    tracing::error!(%err, "error reading stdin, shutting down reader");
                    break;
                }
            }
        }
        // EOF: end the agent loop, and turn pending permission waits into denials.
        drop(user_tx);
        pending_for_task.lock().unwrap().clear();
    });

    StdinRouter { user_rx, pending }
}

async fn route_line(line: &str, user_tx: &mpsc::Sender<UserMessage>, pending: &PendingRequests) {
    match serde_json::from_str::<Inbound>(line) {
        Ok(Inbound::User(env)) => {
            if user_tx.send(env.message).await.is_err() {
                tracing::warn!("user message dropped: agent loop is no longer receiving");
            }
        }
        Ok(Inbound::ControlResponse(env)) => {
            let sender = pending.lock().unwrap().remove(&env.request_id);
            match sender {
                Some(tx) => {
                    let _ = tx.send(env.result);
                }
                None => {
                    tracing::warn!(request_id = %env.request_id, "control_response for unknown/late request_id, dropping");
                }
            }
        }
        Ok(Inbound::ControlCancelRequest(_)) => {
            tracing::info!(
                "control_cancel_request received; cancellation not supported in Phase 1, ignoring"
            );
        }
        Err(err) => {
            tracing::warn!(%err, line = %line, "failed to parse inbound message, skipping");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::PermissionDecision;

    #[tokio::test]
    async fn routes_interleaved_user_and_control_response_lines() {
        let (client, server) = tokio::io::duplex(4096);
        let mut router = spawn_reader(server);

        let mut writer = client;
        use tokio::io::AsyncWriteExt;

        // Register a pending request the way agent.rs would, before the response
        // for it arrives on "stdin".
        let rx = register_pending(&router.pending, "req_1".to_string());

        // Interleave: a control_response for an unknown id (must be dropped, not
        // panic), then a user line, then the real control_response.
        writer
            .write_all(b"{\"type\":\"control_response\",\"request_id\":\"unknown\",\"subtype\":\"success\",\"decision\":{\"behavior\":\"allow\"}}\n")
            .await
            .unwrap();
        writer
            .write_all(b"{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n")
            .await
            .unwrap();
        writer
            .write_all(b"{\"type\":\"control_response\",\"request_id\":\"req_1\",\"subtype\":\"success\",\"decision\":{\"behavior\":\"allow\"}}\n")
            .await
            .unwrap();

        let user_msg = router.user_rx.recv().await.unwrap();
        assert_eq!(user_msg.content, "hi");

        let result = rx.await.unwrap();
        match result {
            ControlResult::Success {
                decision: PermissionDecision::Allow { .. },
            } => {}
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[tokio::test]
    async fn eof_closes_user_channel_and_drops_pending_senders() {
        let (client, server) = tokio::io::duplex(4096);
        let mut router = spawn_reader(server);
        let rx = register_pending(&router.pending, "req_2".to_string());

        drop(client); // simulate stdin EOF

        assert!(router.user_rx.recv().await.is_none());
        assert!(rx.await.is_err());
    }
}
