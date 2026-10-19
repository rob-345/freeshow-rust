//! Native CLI-only inert fixture source. No application/renderer path authority.
//! The file and Node-observed mtime are frozen before the listener starts.
use freeshow_remotes::{
    CoverControl, CoverErrorRendering, CoverFileStat, CoverFuture, CoverSource, CoverState,
    PreparedCover, RemoteConfig, RemoteError, RemoteService, ServerKind,
};
use serde_json::json;
use std::{fs::File, io, net::Ipv4Addr, path::PathBuf, sync::Arc};
use tokio::io::AsyncBufReadExt;

struct Source {
    file: Option<Arc<File>>,
    state: CoverState,
    production: bool,
}
struct Prepared {
    file: Option<Arc<File>>,
    state: CoverState,
    control: CoverControl,
}
impl CoverSource for Source {
    fn error_rendering(&self) -> CoverErrorRendering {
        if self.production {
            CoverErrorRendering::Production
        } else {
            CoverErrorRendering::Deferred
        }
    }
    fn prepare(&self, control: CoverControl) -> CoverFuture<'_, Box<dyn PreparedCover>> {
        let file = self.file.clone();
        let state = self.state;
        Box::pin(async move {
            Ok(Box::new(Prepared {
                file,
                state,
                control,
            }) as Box<dyn PreparedCover>)
        })
    }
}
impl PreparedCover for Prepared {
    fn state(&self) -> CoverState {
        self.state
    }
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        assert!(buffer.len() <= 32768);
        let file = self.file.as_ref().expect("file plan owns descriptor");
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            file.read_at(buffer, offset)
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            file.seek_read(buffer, offset)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (file, offset, buffer);
            Err(io::ErrorKind::Unsupported.into())
        }
    }
    fn before_headers(&mut self) -> CoverFuture<'_, ()> {
        Box::pin(async move {
            if self.control.cancel.is_cancelled() {
                Err(RemoteError {
                    code: "fixture_cancelled",
                    message: "Native fixture was cancelled".into(),
                })
            } else {
                Ok(())
            }
        })
    }
    fn finish(self: Box<Self>) -> CoverFuture<'static, ()> {
        Box::pin(async move {
            drop(self);
            Ok(())
        })
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).take(6).collect();
    if args.len() != 5 || args.iter().any(|v| v.len() > 32768 || v.contains('\0')) {
        return Err(
            "native fixture requires mode,file,mtime,environment,static-root arguments".into(),
        );
    }
    let (file, state) = match args[0].as_str() {
        "file" => {
            let file = Arc::new(File::open(&args[1])?);
            let stat = CoverFileStat::new(file.metadata()?.len(), args[2].parse::<f64>()?)?;
            (Some(file), CoverState::File(stat))
        }
        "missing" => (None, CoverState::Missing),
        "failed" => (None, CoverState::Failed),
        "unconfigured" => (None, CoverState::Missing),
        _ => return Err("unknown native fixture mode".into()),
    };
    let mut config = RemoteConfig {
        bind_ip: Ipv4Addr::LOCALHOST.into(),
        ..RemoteConfig::default()
    };
    for kind in ServerKind::ALL {
        config.static_roots.insert(kind, PathBuf::from(&args[4]));
    }
    let (service, _events) = RemoteService::new(config)?;
    let source = if args[0] == "unconfigured" {
        None
    } else {
        Some(Arc::new(Source {
            file,
            state,
            production: args[3] == "production",
        }) as Arc<dyn CoverSource>)
    };
    let payload = json!({"ports":{"remote":"0","stage":"0","controller":"0"},"disabled":{"remote":false,"stage":false,"controller":false},"data":{}});
    let bound = service.start_with_cover(&payload, 1, source).await?;
    println!("{}", serde_json::to_string(&bound)?);
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    if let Some(line) = lines.next_line().await? {
        if line != "stop" {
            return Err("only native stop fixture control is supported".into());
        }
    }
    service.shutdown().await?;
    println!("{{\"joined\":true}}");
    Ok(())
}
