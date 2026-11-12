//! Bounded byte streams over the remote IDE connection.

use base64::{Engine, engine::general_purpose::STANDARD};
use ide_core::{CancellationToken, IdeError, IdeResult};
use ide_jsonrpc::Peer;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Semaphore, mpsc};

pub(super) const FRAME: &str = "agent_network_frame";
const CHUNK: usize = 64 * 1024;
const WINDOW: usize = 16;
const MAX_STREAMS: usize = 64;

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Frame {
    Data { data: String },
    Credit,
    Eof,
    Close,
}

#[derive(Deserialize, Serialize)]
struct Packet {
    id: u64,
    #[serde(flatten)]
    frame: Frame,
}

struct Stream {
    input: mpsc::Sender<Option<Vec<u8>>>,
    credit: Semaphore,
    stop: CancellationToken,
}

#[derive(Default)]
pub(super) struct Relay(Mutex<HashMap<u64, Arc<Stream>>>);

impl Relay {
    pub fn open(self: &Arc<Self>, id: u64, peer: Peer) -> IdeResult<Slot> {
        let mut streams = self.0.lock();
        if streams.len() >= MAX_STREAMS || streams.contains_key(&id) {
            return Err(IdeError::unavailable("Too many agent network connections or a duplicate stream"));
        }
        let (input, receive) = mpsc::channel(WINDOW + 1);
        let stream = Arc::new(Stream { input, credit: Semaphore::new(WINDOW), stop: CancellationToken::new() });
        streams.insert(id, stream.clone());
        Ok(Slot { id, peer, relay: self.clone(), stream, receive, finished: false })
    }

    pub fn receive(&self, params: &Value) {
        let Ok(Packet { id, frame }) = Packet::deserialize(params) else { return };
        let Some(stream) = self.0.lock().get(&id).cloned() else { return };
        let valid = match frame {
            Frame::Data { data } if data.len() <= CHUNK.div_ceil(3) * 4 => STANDARD
                .decode(data)
                .ok()
                .filter(|bytes| !bytes.is_empty() && bytes.len() <= CHUNK)
                .is_some_and(|bytes| stream.input.try_send(Some(bytes)).is_ok()),
            Frame::Eof => stream.input.try_send(None).is_ok(),
            Frame::Credit if stream.credit.available_permits() < WINDOW => {
                stream.credit.add_permits(1);
                true
            }
            Frame::Close | Frame::Credit | Frame::Data { .. } => false,
        };
        if !valid {
            stream.stop.cancel();
        }
    }
}

pub(super) struct Slot {
    id: u64,
    peer: Peer,
    relay: Arc<Relay>,
    stream: Arc<Stream>,
    receive: mpsc::Receiver<Option<Vec<u8>>>,
    finished: bool,
}

impl Slot {
    pub async fn run(mut self, socket: impl AsyncRead + AsyncWrite + Unpin, stop: &CancellationToken) {
        let (mut read, mut write) = tokio::io::split(socket);
        let send = |frame| self.peer.notify(FRAME, Packet { id: self.id, frame }).map_err(std::io::Error::other);
        let upload = async {
            let mut buffer = vec![0; CHUNK];
            loop {
                let credit = self.stream.credit.acquire().await.expect("the stream semaphore stays open");
                let n = read.read(&mut buffer).await?;
                if n == 0 {
                    return send(Frame::Eof);
                }
                credit.forget();
                send(Frame::Data { data: STANDARD.encode(&buffer[..n]) })?;
            }
        };
        let download = async {
            while let Some(Some(data)) = self.receive.recv().await {
                write.write_all(&data).await?;
                send(Frame::Credit)?;
            }
            write.shutdown().await
        };
        self.finished = tokio::select! {
            _ = stop.cancelled() => false,
            _ = self.stream.stop.cancelled() => false,
            _ = self.peer.closed() => false,
            result = async { tokio::try_join!(upload, download) } => result.is_ok(),
        };
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.relay.0.lock().remove(&self.id);
        if !self.finished {
            let _ = self.peer.notify(FRAME, Packet { id: self.id, frame: Frame::Close });
        }
    }
}
