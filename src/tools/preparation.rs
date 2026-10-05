use super::*;
use serde_json::Value;

impl Tools {
    pub(crate) fn lock_path(&self, path: &str) -> Option<PathBuf> {
        self.target(path).ok().map(|target| target.absolute)
    }

    pub(crate) fn prepare(
        &self,
        turn_id: &str,
        name: &str,
        args: &Value,
    ) -> Result<Option<Self>, ToolError> {
        let string = |key| args.get(key).and_then(Value::as_str).unwrap_or("");
        let body = match name {
            "read_file" | "list_dir" => {
                let target = self.target(string("path"))?;
                let path = display(&target.absolute);
                if !target.inside && !self.allowed_outside_read(&path)? {
                    let verb = if name == "read_file" { "Read" } else { "List" };
                    Some(PermissionBody {
                        action: format!("{verb} {path}"),
                        path: Some(path),
                        ..PermissionBody::default()
                    })
                } else {
                    None
                }
            }
            "write_file" | "search_replace" => {
                let target = self.target(string("path"))?;
                let path = display(&target.absolute);
                if self.allowed_write(&path)? {
                    None
                } else {
                    let contents = if name == "search_replace" {
                        self.replacement(
                            string("path"),
                            string("old_string"),
                            string("new_string"),
                            args.get("replace_all")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        )?
                    } else {
                        string("contents").to_string()
                    };
                    if contents.len() > WRITE_LIMIT {
                        return Err(ToolError::TooLarge {
                            path: target.absolute,
                            bytes: contents.len(),
                            limit: WRITE_LIMIT,
                        });
                    }
                    let parent = write_parent(&target.absolute)?;
                    let (old, created) = write_bytes(&parent, &target.absolute)?;
                    let file_name = target
                        .absolute
                        .file_name()
                        .ok_or_else(|| ToolError::NoFileName {
                            path: target.absolute.clone(),
                        })?
                        .to_string_lossy();
                    let verb = if created { "Create" } else { "Replace" };
                    Some(permit::write_permission(
                        &format!("{verb} {file_name}"),
                        &path,
                        &old,
                        contents.as_bytes(),
                    ))
                }
            }
            "run" => {
                let argv: Vec<String> = args
                    .get("argv")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
                let program = argv.first().ok_or(ToolError::NoCommand)?;
                let timeout = args
                    .get("timeout_sec")
                    .and_then(Value::as_u64)
                    .unwrap_or(DEFAULT_TIMEOUT_SEC);
                if timeout == 0 || timeout > MAX_TIMEOUT_SEC {
                    return Err(ToolError::BadTimeout { secs: timeout });
                }
                if self.allowed_argv(&argv)? {
                    None
                } else {
                    Some(PermissionBody {
                        action: format!("Run {program}"),
                        argv: Some(argv),
                        timeout_sec: (timeout != DEFAULT_TIMEOUT_SEC).then_some(timeout),
                        ..PermissionBody::default()
                    })
                }
            }
            "web_fetch" => {
                crate::web::cap(args.get("max_bytes").and_then(Value::as_u64))?;
                let url = string("url");
                let parsed = crate::web::parse(url)?;
                crate::web::ensure_public(&parsed, &crate::web::production())?;
                if self.allowed_fetch(&crate::web::origin(&parsed)?)? {
                    None
                } else {
                    Some(PermissionBody {
                        action: format!("Fetch {url}"),
                        path: Some(url.to_string()),
                        ..PermissionBody::default()
                    })
                }
            }
            "web_search" if !string("query").trim().is_empty() && !self.allowed_search()? => {
                Some(PermissionBody {
                    action: "Search the web".into(),
                    path: Some(string("query").trim().to_string()),
                    ..PermissionBody::default()
                })
            }
            _ => None,
        };
        let mut prepared = self.clone();
        if let Some(body) = body {
            let verdict = self.gate.ask(turn_id, &body)?;
            if !verdict.allowed() {
                return Ok(None);
            }
            prepared.approval = Some(Arc::new(Mutex::new(Some(Approval { body, verdict }))));
        }
        Ok(Some(prepared))
    }
}
