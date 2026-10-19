#[cfg(target_os = "linux")]
#[path = "../src/documents_path.rs"]
mod documents_path;
fn main() {
    #[cfg(target_os = "linux")]
    println!(
        "{}",
        serde_json::json!({"documents": documents_path::resolve()})
    );
    #[cfg(not(target_os = "linux"))]
    println!(
        "{}",
        serde_json::json!({"unsupported": "Linux Documents authority probe"})
    );
}
