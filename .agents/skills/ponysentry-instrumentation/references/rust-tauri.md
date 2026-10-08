# Rust / Tauri 埋点模板

PonySentry Ingest 上报（Rust 原生，无需额外 SDK 依赖，纯 `reqwest` + `serde_json`）。

## 上报客户端（共享模块）

在项目内新增 `src/telemetry.rs`（或等价模块），供各端复用：

```rust
// src/telemetry.rs
use serde::Serialize;
use std::collections::HashMap;

const INGEST_URL: &str = env!("PONYSENTRY_INGEST_URL", "https://sentry.ponyjob.top");
// 若服务端配置了 CLIENT_TOKEN，必须设置 PONYSENTRY_CLIENT_TOKEN 环境变量

#[derive(Serialize)]
struct Frame {
    filename: Option<String>,
    function: Option<String>,
    lineno: Option<u32>,
    in_app: Option<bool>,
}

#[derive(Serialize)]
struct Exception {
    error_type: String,
    value: Option<String>,
    stacktrace: Option<Vec<Frame>>,
}

#[derive(Serialize)]
struct Breadcrumb {
    category: String,
    message: String,
    data: Option<HashMap<String, String>>,
}

#[derive(Serialize)]
struct IngestPayload {
    platform: String,          // "rust" | "tauri"
    release: String,           // 必须与部署版本一致（回归检测依赖）
    environment: String,       // "production" | "staging" | "dev"
    message: Option<String>,
    exception: Option<Exception>,
    tags: Option<HashMap<String, String>>,
    extra: Option<serde_json::Value>,
    breadcrumbs: Option<Vec<Breadcrumb>>,
}

// 零信任脱敏：与服务端 sanitizer 对齐，客户端先脱敏一层
pub fn sanitize(input: &str) -> String {
    let mut s = input.to_string();
    // 1. 常见绝对路径用户名（按平台）
    s = s.replace("/home/", "[USER_HOME]/")
         .replace("/Users/", "[USER_HOME]/")
         .replace(r"C:\Users\", r"[USER_HOME]\");
    // 2. Token / 密码键值对
    for key in ["token", "password", "secret", "api_key", "authorization"] {
        s = regex_replace_sensitive(&s, key);
    }
    s
}

fn regex_replace_sensitive(text: &str, key: &str) -> String {
    // 匹配 "key": "value" / key=value / Bearer xxx，替换 value 为 [REDACTED_SECRET]
    // 具体实现可复用 crates/ingest/src/sanitizer.rs 的正则，或引入 regex crate：
    //   Regex::new(&format!(r#"(?i)({key}\s*[:=]\s*["']?[^\s,"']{{4,}})"#)).unwrap()
    //       .replace_all(text, &format!(r#"${{1}}[REDACTED]"#))
    text.to_string()
}

// 上报入口：fire-and-forget，不阻塞业务
pub async fn report(payload: IngestPayload) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3)) // 外部调用硬超时 ≤3s
        .build()
        .map_err(|e| e.to_string())?;

    let mut req = client
        .post(format!("{INGEST_URL}/api/v1/ingest"))
        .header("Content-Type", "application/json");

    // 若配置了 CLIENT_TOKEN，附加鉴权头
    if let Ok(token) = std::env::var("PONYSENTRY_CLIENT_TOKEN") {
        if !token.is_empty() {
            req = req.header("X-Client-Token", token);
        }
    }

    let resp = req
        .json(&payload)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("ingest failed: HTTP {}", resp.status()));
    }
    Ok(())
}
```

## Rust 库 / CLI：panic hook + 主函数包裹

```rust
// src/main.rs
use std::panic;

fn install_panic_hook() {
    panic::set_hook(Box::new(|info| {
        let location = info.location().map(|l| l.to_string());
        let message = info.payload().downcast_ref::<String>()
            .cloned().unwrap_or_else(|| {
                info.payload().downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "unknown panic".into())
            });

        let payload = telemetry::IngestPayload {
            platform: "rust".into(),
            release: env!("CARGO_PKG_VERSION").into(),
            environment: std::env::var("APP_ENV").unwrap_or_else(|_| "dev".into()),
            message: Some(telemetry::sanitize(&message)),
            exception: Some(telemetry::Exception {
                error_type: "Panic".into(),
                value: Some(telemetry::sanitize(&message)),
                stacktrace: Some(vec![telemetry::Frame {
                    filename: location,
                    function: Some("panic_hook".into()),
                    lineno: None,
                    in_app: Some(true),
                }]),
            }),
            tags: None,
            extra: None,
            breadcrumbs: telemetry::take_breadcrumbs(), // 全局面包屑队列
        };

        // fire-and-forget：spawn 上报，不阻塞 panic 流程
        tokio::spawn(telemetry::report(payload));
    }));
}

fn main() {
    install_panic_hook();
    // ... 业务逻辑
}
```

## Tauri 桌面端

```rust
// src-tauri/src/main.rs
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 1. 全局 panic hook（崩溃捕获，含 Windows）
    std::panic::set_hook(Box::new(|info| {
        // ... 同 Rust 模板，platform 填 "tauri"，release 填 app 版本（读 tauri.conf.json）
        // 关键：Windows 端崩溃（panic/segfault）都要经过这里或窗口级错误捕获
    }));

    // 2. IPC 调用错误上报：命令失败时自动上报
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![save_settings])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[tauri::command]
async fn save_settings(settings: String) -> Result<(), String> {
    // 业务逻辑 ...
    // 失败路径：
    //   telemetry::report(payload with exception IpcError + breadcrumbs)  // 不上报成功
    Ok(())
}
```

## Tauri：面包屑采集

```rust
// 在窗口/事件层采集用户轨迹
use std::sync::Mutex;
use once_cell::sync::Lazy;

static BREADCRUMBS: Lazy<Mutex<Vec<telemetry::Breadcrumb>>> =
    Lazy::new(|| Mutex::new(Vec::with_capacity(64)));

pub fn add_breadcrumb(category: &str, message: &str, data: Option<HashMap<String, String>>) {
    let mut queue = BREADCRUMBS.lock().unwrap();
    if queue.len() >= 64 { queue.remove(0); } // 环形缓冲上限
    queue.push(telemetry::Breadcrumb {
        category: category.into(),
        message: telemetry::sanitize(message),
        data: data.map(|d| d.into_iter().map(|(k, v)| (k, telemetry::sanitize(&v))).collect()),
    });
}
```

## 校准样例

- 正例：`main.rs` 主逻辑 panic → panic hook 自动上报，platform=`rust`，release=版本。
- 反例：panic hook 里再 panic（上报逻辑崩溃）→ 上报必须 fire-and-forget 且自身不抛异常。
- 正例：Tauri IPC 命令失败 → 上报 `IpcError` + breadcrumb（category=`ipc`，message=命令名）。
- 反例：每次 IPC 成功也上报 → 只报错误，不报成功（去噪）。
