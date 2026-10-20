//! The web interface: one page plus a small JSON API.
//!
//! GET  /api/status   live timecode, levels, connections, fired events
//! GET  /api/devices  audio inputs and MIDI outputs
//! GET  /api/config   settings and events
//! PUT  /api/config   replace settings and events (validated, saved to disk)
//! POST /api/fire     {"id": ...} fires a saved event, or {"commands": [...]} sends them

use crate::config::{Command, Config};
use crate::{App, EngineMsg};
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
            let s = app.status.lock().unwrap().clone();
            json(200, serde_json::to_string(&s).unwrap())
        }
        (Method::Get, "/api/devices") => {
            let inputs = crate::audio::list_inputs();
            let outputs = crate::midi::Midi::open().map(|m| m.ports()).unwrap_or_default();
            json(200, serde_json::json!({ "inputs": inputs, "outputs": outputs }).to_string())
        }
        (Method::Get, "/api/config") => {
            let c = app.config.lock().unwrap().clone();
            json(200, serde_json::to_string(&c).unwrap())
        }
        (Method::Put, "/api/config") | (Method::Post, "/api/config") => match read_body(&mut req) {
            Err(e) => error(400, &e),
            Ok(body) => match serde_json::from_str::<Config>(&body) {
                Err(e) => error(400, &format!("bad config: {e}")),
                Ok(mut c) => match c.validate() {
                    Err(e) => error(400, &e),
                    Ok(()) => match c.save(&app.config_path) {
                        Err(e) => error(500, &format!("save: {e}")),
                        Ok(()) => {
                            let audio_changed = {
                                let mut cur = app.config.lock().unwrap();
                                let changed = cur.audio_device != c.audio_device
                                    || cur.audio_channel != c.audio_channel
                                    || cur.sample_rate != c.sample_rate;
                                *cur = c.clone();
                                changed
                            };
                            if audio_changed {
                                app.audio_gen.fetch_add(1, Ordering::SeqCst);
                            }
                            let _ = app.engine_tx.lock().unwrap().send(EngineMsg::ConfigChanged);
                            json(200, serde_json::to_string(&c).unwrap())
                        }
                    },
                },
            },
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
