//! Opt-in HTTP proxy for remote agent processes and installers. The remote server
//! listens on loopback and the desktop makes the outbound connections.

mod proxy;
mod relay;
#[cfg(test)]
mod tests;

use crate::Project;
use ide_core::proxy_settings::{ProxyMode, ProxySettings};
use ide_core::{CancellationToken, IdeError, IdeResult};
use ide_jsonrpc::{Peer, codes};
use parking_lot::Mutex;
use relay::{FRAME, Relay};
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::net::TcpListener;
use uuid::Uuid;

pub const CONFIGURE: &str = "agent_configure_network";
const OPEN: &str = "agent_network_open";
const BUFFER: usize = 64 * 1024;

/// A grant belongs to one desktop connection and is explicitly revoked on disconnect.
#[derive(Default)]
pub struct Desktop {
    relay: Arc<Relay>,
    grant: Mutex<Option<Grant>>,
}

struct Grant {
    peer: Peer,
    proxy: Arc<proxy::Proxy>,
    stop: CancellationToken,
}

impl Drop for Grant {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl Desktop {
    pub fn enable(&self, peer: Peer) -> IdeResult<String> {
        self.enable_with_proxy(peer, ide_core::proxy_settings::proxy())
    }

    fn enable_with_proxy(&self, peer: Peer, settings: Arc<ProxySettings>) -> IdeResult<String> {
        let token = Uuid::new_v4().simple().to_string();
        let proxy = Arc::new(proxy::Proxy::new(&token, settings)?);
        *self.grant.lock() = Some(Grant { peer, proxy, stop: CancellationToken::new() });
        Ok(token)
    }

    pub fn enabled(&self) -> bool {
        self.grant.lock().is_some()
    }

    pub fn disable(&self) {
        self.grant.lock().take();
    }

    pub fn on_request(&self, method: &str, params: &Value) -> Option<Result<Value, (i64, String)>> {
        if method != OPEN {
            return None;
        }
        let result = (|| {
            let id = params.as_u64().ok_or((codes::INVALID_PARAMS, "Invalid stream id".into()))?;
            let grant = self.grant.lock();
            let grant = grant.as_ref().ok_or((codes::INTERNAL_ERROR, "Agent network access is disabled".into()))?;
            let slot = self.relay.open(id, grant.peer.clone()).map_err(|e| (codes::INTERNAL_ERROR, e.message))?;
            let (stream, socket) = tokio::io::duplex(BUFFER);
            let proxy = grant.proxy.clone();
            let stop = grant.stop.child_token();
            tokio::spawn(async move {
                let serve = proxy.serve(socket, stop.clone());
                let relay = async {
                    slot.run(stream, &stop).await;
                    stop.cancel();
                };
                tokio::join!(serve, relay);
            });
            Ok(Value::Null)
        })();
        Some(result)
    }

    pub fn on_notification(&self, method: &str, params: &Value) -> bool {
        if method == FRAME {
            self.relay.receive(params);
            return true;
        }
        false
    }
}

#[derive(Default)]
pub(crate) struct Server {
    relay: Arc<Relay>,
    next: Arc<AtomicU64>,
    listener: tokio::sync::Mutex<Option<CancellationToken>>,
}

impl Server {
    pub async fn configure(&self, project: &Project, peer: Peer, token: Option<String>) -> IdeResult<()> {
        let mut active = self.listener.lock().await;
        let (proxy, next) = match token {
            Some(token) => {
                if Uuid::try_parse(&token).is_err() {
                    return Err(IdeError::invalid("Invalid agent network token"));
                }
                let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
                let port = listener.local_addr()?.port();
                let stop = CancellationToken::new();
                tokio::spawn(listen(listener, peer, self.relay.clone(), self.next.clone(), stop.clone()));
                let proxy = ProxySettings {
                    mode: ProxyMode::Manual,
                    host: "127.0.0.1".into(),
                    port,
                    authenticate: true,
                    login: proxy::USER.into(),
                    password: token,
                    ..ProxySettings::default()
                };
                (Some(Arc::new(proxy)), Some(stop))
            }
            None => (None, None),
        };
        project.agents.set_network_proxy(proxy);
        if let Some(previous) = std::mem::replace(&mut *active, next) {
            previous.cancel();
        }
        Ok(())
    }

    pub fn on_notification(&self, method: &str, params: Value) {
        if method == FRAME {
            self.relay.receive(&params);
        }
    }
}

async fn listen(listener: TcpListener, peer: Peer, relay: Arc<Relay>, next: Arc<AtomicU64>, stop: CancellationToken) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let socket = tokio::select! {
            _ = stop.cancelled() => break,
            _ = peer.closed() => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => continue,
            socket = listener.accept() => socket,
        };
        let Ok((socket, _)) = socket else { break };
        let _ = socket.set_nodelay(true);
        let id = next.fetch_add(1, Ordering::Relaxed);
        let Ok(slot) = relay.open(id, peer.clone()) else { continue };
        let peer = peer.clone();
        let stop = stop.clone();
        connections.spawn(async move {
            let opened =
                tokio::time::timeout(Duration::from_secs(20), peer.request::<_, ()>(OPEN, id, Some(stop.clone())));
            if matches!(opened.await, Ok(Ok(()))) {
                slot.run(socket, &stop).await;
            }
        });
    }
}
