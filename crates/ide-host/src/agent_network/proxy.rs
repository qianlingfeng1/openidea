//! The desktop end of the remote agent HTTP proxy. CONNECT leaves TLS end to end.

use base64::{Engine, engine::general_purpose::STANDARD};
use hyper::body::Incoming;
use hyper::header::{self, HeaderMap, HeaderValue};
use hyper::{Method, Request, Response, StatusCode, Uri};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::connect::proxy::{SocksV4, SocksV5, Tunnel};
use hyper_util::client::proxy::matcher::Matcher;
use hyper_util::rt::{TokioIo, TokioTimer};
use ide_core::proxy_settings::{ProxyRoute, ProxySettings};
use ide_core::{CancellationToken, IdeResult};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream};
use tower_service::Service;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
pub(super) const USER: &str = "openidea";

pub(super) struct Proxy {
    authorization: HeaderValue,
    settings: Arc<ProxySettings>,
    system: Matcher,
    http: reqwest::Client,
}

impl Proxy {
    pub fn new(token: &str, settings: Arc<ProxySettings>) -> IdeResult<Self> {
        let encoded = STANDARD.encode(format!("{USER}:{token}"));
        let mut authorization = HeaderValue::from_str(&format!("Basic {encoded}")).expect("base64 is a header value");
        authorization.set_sensitive(true);
        let http = ide_http::client::build_with_redirect(&settings, reqwest::redirect::Policy::none())?;
        Ok(Self { authorization, settings, system: Matcher::from_system(), http })
    }

    pub async fn serve(self: Arc<Self>, socket: DuplexStream, stop: CancellationToken) {
        let service = hyper::service::service_fn(|request| {
            let proxy = self.clone();
            let stop = stop.clone();
            async move { Ok::<_, std::convert::Infallible>(proxy.request(request, stop).await) }
        });
        let connection = hyper::server::conn::http1::Builder::new()
            .timer(TokioTimer::new())
            .header_read_timeout(CONNECT_TIMEOUT)
            .serve_connection(TokioIo::new(socket), service)
            .with_upgrades();
        tokio::select! {
            _ = stop.cancelled() => {}
            _ = connection => {}
        }
    }

    async fn request(&self, mut request: Request<Incoming>, stop: CancellationToken) -> Response<reqwest::Body> {
        if request.headers_mut().remove(header::PROXY_AUTHORIZATION).as_ref() != Some(&self.authorization) {
            let mut response = response(StatusCode::PROXY_AUTHENTICATION_REQUIRED, "Proxy authentication required");
            response
                .headers_mut()
                .insert(header::PROXY_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"OpenIDEA\""));
            return response;
        }
        if request.method() == Method::CONNECT {
            let target = request.uri();
            let destination = match (target.authority(), target.port_u16()) {
                (Some(authority), Some(port)) if port > 0 && target.scheme().is_none() => {
                    format!("https://{authority}").parse::<Uri>()
                }
                _ => return response(StatusCode::BAD_REQUEST, "CONNECT requires a host and port"),
            };
            let Ok(destination) = destination else { return response(StatusCode::BAD_REQUEST, "Invalid destination") };
            let connected = tokio::time::timeout(CONNECT_TIMEOUT, self.connect(destination)).await;
            let mut upstream = match connected {
                Ok(Ok(upstream)) => upstream,
                Ok(Err(error)) => {
                    tracing::debug!("agent proxy CONNECT failed: {error}");
                    return response(StatusCode::BAD_GATEWAY, "This computer could not connect to the destination");
                }
                Err(_) => return response(StatusCode::GATEWAY_TIMEOUT, "Connection from this computer timed out"),
            };
            let upgrade = hyper::upgrade::on(&mut request);
            tokio::spawn(async move {
                tokio::select! {
                    _ = stop.cancelled() => {}
                    _ = async {
                        if let Ok(socket) = upgrade.await {
                            let _ = tokio::io::copy_bidirectional(&mut TokioIo::new(socket), &mut upstream).await;
                        }
                    } => {}
                }
            });
            return response(StatusCode::OK, "");
        }
        let (mut parts, body) = request.into_parts();
        if parts.uri.scheme_str() != Some("http") || parts.uri.host().is_none() {
            return response(StatusCode::BAD_REQUEST, "An absolute HTTP URL is required");
        }
        strip_hop_headers(&mut parts.headers);
        parts.headers.remove(header::HOST);
        match self
            .http
            .request(parts.method, parts.uri.to_string())
            .headers(parts.headers)
            .body(reqwest::Body::wrap(body))
            .send()
            .await
        {
            Ok(upstream) => {
                let mut response: Response<reqwest::Body> = upstream.into();
                strip_hop_headers(response.headers_mut());
                response
            }
            Err(_) => response(StatusCode::BAD_GATEWAY, "This computer could not reach the destination"),
        }
    }

    async fn connect(&self, destination: Uri) -> Result<Box<dyn Transport>, Box<dyn std::error::Error + Send + Sync>> {
        let route = match self.settings.route(destination.host()) {
            ProxyRoute::Direct => None,
            ProxyRoute::System => self.system.intercept(&destination),
            ProxyRoute::Via(proxy) => Matcher::builder().all(proxy).build().intercept(&destination),
        };
        let mut tcp = HttpConnector::new();
        tcp.enforce_http(false);
        tcp.set_nodelay(true);
        let Some(proxy) = route else { return Ok(Box::new(tcp.call(destination).await?.into_inner())) };
        let endpoint = proxy.uri().clone();
        let connection = match endpoint.scheme_str() {
            Some("socks4" | "socks4a") => {
                let local = endpoint.scheme_str() == Some("socks4");
                return Ok(Box::new(
                    SocksV4::new(endpoint, tcp).local_dns(local).call(destination).await?.into_inner(),
                ));
            }
            Some("socks5" | "socks5h") => {
                let local = endpoint.scheme_str() == Some("socks5");
                let mut socks = SocksV5::new(endpoint, tcp).local_dns(local);
                if let Some((user, password)) = proxy.raw_auth() {
                    socks = socks.with_auth(user.into(), password.into());
                }
                return Ok(Box::new(socks.call(destination).await?.into_inner()));
            }
            Some("http" | "https") => hyper_rustls::HttpsConnectorBuilder::new()
                .with_tls_config(ide_http::client::tls_config()?)
                .https_or_http()
                .enable_http1()
                .wrap_connector(tcp),
            _ => return Err(std::io::Error::other("Unsupported upstream proxy scheme").into()),
        };
        let mut tunnel = Tunnel::new(endpoint, connection);
        if let Some(auth) = proxy.basic_auth() {
            tunnel = tunnel.with_auth(auth.clone());
        }
        Ok(Box::new(TokioIo::new(tunnel.call(destination).await?)))
    }
}

trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Transport for T {}

fn response(status: StatusCode, body: &'static str) -> Response<reqwest::Body> {
    let mut response = Response::new(body.into());
    *response.status_mut() = status;
    response
}

fn strip_hop_headers(headers: &mut HeaderMap) {
    for value in headers.get_all(header::CONNECTION).iter().cloned().collect::<Vec<_>>() {
        if let Ok(value) = value.to_str() {
            for name in value.split(',').map(str::trim) {
                headers.remove(name);
            }
        }
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-connection",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
}
