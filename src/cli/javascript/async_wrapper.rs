use std::sync::Arc;

use hyper::body::Bytes;
use serde_json::json;
use tokio::{
    runtime::Builder,
    spawn,
    sync::{mpsc, oneshot},
    task::{spawn_local, LocalSet},
};

use crate::{
    blueprint,
    channel::{JsRequest, JsResponse},
    http::Response,
    HttpIO,
};

use super::worker::Worker;

pub type ChannelResult = anyhow::Result<Response<hyper::body::Bytes>>;
pub type ChannelMessage = (oneshot::Sender<ChannelResult>, reqwest::Request);

pub type FetchResult = anyhow::Result<JsResponse>;
pub type FetchMessage = (oneshot::Sender<FetchResult>, JsRequest);

#[derive(Debug, Clone)]
pub struct JsTokioWrapper {
    sender: mpsc::UnboundedSender<ChannelMessage>,
}

impl JsTokioWrapper {
    pub fn new(script: blueprint::Script, http: impl HttpIO) -> Self {
        let (sender, mut receiver) = mpsc::unbounded_channel::<ChannelMessage>();
        let (http_sender, mut http_receiver) = mpsc::unbounded_channel::<FetchMessage>();
        let http = Arc::new(http);

        spawn(async move {
            while let Some((send_response, request)) = http_receiver.recv().await {
                let http = http.clone();

                spawn(async move {
                    let result = http.execute(request.try_into().unwrap()).await;
                    let response = result.and_then(|response| JsResponse::try_from(&response));

                    send_response.send(response).unwrap();
                });
            }
        });

        std::thread::spawn(move || {
            let rt = Builder::new_current_thread().build().unwrap();
            let local = LocalSet::new();

            local.spawn_local(async move {
                let worker = Worker::new(script, http_sender).unwrap();

                while let Some((response, request)) = receiver.recv().await {
                    let worker = worker.clone();
                    spawn_local(async move {
                        let result = worker.on_event(request).await;

                        // ignore errors
                        let _ = response.send(result);
                    });
                }
            });

            rt.block_on(local);
        });

        Self { sender }
    }
}

#[async_trait::async_trait]
impl HttpIO for JsTokioWrapper {
    async fn execute(
        &self,
        request: reqwest::Request,
    ) -> anyhow::Result<Response<hyper::body::Bytes>> {
        let (tx, rx) = oneshot::channel();

        self.sender.send((tx, request))?;

        rx.await?
    }
}
