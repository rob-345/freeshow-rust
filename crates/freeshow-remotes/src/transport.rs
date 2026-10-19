struct CancelIo {
    stream: TcpStream,
    cancelled: Pin<Box<WaitForCancellationFutureOwned>>,
    _permit: OwnedSemaphorePermit,
}
impl AsyncRead for CancelIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}
impl AsyncWrite for CancelIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(io::ErrorKind::ConnectionAborted.into()));
        }
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}
async fn serve_tcp(listener: TcpListener, router: Router, run: Arc<Run>, kind: ServerKind) {
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {biased;
            _=run.cancel.cancelled()=>break,
            Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},
            accepted=listener.accept()=>{let(stream,_)=match accepted{Ok(pair)=>pair,Err(error)=>{run.diagnostic(kind,RemoteError::new("accept_failed",error.to_string()));break;}};
                let permit=match run.tcp_connections.clone().try_acquire_owned(){Ok(permit)=>permit,Err(_)=>{drop(stream);run.diagnostic(kind,RemoteError::new("connection_limit","Remote TCP connection limit reached"));continue;}};
                #[cfg(test)]
                { let bytes=run.socket_send_buffer.load(Ordering::Acquire); if bytes>0 { socket2::SockRef::from(&stream).set_send_buffer_size(bytes).unwrap(); } }
                // A native cover deadline may cancel only this HTTP connection.
                // The child still follows run cancellation, including WebSockets.
                let cancel=run.cancel.child_token();
                let router=router.clone().layer(axum::Extension(crate::cover::ConnectionAbort(cancel.clone())));
                let io=TokioIo::new(CancelIo{stream,cancelled:Box::pin(cancel.clone().cancelled_owned()),_permit:permit});
                tasks.spawn(async move{let mut builder=hyper::server::conn::http1::Builder::new();builder.timer(TokioTimer::new()).header_read_timeout(Duration::from_secs(10)).max_buf_size(16_384).max_headers(100);let service=tower::util::MapResponse::new(router,crate::cover::transport_response);let connection=builder.serve_connection(io,TowerToHyperService::new(service)).with_upgrades();tokio::select!{_=cancel.cancelled()=>{},_=connection=>{}}});
            }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}

async fn engine_request(
    run: Arc<Run>,
    kind: ServerKind,
    mut request: Request,
    next: Next,
) -> Response {
    if run.cancel.is_cancelled() {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "cancelled",
            "Remote service is stopping",
        );
    }
    let mut query = socket_query(request.uri().query().unwrap_or_default());
    // Engine.IO treats an empty string sid as an initial request. Remove it
    // before dependency dispatch, and acquire the same session lease as absent.
    if query.get("sid").and_then(Value::as_str) == Some("") {
        query.as_object_mut().unwrap().remove("sid");
    }
    // The dependency treats an unparseable raw sid as absent. Validate the
    // decoded Engine.IO reserved fields before canonicalizing them, so encoded,
    // duplicate or invalid sid parameters cannot create an unleased session.
    let transport = query.get("transport").and_then(Value::as_str);
    if query.get("EIO").and_then(Value::as_str) != Some("4")
        || !matches!(transport, Some("polling" | "websocket"))
    {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_engine_query",
            "Unsupported Engine.IO transport or protocol",
        );
    }
    if let Some(sid) = query.get("sid") {
        if !sid
            .as_str()
            .is_some_and(|sid| sid.parse::<socketioxide::socket::Sid>().is_ok())
        {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_engine_sid",
                "Invalid Engine.IO session ID",
            );
        }
    }
    let initial = request.method() == Method::GET && query.get("sid").is_none();
    let polling = transport == Some("polling");
    let canonical = {
        let mut encoded = form_urlencoded::Serializer::new(String::new());
        if let Some(values) = query.as_object() {
            for (key, value) in values {
                match value {
                    Value::Array(values) => {
                        for value in values {
                            if let Some(value) = value.as_str() {
                                encoded.append_pair(key, value);
                            }
                        }
                    }
                    Value::String(value) => {
                        encoded.append_pair(key, value);
                    }
                    _ => {}
                }
            }
        }
        format!("{}?{}", request.uri().path(), encoded.finish())
    };
    *request.uri_mut() = canonical.parse().unwrap();
    let lease = if initial {
        let permit = match run.engine_sessions.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "session_limit",
                    "Remote Engine.IO session limit reached",
                )
            }
        };
        let lease = SessionLease(Arc::new(Lease {
            _permit: permit,
            sid: StdMutex::new(None),
            sids: Arc::downgrade(&run.polling_sids),
        }));
        request.extensions_mut().insert(lease.clone());
        Some(lease)
    } else {
        None
    };
    let response = if initial {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let task_run = run.clone();
        if run
            .spawn(async move {
                let mut response = next.run(request).await;
                #[cfg(test)]
                {
                    let pause = task_run.handshake_pause.lock().unwrap().take();
                    if let Some((entered, release)) = pause {
                        entered.notify_one();
                        tokio::select! {_=release.notified()=>{},_=task_run.cancel.cancelled()=>{}};
                    }
                }
                if polling && response.status() == StatusCode::OK {
                    let (parts, body) = response.into_parts();
                    response = match to_bytes(body, 2048).await {
                        Ok(bytes) => {
                            if let Some(sid) = bytes
                                .first()
                                .filter(|b| **b == b'0')
                                .and_then(|_| serde_json::from_slice::<Value>(&bytes[1..]).ok())
                                .and_then(|v| {
                                    v.get("sid").and_then(Value::as_str).map(str::to_owned)
                                })
                            {
                                if let Some(lease) = lease {
                                    let tagged = format!("{}:{sid}", kind.channel());
                                    *lease.0.sid.lock().unwrap() = Some(tagged.clone());
                                    task_run.polling_sids.lock().unwrap().insert(tagged);
                                }
                            }
                            Response::from_parts(parts, Body::from(bytes))
                        }
                        Err(_) => error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "handshake_failed",
                            "Engine.IO handshake response exceeds its limit",
                        ),
                    };
                }
                let _ = sender.send(response);
            })
            .is_err()
        {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "cancelled",
                "Remote handshake admission is closed",
            );
        }
        match receiver.await {
            Ok(response) => response,
            Err(_) => {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "cancelled",
                    "Remote handshake was cancelled",
                )
            }
        }
    } else {
        next.run(request).await
    };
    response
}
