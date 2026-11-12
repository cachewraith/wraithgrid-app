//! A pane spec on the wire (`spawnSpecSchema` in `src/main/spawn-resolve.ts`): the window
//! sends it in a `spawn` frame when the machine resolves panes with `wraithgrid host-stdio`,
//! and that bridge checks it before resolving anything. Core's `SpawnSpec` has no serde, so
//! both directions live here.

use serde_json::{json, Value};
use wraithgrid_core::agent_cli::AgentCli;
use wraithgrid_core::config::is_id;
use wraithgrid_core::spawn::SpawnSpec;

/// The spec as the TS sends it (camelCase).
pub fn spawn_spec_json(spec: &SpawnSpec) -> Value {
    json!({
        "paneId": spec.pane_id,
        "cli": spec.cli.as_str(),
        "shell": spec.shell,
        "configDir": spec.config_dir,
        "cwd": spec.cwd,
        "args": spec.args,
        "resume": spec.resume,
        "cols": spec.cols,
        "rows": spec.rows,
    })
}

/// zod counts UTF-16 units.
fn len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn dim(v: &Value) -> Option<u16> {
    v.as_u64()
        .filter(|n| (1..=1000).contains(n))
        .map(|n| n as u16)
}

/// The spec if `v` passes the schema; None for anything else.
pub fn parse_spawn_spec(v: &Value) -> Option<SpawnSpec> {
    let o = v.as_object()?;
    let pane_id = o.get("paneId")?.as_str().filter(|s| is_id(s))?;
    let config_dir = match o.get("configDir")? {
        Value::Null => None,
        Value::String(s) if len(s) <= 4096 => Some(s.clone()),
        _ => return None,
    };
    let cwd = o
        .get("cwd")?
        .as_str()
        .filter(|s| (1..=4096).contains(&len(s)))?;
    let args = o.get("args")?.as_array().filter(|a| a.len() <= 32)?;
    let args = args
        .iter()
        .map(|a| {
            a.as_str()
                .filter(|s| len(s) <= 1000 && !s.contains('\0'))
                .map(String::from)
        })
        .collect::<Option<Vec<_>>>()?;
    Some(SpawnSpec {
        pane_id: pane_id.to_string(),
        cli: AgentCli::parse(o.get("cli")?.as_str()?)?,
        shell: o.get("shell")?.as_bool()?,
        config_dir,
        cwd: cwd.to_string(),
        args,
        resume: o.get("resume")?.as_bool()?,
        cols: dim(o.get("cols")?)?,
        rows: dim(o.get("rows")?)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> SpawnSpec {
        SpawnSpec {
            pane_id: "p-1".into(),
            cli: AgentCli::Codex,
            shell: false,
            config_dir: Some("~/.wraithgrid/accounts/work".into()),
            cwd: "~/proj".into(),
            args: vec!["--model".into(), "o3".into()],
            resume: true,
            cols: 100,
            rows: 30,
        }
    }

    #[test]
    fn round_trips() {
        let s = spec();
        assert_eq!(parse_spawn_spec(&spawn_spec_json(&s)), Some(s.clone()));
        let none = SpawnSpec {
            config_dir: None,
            ..s
        };
        assert_eq!(parse_spawn_spec(&spawn_spec_json(&none)), Some(none));
    }

    #[test]
    fn refuses_what_the_schema_refuses() {
        let bad = |k: &str, v: Value| {
            let mut j = spawn_spec_json(&spec());
            j[k] = v;
            parse_spawn_spec(&j)
        };
        assert!(bad("paneId", json!("a b")).is_none());
        assert!(bad("cli", json!("vim")).is_none());
        assert!(bad("cwd", json!("")).is_none());
        assert!(bad("cols", json!(0)).is_none());
        assert!(bad("rows", json!(1001)).is_none());
        assert!(bad("args", json!(["a\0b"])).is_none());
        assert!(bad("args", json!(vec!["x"; 33])).is_none());
        assert!(bad("shell", json!("yes")).is_none());
        let mut j = spawn_spec_json(&spec());
        j.as_object_mut().unwrap().remove("configDir");
        assert!(parse_spawn_spec(&j).is_none());
        assert!(parse_spawn_spec(&json!(null)).is_none());
    }
}
