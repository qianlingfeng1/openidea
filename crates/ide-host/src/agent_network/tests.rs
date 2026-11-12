use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use ide_jsonrpc::Handler;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

struct ClientHandler(Arc<Desktop>);

impl Handler for ClientHandler {
    fn on_notification(&self, method: &str, params: Value) {
        self.0.on_notification(method, &params);
    }

    fn on_request(&self, method: &str, params: Value) -> Result<Value, (i64, String)> {
        self.0.on_request(method, &params).expect("agent network request")
    }
}

struct ServerHandler(Arc<Server>);

impl Handler for ServerHandler {
    fn on_notification(&self, method: &str, params: Value) {
        self.0.on_notification(method, params);
    }

    fn on_request(&self, _: &str, _: Value) -> Result<Value, (i64, String)> {
        Err((codes::METHOD_NOT_FOUND, "unexpected request".into()))
    }
}

#[tokio::test]
async fn authenticated_connect_carries_streaming_bytes_and_revocation_closes_it() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let desktop = Arc::new(Desktop::default());
        let server = Arc::new(Server::default());
        let (client, remote) = tokio::io::duplex(2 * BUFFER);
        let (read, write) = tokio::io::split(client);
        let client = Peer::connect(read, write, Arc::new(ClientHandler(desktop.clone())));
        let (read, write) = tokio::io::split(remote);
        let remote = Peer::connect(read, write, Arc::new(ServerHandler(server.clone())));
        assert!(remote.request::<_, ()>(OPEN, 0, None).await.is_err());
        let settings = ProxySettings { mode: ProxyMode::None, ..ProxySettings::default() };
        let token = desktop.enable_with_proxy(client.clone(), Arc::new(settings)).unwrap();

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let proxy = listener.local_addr().unwrap();
        let stop = CancellationToken::new();
        let listener = tokio::spawn(listen(listener, remote.clone(), server.relay.clone(), server.next.clone(), stop));
        let origin = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let target = origin.local_addr().unwrap();
        let address = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
        let mut socket = BufReader::new(TcpStream::connect(proxy).await.unwrap());
        socket.get_mut().write_all(format!("{address}\r\n").as_bytes()).await.unwrap();
        assert_eq!(status(&mut socket).await, "HTTP/1.1 407 Proxy Authentication Required");
        drop(socket);

        let auth = STANDARD.encode(format!("{}:{token}", proxy::USER));
        let payload = vec![b'x'; 2 * 1024 * 1024];
        let origin = tokio::spawn({
            let payload = payload.clone();
            async move {
                let (mut socket, _) = origin.accept().await.unwrap();
                let mut ping = [0; 4];
                socket.read_exact(&mut ping).await.unwrap();
                assert_eq!(&ping, b"ping");
                socket.write_all(&payload).await.unwrap();
                assert_eq!(socket.read(&mut ping).await.unwrap(), 0);
            }
        });
        let mut socket = BufReader::new(TcpStream::connect(proxy).await.unwrap());
        socket
            .get_mut()
            .write_all(format!("{address}Proxy-Authorization: Basic {auth}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        assert_eq!(status(&mut socket).await, "HTTP/1.1 200 OK");
        socket.get_mut().write_all(b"ping").await.unwrap();
        let mut received = vec![0; payload.len()];
        socket.read_exact(&mut received).await.unwrap();
        assert_eq!(received, payload);

        desktop.disable();
        assert_eq!(socket.read(&mut [0]).await.unwrap(), 0);
        origin.await.unwrap();
        remote.kill();
        listener.await.unwrap();
    })
    .await
    .unwrap();
}

async fn status(socket: &mut BufReader<TcpStream>) -> String {
    let mut status = String::new();
    socket.read_line(&mut status).await.unwrap();
    loop {
        let mut line = String::new();
        assert!(socket.read_line(&mut line).await.unwrap() > 0);
        if line == "\r\n" {
            return status.trim_end().into();
        }
    }
}
