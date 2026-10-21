//! The web interface: one page plus a small JSON API.
//!
//! GET  /api/status   live timecode, levels, connections, fired events
//! GET  /api/devices  audio inputs and MIDI outputs
//! GET  /api/config   settings and events
//! PUT  /api/config   replace settings and events (validated, saved to disk, active at once)
//! POST /api/fire     {"id": ...} fires a saved event, or {"commands": [...]} sends them
//! POST /api/armed    {"armed": bool} starts or stops the cues
//! GET  /api/shows    {"current": name, "shows": [{name, modified, events}]}
//! POST /api/shows/save    {"name"?, "overwrite"?} saves the active config (to the current show)
//! POST /api/shows/load    {"name"} makes a show the active config
//! POST /api/shows/delete  {"name"}
//! POST /api/shows/get     {"name"} the show file (download)
//! POST /api/shows/upload  {"name", "config", "overwrite"?} stores a show file

use crate::config::{Command, Config};
use crate::{shows, App, EngineMsg};
use serde::Deserialize;
use std::io::Read;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tiny_http::{Header, Method, Request, Response, Server};

const INDEX: &str = include_str!("../web/index.html");

pub fn run(app: Arc<App>, listen: &str) -> Result<(), String> {
    let server = Arc::new(Server::http(listen).map_err(|e| format!("listen {listen}: {e}"))?);
    eprintln!("web interface on http://{listen}/");
    let mut workers = Vec::new();
    for _ in 0..3 {
        let (s, a) = (server.clone(), app.clone());
        workers.push(std::thread::spawn(move || {
            for req in s.incoming_requests() {
                handle(&a, req);
            }
        }));
    }
    for w in workers {
        let _ = w.join();
    }
    Ok(())
}

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

fn json(code: u16, body: String) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(body)
        .with_status_code(code)
        .with_header(header("Content-Type", "application/json"))
        .with_header(header("Cache-Control", "no-store"))
}

fn error(code: u16, msg: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    json(code, serde_json::json!({ "error": msg }).to_string())
}

#[derive(Deserialize)]
struct ArmedReq {
    armed: bool,
}

#[derive(Deserialize)]
struct ShowReq {
    name: Option<String>,
    #[serde(default)]
    overwrite: bool,
}

#[derive(Deserialize)]
struct UploadReq {
    name: String,
    config: Config,
    #[serde(default)]
    overwrite: bool,
}

fn need(name: Option<String>) -> Result<String, String> {
    name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).ok_or_else(|| "give a show name".to_string())
}

fn body<T: serde::de::DeserializeOwned>(req: &mut Request) -> Result<T, String> {
    serde_json::from_str(&read_body(req)?).map_err(|e| format!("bad request: {e}"))
}

#[derive(Deserialize)]
struct FireReq {
    id: Option<String>,
    commands: Option<Vec<Command>>,
}

fn handle(app: &Arc<App>, mut req: Request) {
    let path = req.url().split('?').next().unwrap_or("").to_string();
    let method = req.method().clone();
    let resp = match (method, path.as_str()) {
        (Method::Get, "/") | (Method::Get, "/index.html") => Response::from_string(INDEX)
            .with_header(header("Content-Type", "text/html; charset=utf-8"))
            .with_header(header("Cache-Control", "no-store")),
        (Method::Get, "/api/status") => {
            let mut s = app.status.lock().unwrap().clone();
            s.armed = app.armed.load(Ordering::SeqCst);
            s.show = app.show.lock().unwrap().clone();
            json(200, serde_json::to_string(&s).unwrap())
        }
        (Method::Get, "/api/devices") => {
            let inputs = crate::audio::list_inputs();
            let outputs = crate::midi::Midi::open().map(|m| m.ports().to_vec()).unwrap_or_default();
            json(200, serde_json::json!({ "inputs": inputs, "outputs": outputs }).to_string())
        }
        (Method::Get, "/api/config") => {
            let c = app.config.lock().unwrap().clone();
            json(200, serde_json::to_string(&c).unwrap())
        }
        (Method::Put, "/api/config") | (Method::Post, "/api/config") => {
            match body::<Config>(&mut req).and_then(|c| shows::apply(app, c)) {
                Ok(c) => json(200, serde_json::to_string(&c).unwrap()),
                Err(e) => error(400, &e),
            }
        }
        (Method::Post, "/api/armed") => match body::<ArmedReq>(&mut req) {
            Err(e) => error(400, &e),
            Ok(r) => {
                app.armed.store(r.armed, Ordering::SeqCst);
                eprintln!("cues {}", if r.armed { "active" } else { "stopped" });
                match shows::save_state(app) {
                    Ok(()) => json(200, serde_json::json!({ "armed": r.armed }).to_string()),
                    Err(e) => error(500, &format!("cues {} but not saved: {e}", if r.armed { "active" } else { "stopped" })),
                }
            }
        },
        (Method::Get, "/api/shows") => {
            let cur = app.show.lock().unwrap().clone();
            json(200, serde_json::json!({ "current": cur, "shows": shows::list(app) }).to_string())
        }
        (Method::Post, "/api/shows/save") => match body::<ShowReq>(&mut req) {
            Err(e) => error(400, &e),
            Ok(r) => {
                let exists = r.name.as_deref().is_some_and(|n| shows::list(app).iter().any(|s| s.name == n.trim()));
                let is_current = r.name.as_deref().map(str::trim) == app.show.lock().unwrap().as_deref();
                if exists && !is_current && !r.overwrite {
                    error(409, "a show with this name exists")
                } else {
                    match shows::save(app, r.name.as_deref()) {
                        Ok(name) => json(200, serde_json::json!({ "current": name }).to_string()),
                        Err(e) => error(400, &e),
                    }
                }
            }
        },
        (Method::Post, "/api/shows/load") => match body::<ShowReq>(&mut req).and_then(|r| need(r.name)).and_then(|n| shows::load(app, &n)) {
            Ok(c) => json(200, serde_json::to_string(&c).unwrap()),
            Err(e) => error(400, &e),
        },
        (Method::Post, "/api/shows/delete") => match body::<ShowReq>(&mut req).and_then(|r| need(r.name)).and_then(|n| shows::delete(app, &n)) {
            Ok(()) => json(200, "{}".into()),
            Err(e) => error(400, &e),
        },
        (Method::Post, "/api/shows/get") => match body::<ShowReq>(&mut req).and_then(|r| need(r.name)).and_then(|n| shows::file(app, &n)) {
            Ok(text) => json(200, text),
            Err(e) => error(404, &e),
        },
        (Method::Post, "/api/shows/upload") => match body::<UploadReq>(&mut req) {
            Err(e) => error(400, &e),
            Ok(mut r) => {
                let name = r.name.trim().to_string();
                if !r.overwrite && shows::list(app).iter().any(|s| s.name == name) {
                    error(409, "a show with this name exists")
                } else {
                    match r.config.validate().and_then(|()| shows::write(app, &name, &r.config)) {
                        Ok(()) => json(200, serde_json::json!({ "name": name }).to_string()),
                        Err(e) => error(400, &e),
                    }
                }
            }
        },
        (Method::Post, "/api/fire") => match read_body(&mut req).and_then(|b| {
            serde_json::from_str::<FireReq>(&b).map_err(|e| format!("bad request: {e}"))
        }) {
            Err(e) => error(400, &e),
            Ok(FireReq { id: Some(id), .. }) => {
                let _ = app.engine_tx.lock().unwrap().send(EngineMsg::Fire(id));
                json(200, "{}".into())
            }
            Ok(FireReq { commands: Some(c), .. }) => {
                let _ = app.engine_tx.lock().unwrap().send(EngineMsg::Send(c));
                json(200, "{}".into())
            }
            Ok(_) => error(400, "give id or commands"),
        },
        _ => error(404, "not found"),
    };
    let _ = req.respond(resp);
}

fn read_body(req: &mut Request) -> Result<String, String> {
    let mut s = String::new();
    req.as_reader().take(1 << 20).read_to_string(&mut s).map_err(|e| e.to_string())?;
    Ok(s)
}
