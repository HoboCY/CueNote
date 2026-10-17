//! Fixed, read-only observations for the native macOS performance driver.
//! No arbitrary JavaScript, input actions, or business IPC are exposed here.
use std::{
    fs::OpenOptions,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::OpenOptionsExt,
    sync::mpsc,
    time::Duration,
};

use block2::RcBlock;
use objc2::{rc::Retained, runtime::AnyObject, MainThreadMarker};
use objc2_app_kit::{NSScreen, NSView};
use objc2_foundation::{NSError, NSProcessInfo, NSString};
use objc2_web_kit::{WKContentWorld, WKWebView};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tauri::Manager;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    token: String,
    operation: Operation,
    #[serde(default)]
    paint: bool,
    selector: Option<String>,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
enum Operation {
    Snapshot,
    Element,
}

fn geometry(webview: &WKWebView) -> Result<Value, String> {
    let mtm = MainThreadMarker::new().ok_or("Observation must run on the main thread")?;
    let view: &NSView = webview;
    let window = view.window().ok_or("WebView has no window")?;
    if view.frameRotation() != 0.0 || view.boundsRotation() != 0.0 {
        return Err("Rotated WebViews are unsupported by the native driver".into());
    }
    let rect = window.convertRectToScreen(view.convertRect_toView(view.bounds(), None));
    let screens = NSScreen::screens(mtm);
    let primary = screens.firstObject().ok_or("No primary screen")?.frame();
    Ok(json!({
        "x": rect.origin.x,
        "y": primary.origin.y + primary.size.height - (rect.origin.y + rect.size.height),
        "width": rect.size.width, "height": rect.size.height,
        "windowNumber": window.windowNumber(), "key": window.isKeyWindow(),
    }))
}

fn observe(app: &tauri::AppHandle, request: &Request) -> Result<Value, String> {
    let window = app
        .get_webview_window("main")
        .ok_or("Main window not ready")?;
    let payload = json!({"operation": request.operation, "paint": request.paint, "selector": request.selector});
    // JSON serialization keeps selector data out of executable source syntax.
    let script = format!(
        "const request = {payload};\n{}",
        include_str!("../../../tools/performance/macos/observe.js")
    );
    let (sender, receiver) = mpsc::sync_channel(1);
    window
        .with_webview(move |platform| unsafe {
            // Tauri guarantees main-thread execution and a live WKWebView here.
            let webview = &*(platform.inner() as *const WKWebView);
            let held =
                Retained::retain(platform.inner() as *mut WKWebView).expect("live WKWebView");
            let callback = RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
                let result = (|| {
                    if let Some(error) = error.as_ref() {
                        return Err(error.localizedDescription().to_string());
                    }
                    let text = value
                        .as_ref()
                        .and_then(|v| v.downcast_ref::<NSString>())
                        .ok_or("WebKit did not return a JSON string")?;
                    let text = text.to_string();
                    if text.len() > 1024 * 1024 {
                        return Err("Observation exceeded the response budget".into());
                    }
                    let state: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                    Ok(json!({"value":state,"screen":geometry(&held)?,"pid":std::process::id()}))
                })();
                // Never block the main thread if the external driver timed out.
                let _ = sender.try_send(result);
            });
            // Unlike evaluateJavaScript, this API waits for the async function and
            // both requested animation frames before invoking the completion block.
            let world =
                WKContentWorld::defaultClientWorld(MainThreadMarker::new().expect("main thread"));
            webview.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(
                &NSString::from_str(&script),
                None,
                None,
                &world,
                Some(&callback),
            );
        })
        .map_err(|e| e.to_string())?;
    receiver
        .recv_timeout(Duration::from_secs(30))
        .map_err(|e| e.to_string())?
}

fn serve(
    app: &tauri::AppHandle,
    stream: &mut TcpStream,
    token: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut bytes = Vec::new();
    BufReader::new((&mut *stream).take(4097)).read_until(b'\n', &mut bytes)?;
    let result = (|| -> Result<Value, String> {
        if bytes.len() > 4096 || bytes.last() != Some(&b'\n') {
            return Err("Invalid request size/framing".into());
        }
        let request: Request =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid observation request")?;
        if request.token != token {
            return Err("Unauthorized observation".into());
        }
        match request.operation {
            Operation::Snapshot if request.selector.is_some() => {
                return Err("Snapshot has no selector".into())
            }
            Operation::Element
                if request
                    .selector
                    .as_ref()
                    .is_none_or(|s| s.is_empty() || s.len() > 512) =>
            {
                return Err("Invalid selector length".into())
            }
            _ => {}
        }
        observe(app, &request)
    })();
    let response = match result {
        Ok(value) => value,
        Err(error) => json!({"error":error,"pid":std::process::id()}),
    };
    serde_json::to_writer(&mut *stream, &response)?;
    stream.write_all(b"\n")?;
    Ok(())
}

pub(crate) fn attach(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    if NSProcessInfo::processInfo()
        .operatingSystemVersion()
        .majorVersion
        < 14
    {
        return Err("Performance profile isolation requires macOS 14 or newer".into());
    }
    let token = std::env::var("CUENOTE_PERF_OBSERVER_TOKEN")?;
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("A fresh 256-bit observation token is required".into());
    }
    let root = super::root()?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    let digest = Sha256::digest(root.to_string_lossy().as_bytes());
    let mut store = [0_u8; 16];
    store.copy_from_slice(&digest[..16]);
    let endpoint = json!({"pid":std::process::id(),"port":listener.local_addr()?.port(),
        "webKitDataStoreIdentifier":uuid::Uuid::from_bytes(store).to_string(),
        "webKitStorage":"OS-managed isolated WKWebsiteDataStore; not the root/webview directory"});
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join(format!("observer-{}.json", std::process::id())))?;
    serde_json::to_writer(file, &endpoint)?;
    let handle = app.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(mut stream) => {
                    let _ = serve(&handle, &mut stream, &token);
                }
                Err(_) => break,
            }
        }
    });
    Ok(())
}
