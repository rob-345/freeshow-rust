//! Readonly native OS queries stay outside ordered library filesystem work.
use freeshow_core::{
    protocol::{Event, Request},
    service::LibraryService,
    WireError,
};
use freeshow_system::{JobControl, SystemService};
use serde_json::json;

pub const COMMANDS: &[&str] = &["GET_OS", "GET_DEVICE_NAME", "CHECK_RAM_USAGE"];

fn service_error(error: freeshow_system::Error) -> WireError {
    let code = match error.code.as_str() {
        "system_busy" => "system_busy",
        "system_shutdown" => "system_shutdown",
        "system_cancelled" => "system_cancelled",
        "system_deadline" => "system_deadline",
        "system_unsupported" => "system_unsupported",
        "system_encoding" => "system_encoding",
        "system_os" => "system_os",
        "system_invalid" => "system_invalid",
        _ => "system_worker",
    };
    WireError::new(code, error.message)
}

pub async fn dispatch(
    library: &LibraryService,
    system: &SystemService,
    request: Request,
) -> Result<Vec<Event>, WireError> {
    library
        .contextualize(&request, &mut [])
        .map_err(WireError::from)?;
    let command = request.data["channel"].as_str().unwrap_or("");
    let data = match command {
        "GET_OS" => json!(system
            .get_os(JobControl::default())
            .await
            .map_err(service_error)?),
        "GET_DEVICE_NAME" => json!(system
            .get_device_name(JobControl::default())
            .await
            .map_err(service_error)?),
        "CHECK_RAM_USAGE" => json!(system
            .ram_usage(JobControl::default())
            .await
            .map_err(service_error)?),
        _ => {
            return Err(WireError::new(
                "unsupported_command",
                "Unknown native system query",
            ))
        }
    };
    let mut events = vec![Event::reply(
        "MAIN",
        command,
        data,
        request.listener_id.clone(),
    )];
    library
        .contextualize(&request, &mut events)
        .map_err(WireError::from)?;
    Ok(events)
}
