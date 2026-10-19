use crate::*;
use ::http::{Request, Uri};
use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper_util::rt::TokioIo;
use std::{future::Future, sync::Arc};
use tokio::io::{AsyncRead, AsyncWrite};
pub(crate) fn tls() -> Result<Arc<rustls::ClientConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| Error::new("download_tls", "TLS provider unavailable"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}
trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
async fn controlled<T>(job: &service::Job, future: impl Future<Output = Result<T>>) -> Result<T> {
    job.control.check()?;
    let idle = Instant::now() + job.config.idle_timeout;
    tokio::select! {biased;
        _=job.control.service.cancelled()=>Err(Error::shutdown()),
        _=job.control.caller.cancelled()=>Err(Error::cancelled()),
        _=job.control.cancel.cancelled()=>Err(Error::cancelled()),
        _=tokio::time::sleep_until(job.control.deadline.into())=>Err(Error::new("download_deadline","FFmpeg download deadline elapsed")),
        _=tokio::time::sleep_until(idle.into())=>Err(Error::new("download_idle_timeout","FFmpeg download made no progress before timeout")),
        result=future=>result,
    }
}
fn policy(uri: &Uri, fixture: bool) -> Result<()> {
    if uri.to_string().len() > 8192 || uri.authority().is_none_or(|v| v.as_str().contains('@')) {
        return Err(Error::new("download_url", "Invalid download URL"));
    }
    if fixture {
        ControlledTestConfig::loopback(&uri.to_string()).map(|_| ())
    } else if uri.scheme_str() == Some("https") {
        Ok(())
    } else {
        Err(Error::new(
            "download_redirect",
            "Production download requires HTTPS",
        ))
    }
}
fn redirect(base: &Uri, location: &str) -> Result<Uri> {
    if location.len() > 8192 {
        return Err(Error::limit());
    }
    let location = location.split('#').next().unwrap_or("");
    let absolute = if location.starts_with("https://") || location.starts_with("http://") {
        location.into()
    } else if location.starts_with("//") {
        format!("{}:{location}", base.scheme_str().unwrap())
    } else {
        let path = if location.is_empty() {
            base.path_and_query()
                .map(|v| v.as_str())
                .unwrap_or("/")
                .to_owned()
        } else if location.starts_with('?') {
            format!("{}{location}", base.path())
        } else if location.starts_with('/') {
            location.into()
        } else {
            format!(
                "{}{location}",
                &base.path()[..base.path().rfind('/').map_or(0, |i| i + 1)]
            )
        };
        let (path, query) = path
            .split_once('?')
            .map_or((path.as_str(), None), |(p, q)| (p, Some(q)));
        let trailing = path.ends_with('/');
        let mut parts = Vec::new();
        for part in path.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                _ => parts.push(part),
            }
        }
        let mut path = format!("/{}", parts.join("/"));
        if trailing && !path.ends_with('/') {
            path.push('/');
        }
        if let Some(query) = query {
            path.push('?');
            path.push_str(query);
        }
        format!(
            "{}://{}{path}",
            base.scheme_str().unwrap(),
            base.authority().unwrap()
        )
    };
    absolute
        .parse()
        .map_err(|_| Error::new("download_redirect", "Unsupported redirect URI"))
}
pub(crate) async fn transfer(
    job: &Arc<service::Job>,
    tls: Arc<rustls::ClientConfig>,
) -> Result<TransferStats> {
    job.authorize().await?;
    job.control.check()?;
    let fixture = job.config.controlled_test.is_some();
    let initial = job
        .config
        .controlled_test
        .as_ref()
        .map_or(job.plan.url, |v| v.url.as_str());
    let mut uri: Uri = initial
        .parse()
        .map_err(|_| Error::new("download_url", "Invalid host download URL"))?;
    for redirects in 0..=job.config.redirects {
        policy(&uri, fixture)?;
        job.control.check()?;
        let host = uri
            .host()
            .ok_or_else(|| Error::new("download_url", "Download host missing"))?
            .trim_matches(['[', ']'])
            .to_string();
        let port = uri
            .port_u16()
            .unwrap_or(if uri.scheme_str() == Some("https") {
                443
            } else {
                80
            });
        let addresses = job.resolve(host.clone(), port).await?;
        job.control.check()?;
        let mut stream = None;
        for address in addresses {
            let connect = controlled(job, async {
                tokio::net::TcpStream::connect(address)
                    .await
                    .map_err(|_| Error::source("fetch failed"))
            })
            .await;
            match connect {
                Ok(socket) => {
                    stream = Some(socket);
                    break;
                }
                Err(error) if error.code == "download_source" => {}
                Err(error) => return Err(error),
            }
        }
        let stream = stream.ok_or_else(|| Error::source("fetch failed"))?;
        let io: Box<dyn Io> = if uri.scheme_str() == Some("https") {
            let name = rustls::pki_types::ServerName::try_from(host.clone())
                .map_err(|_| Error::new("download_tls", "Invalid TLS server name"))?;
            Box::new(
                controlled(job, async {
                    tokio_rustls::TlsConnector::from(tls.clone())
                        .connect(name, stream)
                        .await
                        .map_err(|_| Error::source("fetch failed"))
                })
                .await?,
            )
        } else {
            Box::new(stream)
        };
        let mut builder = hyper::client::conn::http1::Builder::new();
        builder.max_headers(100).max_buf_size(65536);
        let (mut sender, connection) = controlled(job, async {
            builder
                .handshake::<_, Empty<Bytes>>(TokioIo::new(io))
                .await
                .map_err(|_| Error::source("fetch failed"))
        })
        .await?;
        let mut connection = Box::pin(connection);
        let request = Request::builder()
            .method("GET")
            .uri(uri.path_and_query().map(|p| p.as_str()).unwrap_or("/"))
            .header("Host", uri.authority().unwrap().as_str())
            .header("Accept", "*/*")
            .header("Accept-Encoding", "identity")
            .header("Connection", "close")
            .body(Empty::<Bytes>::new())
            .map_err(|_| Error::new("download_request", "Invalid download request"))?;
        let mut connection_done = false;
        let response = controlled(job, async {
            let response = sender.send_request(request);
            tokio::pin!(response);
            loop {
                tokio::select! { biased;
                    result = &mut response => return result.map_err(|_|Error::source("fetch failed")),
                    _ = &mut connection, if !connection_done => { connection_done = true; },
                }
            }
        }).await?;
        let status = response.status().as_u16();
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            if let Some(location) = response.headers().get("location") {
                if redirects == job.config.redirects {
                    return Err(Error::source("fetch failed"));
                }
                uri = redirect(
                    &uri,
                    location.to_str().map_err(|_| {
                        Error::new("download_redirect", "Unsupported redirect header")
                    })?,
                )?;
                continue;
            }
        }
        if !(200..300).contains(&status) || matches!(status, 204 | 205) {
            return Err(Error::source(&format!("Download failed: {status}")));
        }
        if response
            .headers()
            .get("content-encoding")
            .is_some_and(|v| v.as_bytes() != b"identity")
        {
            return Err(Error::new(
                "download_content_encoding",
                "HTTP Content-Encoding is unsupported for this archive transport",
            ));
        }
        let total = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        if total > job.config.compressed_bytes as f64 {
            return Err(Error::limit());
        }
        *locked(&job.phase) = service::Phase::Streaming;
        let (pipe, receive) = tokio::sync::mpsc::channel::<Vec<u8>>(2);
        let archive = job.archive(receive).await?;
        let mut body = response.into_body();
        let mut stats = TransferStats {
            redirects,
            ..TransferStats::default()
        };
        let mut previous = None;
        let stream_result:Result<()> = async{
            loop{
                let frame=controlled(job,async{
                    loop{tokio::select!{biased;
                        frame=body.frame()=>return frame.transpose().map_err(|_|Error::source("fetch failed")),
                        result=&mut connection,if !connection_done=>{connection_done=true;if result.is_err(){return Err(Error::source("fetch failed"));}},
                    }}
                }).await?;
                let Some(frame)=frame else{break;};let Ok(bytes)=frame.into_data() else{continue;};
                if bytes.len() as u64>job.config.compressed_bytes.saturating_sub(stats.compressed_bytes){return Err(Error::limit());}stats.compressed_bytes+=bytes.len() as u64;
                if let Some(progress)=progress_for_bytes(stats.compressed_bytes,total){if previous!=Some(progress.progress)&&stats.progress_updates<256{previous=Some(progress.progress);job.progress.send_replace(Some(progress));stats.progress_updates+=1;}}
                for chunk in bytes.chunks(CHUNK){controlled(job,async{pipe.send(chunk.to_vec()).await.map_err(|_|Error::new("download_pipe_closed","Archive worker ended before the HTTP stream"))}).await?;}
            }Ok(())
        }.await;
        drop(pipe);
        drop(body);
        drop(sender);
        drop(connection);
        // Always drain actual gzip/file work before returning or releasing a
        // full-pipeline lease, even after network cancellation or caller drop.
        let decoded = archive
            .await
            .map_err(|_| Error::new("download_worker", "Archive worker stopped"))?;
        if let Err(error) = stream_result {
            if error.code == "download_pipe_closed" {
                return decoded.and(Err(error));
            }
            return Err(error);
        }
        let decoded = decoded?;
        stats.uncompressed_bytes = decoded.uncompressed_bytes;
        stats.members = decoded.members;
        *locked(&job.stats) = stats.clone();
        job.control.check()?;
        return Ok(stats);
    }
    Err(Error::source("fetch failed"))
}
