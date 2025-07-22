use parking_lot::Mutex;
use rand::Rng;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{fs, sync::Arc, time::Duration};
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

    // Combine routes
    let routes = status_route
        .or(start_route)
        .or(stop_route)
        .or(get_targets_route)
        .or(add_target_route)
        .or(remove_target_route)
        .or(logs_route)
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
                // If stopped, break loop
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
    let mut locked = state.lock();
    locked.logs.push(message);
    // Keep logs trimmed to last 100 entries
    let len = locked.logs.len();
    if len > 100 {
        locked.logs.drain(..len - 100);
    }
}

// Handlers

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

    // Handles will exit naturally on next loop check
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

    // If running, restart ping loops to include new target
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

    // If running, restart ping loops without removed target
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
