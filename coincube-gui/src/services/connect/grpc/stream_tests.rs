//! A quiet server sends no opening response until a client ping, matching
//! Connect's Go server when the account has no missed events to replay.
use super::*;
use iced::futures::{stream, Stream};
use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
};
use tonic::codegen::{http, BoxFuture, Service};

#[derive(Clone)]
struct QuietRealtime {
    respond: bool,
}

impl tonic::server::NamedService for QuietRealtime {
    const NAME: &'static str = "connect.v1.RealtimeService";
}

impl Service<http::Request<tonic::body::Body>> for QuietRealtime {
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<tonic::body::Body>) -> Self::Future {
        struct Connect(bool);
        impl tonic::server::StreamingService<StreamEnvelope> for Connect {
            type Response = StreamEnvelope;
            type ResponseStream =
                Pin<Box<dyn Stream<Item = Result<StreamEnvelope, tonic::Status>> + Send>>;
            type Future = BoxFuture<tonic::Response<Self::ResponseStream>, tonic::Status>;

            fn call(
                &mut self,
                request: tonic::Request<tonic::Streaming<StreamEnvelope>>,
            ) -> Self::Future {
                let respond = self.0;
                Box::pin(async move {
                    let mut inbound = request.into_inner();
                    let hello = inbound.message().await?.expect("ClientHello");
                    assert!(matches!(hello.body, Some(Body::ClientHello(_))));
                    let ping = inbound.message().await?.expect("opening Ping");
                    assert!(matches!(ping.body, Some(Body::Ping(_))));
                    if !respond {
                        return std::future::pending().await;
                    }
                    let pong = || StreamEnvelope {
                        body: Some(Body::Pong(Pong::default())),
                    };
                    let responses = stream::once(async move { Ok(pong()) }).chain(stream::unfold(
                        inbound,
                        move |mut inbound| async move {
                            loop {
                                match inbound.message().await {
                                    Ok(Some(StreamEnvelope {
                                        body: Some(Body::Ping(_)),
                                    })) => {
                                        return Some((Ok(pong()), inbound));
                                    }
                                    Ok(Some(_)) => {}
                                    Ok(None) => return None,
                                    Err(e) => return Some((Err(e), inbound)),
                                }
                            }
                        },
                    ));
                    Ok(tonic::Response::new(
                        Box::pin(responses) as Self::ResponseStream
                    ))
                })
            }
        }

        let respond = self.respond;
        Box::pin(async move {
            assert_eq!(request.uri().path(), "/connect.v1.RealtimeService/Connect");
            let mut grpc = tonic::server::Grpc::new(tonic::codec::ProstCodec::default());
            Ok(grpc.streaming(Connect(respond), request).await)
        })
    }
}

async fn quiet_server(respond: bool) -> (ConnectStreamConfig, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let incoming = stream::unfold(listener, |listener| async move {
        Some((listener.accept().await.map(|(socket, _)| socket), listener))
    });
    let server = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(QuietRealtime { respond })
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });
    let config = ConnectStreamConfig {
        grpc_url: format!("http://{address}"),
        tokens: Arc::new(RwLock::new(AccessTokenResponse {
            access_token: "synthetic-token".into(),
            refresh_token: "synthetic-refresh".into(),
            expires_at: i64::MAX,
        })),
        device_id: "1".into(),
        user_agent: "quiet-stream-test".into(),
        vault_ids: vec![],
        last_seen_seq: 891,
    };
    (config, server)
}

#[tokio::test]
async fn quiet_account_connects_without_replayed_events() {
    let (config, server) = quiet_server(true).await;
    let events = connect_stream(&config);
    iced::futures::pin_mut!(events);
    let first = tokio::time::timeout(Duration::from_secs(3), events.next()).await;
    server.abort();
    assert!(
        matches!(first, Ok(Some(ConnectStreamMessage::Connected))),
        "{:?}",
        first
    );
}

#[tokio::test]
async fn unresponsive_server_reports_handshake_timeout() {
    let (config, server) = quiet_server(false).await;
    let events = connect_stream(&config);
    iced::futures::pin_mut!(events);
    let first = tokio::time::timeout(CONNECT_TIMEOUT + Duration::from_secs(3), events.next()).await;
    server.abort();
    assert!(
        matches!(first, Ok(Some(ConnectStreamMessage::Error(ref error)))
        if error.contains("opening handshake timed out")),
        "{:?}",
        first
    );
}
