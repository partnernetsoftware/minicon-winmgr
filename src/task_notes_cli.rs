//! Bounded local agent access to the same analysis store used by the native UI.
use crate::{
    task_access::Access,
    task_notes::*,
    task_service::{self, Collector, Identity},
};
use serde::Deserialize;
use std::{
    ffi::OsString,
    io::{Read, Write},
    path::PathBuf,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    op: String,
    target: Option<Identity>,
    subject: Option<Subject>,
    scope: Option<String>,
    expected_revision: Option<u64>,
    author: Option<String>,
    text: Option<String>,
    evidence: Option<String>,
    flag: Option<Flag>,
}
fn error(code: &'static str, message: impl Into<String>) -> Error {
    Error {
        code,
        message: message.into(),
    }
}
fn execute(store: Store, request: Request) -> Result<serde_json::Value, Error> {
    if request.op == "list" {
        if request.target.is_some() || request.subject.is_some() || request.scope.is_some() {
            return Err(error("invalid", "list does not accept a target"));
        }
        return serde_json::to_value(store.summaries()?).map_err(|e| error("json", e.to_string()));
    }
    if !matches!(request.op.as_str(), "read" | "append") {
        return Err(error("invalid", "Expected list, read or append"));
    }
    let historical = request.target.is_none();
    let subject = match (request.target, request.subject) {
        (Some(target), None) => {
            let access = Access::current().map_err(|e| error("access", e))?;
            if access
                .identify(target.pid)
                .map_err(|e| error("target", e))?
                != target
            {
                return Err(error("target", "Process identity changed"));
            }
            let snapshot = Collector::with_access(access.clone())
                .sample()
                .map_err(|e| error("target", e))?;
            let process = snapshot
                .processes
                .iter()
                .find(|p| p.identity == target)
                .ok_or_else(|| error("target", "Target is unavailable in my tasks"))?;
            if access
                .identify(target.pid)
                .map_err(|e| error("target", e))?
                != target
            {
                return Err(error("target", "Process identity changed"));
            }
            match request.scope.as_deref().unwrap_or("run") {
                "run" => Subject::for_run(
                    process
                        .owner
                        .clone()
                        .ok_or_else(|| error("target", "Owner unavailable"))?,
                    target,
                )?,
                "application" if process.path.is_some() => Subject::process(process)?,
                _ => {
                    return Err(error(
                        "invalid",
                        "scope must be run or application (with a known image path)",
                    ));
                }
            }
        }
        (None, Some(subject)) if request.scope.is_none() => subject,
        _ => {
            return Err(error(
                "invalid",
                "Supply exactly one verified target or stored subject",
            ));
        }
    };
    let thread = store.thread(&subject)?;
    if request.op == "read" {
        return serde_json::to_value(thread).map_err(|e| error("json", e.to_string()));
    }
    if historical && thread.revision == 0 {
        return Err(error(
            "target",
            "New subjects must be observed through a verified running target",
        ));
    }
    let result = store.append(Append {
        subject,
        run: request.target,
        expected_revision: request
            .expected_revision
            .ok_or_else(|| error("invalid", "expected_revision is required"))?,
        author: Author {
            kind: AuthorKind::Agent,
            name: request
                .author
                .ok_or_else(|| error("invalid", "author is required"))?,
        },
        text: request.text.unwrap_or_default(),
        evidence: request.evidence.unwrap_or_default(),
        flag: request.flag,
    })?;
    serde_json::to_value(result).map_err(|e| error("json", e.to_string()))
}
pub fn run(arguments: Vec<OsString>) -> i32 {
    let result = (|| {
        if task_service::elevated().map_err(|e| error("access", e))? {
            return Err(error(
                "access",
                "The notes JSON interface uses ordinary user rights; start it without elevation",
            ));
        }
        let store = match arguments.as_slice() {
            [] => Store::current()?,
            [flag, path] if flag == "--store" => Store::at(PathBuf::from(path))?,
            _ => {
                return Err(error(
                    "invalid",
                    "Arguments: --notes-json [--store ABSOLUTE_DIRECTORY]; one JSON request on stdin",
                ));
            }
        };
        let mut bytes = Vec::new();
        std::io::stdin().take(65537).read_to_end(&mut bytes)?;
        if bytes.len() > 65536 {
            return Err(error("invalid", "Request exceeds 64 KiB"));
        }
        let request = serde_json::from_slice(&bytes).map_err(|e| error("json", e.to_string()))?;
        execute(store, request)
    })();
    let (code, response) = match result {
        Ok(data) => (0, serde_json::json!({"schema":1,"ok":true,"data":data})),
        Err(e) => (
            1,
            serde_json::json!({"schema":1,"ok":false,"error":{"code":e.code,"message":e.message}}),
        ),
    };
    let _ = writeln!(std::io::stdout().lock(), "{response}");
    code
}
