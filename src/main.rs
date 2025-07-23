use parking_lot::Mutex;
use rand::Rng;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    sync::Arc,
    time::Duration,
};
use tokio::{task::JoinHandle, time::sleep};
use warp::{http::StatusCode, Filter};

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PingTarget {
    url: String,
    min_delay: u64,
    max_delay: u64,
}

#[derive(Debug)]
struct AppState {
    targets: Vec<PingTarget>,
    running: bool,
    handles: Vec<JoinHandle<()>>,
    logs: Vec<String>,
}

type SharedState = Arc<Mutex<AppState>>;

const LOG_FILE_PATH: &str = "nonap.log";

#[tokio::main]
async fn main() {
    println!("🚀 NoNap microservice with control API started!");

    let initial_targets = load_targets_from_file("targets.json").unwrap_or_default();

    let state = Arc::new(Mutex::new(AppState {
        targets: initial_targets,
        running: false,
        handles: vec![],
        logs: vec![],
    }));

    // Start pinging immediately on launch
    {
        let mut locked = state.lock();
        locked.running = true;

        let client = Client::new();
        locked.handles = vec![];

        for target in locked.targets.clone() {
            let c = client.clone();
            let s = state.clone();
            let handle = tokio::spawn(async move { ping_loop(target, c, s).await });
            locked.handles.push(handle);
        }
    }

    // Clone state for warp filters
    let with_state = warp::any().map({
        let state = state.clone();
        move || state.clone()
    });

    // Routes
    let status_route = warp::path!("status")
        .and(warp::get())
        .and(with_state.clone())
        .and_then(handle_status);

    let start_route = warp::path!("start")
        .and(warp::post())
        .and(with_state.clone())
        .and_then(handle_start);

    let stop_route = warp::path!("stop")
        .and(warp::post())
        .and(with_state.clone())
        .and_then(handle_stop);

    let get_targets_route = warp::path!("targets")
        .and(warp::get())
        .and(with_state.clone())
        .and_then(handle_get_targets);

    let add_target_route = warp::path!("add-target")
        .and(warp::post())
        .and(warp::body::json())
        .and(with_state.clone())
        .and_then(handle_add_target);

    let remove_target_route = warp::path!("remove-target")
        .and(warp::post())
        .and(warp::body::json())
        .and(with_state.clone())
        .and_then(handle_remove_target);

    let logs_route = warp::path!("logs")
        .and(warp::get())
        .and(warp::query::<LogQuery>())
        .and(with_state.clone())
        .and_then(handle_logs);

    let reload_route = warp::path!("reload")
        .and(warp::post())
        .and(with_state.clone())
        .and_then(handle_reload);

    // Dashboard route (serves static html)
    let dashboard_route = warp::path::end().and(warp::get()).map(|| {
        warp::reply::html(DASHBOARD_HTML)
    });

    // Combine all routes
    let routes = status_route
        .or(start_route)
        .or(stop_route)
        .or(get_targets_route)
        .or(add_target_route)
        .or(remove_target_route)
        .or(logs_route)
        .or(reload_route)
        .or(dashboard_route)
        .with(warp::log("nonap"));

    warp::serve(routes).run(([0, 0, 0, 0], 3030)).await;
}

fn load_targets_from_file(path: &str) -> Result<Vec<PingTarget>, String> {
    match fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str(&contents).map_err(|e| e.to_string()),
        Err(e) => Err(format!("Failed to read {}: {}", path, e)),
    }
}

async fn ping_loop(target: PingTarget, client: Client, state: SharedState) {
    loop {
        {
            let locked = state.lock();
            if !locked.running {
                break;
            }
        }

        let delay = rand::thread_rng().gen_range(target.min_delay..=target.max_delay);
        let msg = format!("🛌 [NoNap] Sleeping {} minutes before pinging {}", delay, &target.url);
        println!("{}", msg);
        append_log(state.clone(), msg);

        sleep(Duration::from_secs(delay * 60)).await;

        match client.get(&target.url).send().await {
            Ok(resp) => {
                let status = resp.status();
                let msg = format!("✅ [NoNap] {} responded with status {}", &target.url, status);
                println!("{}", msg);
                append_log(state.clone(), msg);
            }
            Err(e) => {
                let msg = format!("❌ [NoNap] Failed to ping {}: {}", &target.url, e);
                eprintln!("{}", msg);
                append_log(state.clone(), msg);
            }
        }
    }
}

fn append_log(state: SharedState, message: String) {
    // Add to in-memory logs
    {
        let mut locked = state.lock();
        locked.logs.push(message.clone());
        let len = locked.logs.len();
        if len > 100 {
            locked.logs.drain(..len - 100);
        }
    }

    // Append to log file (best effort, ignore errors)
    let _ = std::thread::spawn(move || {
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(LOG_FILE_PATH)
        {
            let _ = writeln!(file, "{}", message);
        }
    });
}

// -- Handlers --

async fn handle_status(state: SharedState) -> Result<impl warp::Reply, warp::Rejection> {
    let locked = state.lock();
    let resp = serde_json::json!({
        "running": locked.running,
        "targets": locked.targets,
        "logs_count": locked.logs.len()
    });
    Ok(warp::reply::json(&resp))
}

async fn handle_start(state: SharedState) -> Result<impl warp::Reply, warp::Rejection> {
    let mut locked = state.lock();

    if locked.running {
        return Ok(warp::reply::with_status(
            "Already running",
            StatusCode::BAD_REQUEST,
        ));
    }

    locked.running = true;

    let client = Client::new();
    locked.handles = vec![];

    for target in locked.targets.clone() {
        let c = client.clone();
        let s = state.clone();
        let handle = tokio::spawn(async move { ping_loop(target, c, s).await });
        locked.handles.push(handle);
    }

    Ok(warp::reply::with_status("Started pinging", StatusCode::OK))
}

async fn handle_stop(state: SharedState) -> Result<impl warp::Reply, warp::Rejection> {
    let mut locked = state.lock();

    if !locked.running {
        return Ok(warp::reply::with_status(
            "Already stopped",
            StatusCode::BAD_REQUEST,
        ));
    }

    locked.running = false;
    locked.handles = vec![];

    Ok(warp::reply::with_status("Stopped pinging", StatusCode::OK))
}

async fn handle_get_targets(state: SharedState) -> Result<impl warp::Reply, warp::Rejection> {
    let locked = state.lock();
    Ok(warp::reply::json(&locked.targets))
}

async fn handle_add_target(
    new_target: PingTarget,
    state: SharedState,
) -> Result<impl warp::Reply, warp::Rejection> {
    let mut locked = state.lock();

    if locked.targets.iter().any(|t| t.url == new_target.url) {
        return Ok(warp::reply::with_status(
            "Target already exists",
            StatusCode::BAD_REQUEST,
        ));
    }

    locked.targets.push(new_target);

    if locked.running {
        locked.running = false;
        locked.handles = vec![];
        locked.running = true;

        let client = Client::new();
        for target in locked.targets.clone() {
            let c = client.clone();
            let s = state.clone();
            let handle = tokio::spawn(async move { ping_loop(target, c, s).await });
            locked.handles.push(handle);
        }
    }

    Ok(warp::reply::with_status("Target added", StatusCode::OK))
}

#[derive(Deserialize)]
struct RemoveTargetBody {
    url: String,
}

async fn handle_remove_target(
    body: RemoveTargetBody,
    state: SharedState,
) -> Result<impl warp::Reply, warp::Rejection> {
    let mut locked = state.lock();

    let original_len = locked.targets.len();
    locked.targets.retain(|t| t.url != body.url);

    if locked.targets.len() == original_len {
        return Ok(warp::reply::with_status(
            "Target not found",
            StatusCode::NOT_FOUND,
        ));
    }

    if locked.running {
        locked.running = false;
        locked.handles = vec![];
        locked.running = true;

        let client = Client::new();
        for target in locked.targets.clone() {
            let c = client.clone();
            let s = state.clone();
            let handle = tokio::spawn(async move { ping_loop(target, c, s).await });
            locked.handles.push(handle);
        }
    }

    Ok(warp::reply::with_status("Target removed", StatusCode::OK))
}

#[derive(Deserialize)]
struct LogQuery {
    tail: Option<usize>,
}

async fn handle_logs(
    params: LogQuery,
    state: SharedState,
) -> Result<impl warp::Reply, warp::Rejection> {
    let locked = state.lock();
    let tail = params.tail.unwrap_or(20);

    let logs: Vec<String> = if locked.logs.len() > tail {
        locked.logs[locked.logs.len() - tail..].to_vec()
    } else {
        locked.logs.clone()
    };

    Ok(warp::reply::json(&logs))
}

async fn handle_reload(state: SharedState) -> Result<impl warp::Reply, warp::Rejection> {
    match load_targets_from_file("targets.json") {
        Ok(new_targets) => {
            let mut locked = state.lock();
            locked.targets = new_targets;

            if locked.running {
                locked.running = false;
                locked.handles.clear();
                locked.running = true;

                let client = Client::new();
                for target in locked.targets.clone() {
                    let c = client.clone();
                    let s = state.clone();
                    locked.handles.push(tokio::spawn(async move {
                        ping_loop(target, c, s).await
                    }));
                }
            }

            // Build an HTML<String> reply
            let reply = warp::reply::html("Targets reloaded".to_string());
            Ok(warp::reply::with_status(reply, StatusCode::OK))
        }
        Err(e) => {
            // Also build an HTML<String> reply
            let msg = format!("Failed to reload targets: {}", e);
            let reply = warp::reply::html(msg);
            Ok(warp::reply::with_status(reply, StatusCode::INTERNAL_SERVER_ERROR))
        }
    }
}




// Dashboard HTML served at /
const DASHBOARD_HTML: &str = r#"
<!DOCTYPE html>
<html lang='en'>
<head>
<meta charset='UTF-8' />
<meta name='viewport' content='width=device-width, initial-scale=1' />
<title>NoNap Dashboard</title>
<style>
  body { font-family: Arial, sans-serif; margin: 20px; }
  h1 { color: #444; }
  #status { margin-bottom: 20px; }
  #logs { white-space: pre-wrap; background: #f0f0f0; padding: 10px; height: 300px; overflow-y: scroll; border: 1px solid #ccc; }
</style>
</head>
<body>
  <h1>NoNap Service Dashboard</h1>
  <div id="status">Loading status...</div>
  <h2>Recent Logs</h2>
  <div id="logs">Loading logs...</div>
<script>
  async function fetchStatus() {
    const res = await fetch('/status');
    if (!res.ok) {
      document.getElementById('status').textContent = 'Failed to fetch status';
      return;
    }
    const data = await res.json();
    let html = `<b>Running:</b> ${data.running}<br/>`;
    html += `<b>Targets (${data.targets.length}):</b><ul>`;
    data.targets.forEach(t => {
      html += `<li>${t.url} (delay: ${t.min_delay}-${t.max_delay} mins)</li>`;
    });
    html += '</ul>';
    html += `<b>Logs count:</b> ${data.logs_count}`;
    document.getElementById('status').innerHTML = html;
  }

  async function fetchLogs() {
    const res = await fetch('/logs?tail=20');
    if (!res.ok) {
      document.getElementById('logs').textContent = 'Failed to fetch logs';
      return;
    }
    const logs = await res.json();
    document.getElementById('logs').textContent = logs.join('\n');
  }

  async function refresh() {
    await fetchStatus();
    await fetchLogs();
  }

  refresh();
  setInterval(refresh, 5000); // Refresh every 5 seconds
</script>
</body>
</html>
"#;
