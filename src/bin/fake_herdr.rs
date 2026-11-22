//! Compiled test shim emulating both `herdr-app` and `herdr-core` (`hc`) for offline E2E tests.
//! Enforces strict argument schemas matching the real installed `herdr-app` and `hc`:
//! - `do space.new`: `{name, folder, focus}`
//! - `do space.close`: `{id, confirmed}` (rejects unknown keys like `space`!)
//! - `do agent.focus`: `{terminal}`
//! - `run --bg --outlive-parent --name <1..32 [a-z0-9-_]> --cwd <dir> --label herdr.space=<id> <argv...>`
//! - Logs every invocation as JSONL to `$FAKE_HERDR_LOG`
//! - Persists fake spaces/terminals in `$FAKE_HERDR_STATE`
//! - Applies optional scripted overrides from `$FAKE_HERDR_SCRIPT`

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Debug, Default, Serialize, Deserialize)]
struct FakeState {
    next_space: u32,
    next_term: u32,
    spaces: BTreeMap<String, FakeSpace>,
    terminals: BTreeMap<String, FakeTerm>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FakeSpace {
    id: String,
    name: String,
    folder: String,
    initial_terminal: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FakeTerm {
    id: String,
    name: String,
    state: String,
    status: String,
    labels: BTreeMap<String, String>,
    first_message_sent: bool,
    first_message_reason: Option<String>,
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    log_call(&args);

    let script = load_script();
    let mut state = load_state();

    if args.is_empty() {
        eprintln!("fake_herdr: missing command");
        return ExitCode::from(2);
    }

    match args[0].as_str() {
        "do" => handle_app_do(&args[1..], &script, &mut state),
        "ls" => {
            let spaces: Vec<Value> = state
                .spaces
                .values()
                .map(|s| json!({"id": s.id, "name": s.name, "folder": s.folder}))
                .collect();
            println!("{}", json!({"spaces": spaces}));
            ExitCode::SUCCESS
        }
        "run" => handle_hc_run(&args[1..], &script, &mut state),
        "get" => handle_hc_get(&args[1..], &script, &state),
        "read" => {
            let screen = script
                .get("read_output")
                .and_then(|v| v.as_str())
                .unwrap_or("Trust this workspace? [y/N]");
            println!("{screen}");
            ExitCode::SUCCESS
        }
        "kill" => handle_hc_kill(&args[1..], &mut state),
        "wait" => ExitCode::SUCCESS,
        "rm" => handle_hc_rm(&args[1..], &mut state),
        other => {
            eprintln!("fake_herdr: unknown subcommand '{other}'");
            ExitCode::from(2)
        }
    }
}

fn handle_app_do(args: &[String], script: &Value, state: &mut FakeState) -> ExitCode {
    if args.len() < 2 {
        eprintln!("{{\"error\":\"missing command or json payload\"}}");
        return ExitCode::from(1);
    }
    let cmd = args[0].as_str();
    let payload: Value = match serde_json::from_str(&args[1]) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{{\"error\":\"invalid JSON payload: {e}\"}}");
            return ExitCode::from(1);
        }
    };
    let Some(obj) = payload.as_object() else {
        eprintln!("{{\"error\":\"payload must be an object\"}}");
        return ExitCode::from(1);
    };

    match cmd {
        "space.new" => {
            for k in obj.keys() {
                if !matches!(k.as_str(), "name" | "folder" | "focus" | "argv" | "task" | "machine") {
                    eprintln!("{{\"error\":\"unknown argument '{k}' for space.new\"}}");
                    return ExitCode::from(1);
                }
            }
            if script.get("fail_space_new").and_then(|v| v.as_bool()) == Some(true) {
                eprintln!("{{\"error\":\"herdr-app window not running\"}}");
                return ExitCode::from(1);
            }
            state.next_space += 1;
            state.next_term += 1;
            let sid = format!("s{}", state.next_space);
            let tid = format!("t{}", state.next_term);
            let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let folder = obj.get("folder").and_then(|v| v.as_str()).unwrap_or("").to_string();
            state.spaces.insert(
                sid.clone(),
                FakeSpace {
                    id: sid.clone(),
                    name,
                    folder,
                    initial_terminal: tid.clone(),
                },
            );
            save_state(state);
            println!("{}", json!({"space": sid, "terminal": tid, "pane": format!("p{}", state.next_term)}));
            ExitCode::SUCCESS
        }
        "space.close" => {
            // Strict schema matching real `herdr-app commands`: ONLY `id` and `confirmed`!
            for k in obj.keys() {
                if !matches!(k.as_str(), "id" | "confirmed") {
                    eprintln!("{{\"error\":\"unknown argument '{k}' for space.close (expected id, confirmed)\"}}");
                    return ExitCode::from(1);
                }
            }
            let Some(id) = obj.get("id").and_then(|v| v.as_str()) else {
                eprintln!("{{\"error\":\"missing required argument 'id'\"}}");
                return ExitCode::from(1);
            };
            if script.get("fail_space_close").and_then(|v| v.as_bool()) == Some(true) {
                eprintln!("{{\"error\":\"space not found\"}}");
                return ExitCode::from(1);
            }
            state.spaces.remove(id);
            save_state(state);
            println!("{}", json!({"closed": id}));
            ExitCode::SUCCESS
        }
        "agent.focus" => {
            for k in obj.keys() {
                if k != "terminal" {
                    eprintln!("{{\"error\":\"unknown argument '{k}' for agent.focus\"}}");
                    return ExitCode::from(1);
                }
            }
            println!("{}", json!({"focused": obj.get("terminal")}));
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("{{\"error\":\"unknown command '{other}'\"}}");
            ExitCode::from(1)
        }
    }
}

fn handle_hc_run(args: &[String], script: &Value, state: &mut FakeState) -> ExitCode {
    if script.get("fail_hc_run").and_then(|v| v.as_bool()) == Some(true) {
        eprintln!("simulated hc run failure");
        return ExitCode::from(1);
    }

    let mut name = String::new();
    let mut labels = BTreeMap::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bg" | "--outlive-parent" => i += 1,
            "--name" => {
                i += 1;
                if i < args.len() {
                    name = args[i].clone();
                }
                i += 1;
            }
            "--cwd" => {
                i += 2;
            }
            "--label" => {
                i += 1;
                if i < args.len() {
                    if let Some((k, v)) = args[i].split_once('=') {
                        labels.insert(k.to_string(), v.to_string());
                    }
                }
                i += 1;
            }
            _ => break,
        }
    }

    // Enforce real `hc` --name contract: 1..=32 chars of `[a-z0-9-_]`
    if name.is_empty()
        || name.len() > 32
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        eprintln!("invalid hc terminal name '{name}': must be 1..32 chars of [a-z0-9-_]");
        return ExitCode::from(2);
    }

    state.next_term += 1;
    let tid = format!("t{}", state.next_term);
    let status = script
        .get("run_status")
        .and_then(|v| v.as_str())
        .unwrap_or("working")
        .to_string();
    let fm_sent = script
        .get("first_message_sent")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let fm_reason = script
        .get("first_message_reason")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    state.terminals.insert(
        name.clone(),
        FakeTerm {
            id: tid.clone(),
            name: name.clone(),
            state: "live".to_string(),
            status,
            labels,
            first_message_sent: fm_sent,
            first_message_reason: fm_reason,
        },
    );
    save_state(state);
    println!("{tid}");
    ExitCode::SUCCESS
}

fn handle_hc_get(args: &[String], script: &Value, state: &FakeState) -> ExitCode {
    if let Some(code) = script.get("hc_get_exit").and_then(|v| v.as_u64()) {
        eprintln!("scripted hc get exit {code}");
        return ExitCode::from(code as u8);
    }
    let Some(target) = args.first() else {
        return ExitCode::from(2);
    };
    let found = state
        .terminals
        .get(target)
        .or_else(|| state.terminals.values().find(|t| &t.id == target));
    let Some(term) = found else {
        eprintln!("not_found");
        return ExitCode::from(3);
    };
    let out = json!({
        "id": term.id,
        "name": term.name,
        "state": term.state,
        "status": term.status,
        "labels": term.labels,
        "first_message": {
            "sent": term.first_message_sent,
            "reason": term.first_message_reason,
        }
    });
    println!("{out}");
    ExitCode::SUCCESS
}

fn handle_hc_kill(args: &[String], state: &mut FakeState) -> ExitCode {
    let Some(target) = args.first() else {
        return ExitCode::from(2);
    };
    let key = state
        .terminals
        .iter()
        .find(|(k, v)| *k == target || &v.id == target)
        .map(|(k, _)| k.clone());
    if let Some(k) = key {
        if let Some(t) = state.terminals.get_mut(&k) {
            t.state = "finished".to_string();
            t.status = "done".to_string();
        }
        save_state(state);
        ExitCode::SUCCESS
    } else {
        ExitCode::from(3)
    }
}

fn handle_hc_rm(args: &[String], state: &mut FakeState) -> ExitCode {
    let Some(target) = args.first() else {
        return ExitCode::from(2);
    };
    let key = state
        .terminals
        .iter()
        .find(|(k, v)| *k == target || &v.id == target)
        .map(|(k, _)| k.clone());
    if let Some(k) = key {
        state.terminals.remove(&k);
        save_state(state);
    }
    ExitCode::SUCCESS
}

fn log_call(args: &[String]) {
    if let Ok(path) = env::var("FAKE_HERDR_LOG") {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{}", json!({"argv": args}));
        }
    }
}

fn load_script() -> Value {
    env::var("FAKE_HERDR_SCRIPT")
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({}))
}

fn load_state() -> FakeState {
    env::var("FAKE_HERDR_STATE")
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_state(state: &FakeState) {
    if let Ok(path) = env::var("FAKE_HERDR_STATE") {
        let p = PathBuf::from(path);
        if let Some(parent) = p.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(p, serde_json::to_vec_pretty(state).unwrap());
    }
}
