use super::*;

#[derive(Clone, Default)]
pub(super) struct Snapshot {
    sessions: Vec<SessionRow>,
    projects: Vec<ListedProject>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Project {
    pub server: Option<String>,
    pub row: ListedProject,
}

pub(super) fn key(server: &Option<String>, id: &str) -> String {
    serde_json::to_string(&(server, id)).unwrap()
}

fn group_key(server: &Option<String>, id: Option<&str>) -> String {
    serde_json::to_string(&(server, id)).unwrap()
}

pub(super) fn sessions(app: &App) -> Vec<SessionRow> {
    let Some(servers) = &app.servers else {
        return app.sessions.clone();
    };
    servers
        .iter()
        .flat_map(|(server, snapshot)| {
            let rows = if server == &app.server {
                &app.sessions
            } else {
                &snapshot.sessions
            };
            rows.iter()
                .map(|row| {
                    let mut row = row.clone();
                    row.id = key(server, &row.id);
                    row.parent_id = row.parent_id.map(|id| key(server, &id));
                    if !row.archived {
                        row.project = Some(group_key(server, row.project.as_deref()));
                    }
                    row
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

pub(super) fn selected(app: &App) -> String {
    if app.servers.is_some() {
        key(&app.server, &app.selected)
    } else {
        app.selected.clone()
    }
}

pub(super) fn projects(app: &App) -> Vec<Project> {
    app.servers
        .iter()
        .flat_map(|servers| servers.iter())
        .flat_map(|(server, snapshot)| {
            let projects = if server == &app.server {
                &app.project_rows
            } else {
                &snapshot.projects
            };
            projects.iter().map(|row| Project {
                server: server.clone(),
                row: row.clone(),
            })
        })
        .collect()
}

pub(super) fn project_rows(app: &App) -> Vec<screen::ProjectRow> {
    let mut rows: Vec<_> = projects(app)
        .into_iter()
        .map(|project| screen::ProjectRow {
            id: group_key(&project.server, Some(&project.row.id)),
            name: project.row.name,
            server: Some(
                project
                    .server
                    .as_deref()
                    .map_or("Local", |id| app.server_label(id))
                    .to_string(),
            ),
        })
        .collect();
    if let Some(servers) = &app.servers {
        for (server, snapshot) in servers {
            let sessions = if server == &app.server {
                &app.sessions
            } else {
                &snapshot.sessions
            };
            for session in sessions {
                let id = group_key(server, session.project.as_deref());
                if rows.iter().any(|row| row.id == id) {
                    continue;
                }
                rows.push(screen::ProjectRow {
                    id,
                    name: session
                        .project_name
                        .clone()
                        .or_else(|| session.project.clone())
                        .unwrap_or_else(|| screen::OTHER_PROJECT.into()),
                    server: Some(
                        server
                            .as_deref()
                            .map_or("Local", |id| app.server_label(id))
                            .to_string(),
                    ),
                });
            }
        }
    }
    rows
}

pub(super) async fn fetch_projects(
    home: &Path,
    selected: &Option<String>,
    client: &Client,
) -> Result<(Vec<Project>, Vec<String>), String> {
    let saved = crate::pairing::Connections::load(&home.join(".kyotoagent/config.toml"))?;
    let mut targets: BTreeMap<_, _> = saved
        .servers
        .into_iter()
        .map(|(id, uri)| (Some(id), Client::at_url(&uri)))
        .collect();
    if home.join(".kyotoagent").join(server::SOCKET_FILE).exists() {
        targets.insert(
            None,
            Ok(Client::at(
                home.join(".kyotoagent").join(server::SOCKET_FILE),
            )),
        );
    }
    targets.insert(selected.clone(), Ok(client.clone()));
    let results =
        futures_util::future::join_all(targets.into_iter().map(|(id, connection)| async move {
            let result = async {
                let connection = connection?;
                let (status, body) = tokio::time::timeout(
                    Duration::from_secs(8),
                    connection.request("GET", "/v1/projects", None),
                )
                .await
                .map_err(|_| "Server connection timed out.".to_string())??;
                if status != 200 {
                    return Err(error_text(&body, status));
                }
                let rows = serde_json::from_str::<Vec<ListedProject>>(&body)
                    .map_err(|_| "Invalid project list from server.".to_string())?;
                *connection.projects.lock().unwrap() = Some(CachedProjects {
                    refreshed: Instant::now(),
                    rows: rows.clone(),
                });
                Ok(rows)
            }
            .await;
            (id, result)
        }))
        .await;
    let mut projects = Vec::new();
    let mut errors = Vec::new();
    for (server, result) in results {
        match result {
            Ok(rows) => projects.extend(rows.into_iter().map(|row| Project {
                server: server.clone(),
                row,
            })),
            Err(error) => errors.push(format!(
                "{}: {error}",
                server.as_deref().map_or("Local", |id| saved
                    .server_names
                    .get(id)
                    .map_or(id, String::as_str))
            )),
        }
    }
    Ok((projects, errors))
}

pub(super) struct Connections {
    clients: BTreeMap<Option<String>, (String, Client, poll::BackgroundPoll)>,
    checked: Instant,
}

impl Connections {
    pub(super) fn new() -> Self {
        Self {
            clients: BTreeMap::new(),
            checked: Instant::now() - Duration::from_secs(2),
        }
    }

    pub(super) async fn advance(
        &mut self,
        app: &mut App,
        client: &Client,
        cached: &mut BTreeMap<Option<String>, App>,
    ) {
        if self.checked.elapsed() >= Duration::from_secs(1) {
            self.checked = Instant::now();
            if let Ok(saved) =
                crate::pairing::Connections::load(&app.home.join(".kyotoagent/config.toml"))
            {
                app.server_names = saved.server_names;
                self.clients.retain(|id, _| {
                    id.is_none()
                        || id == &app.server
                        || id.as_ref().is_some_and(|id| saved.servers.contains_key(id))
                });
                cached.retain(|id, _| {
                    id.is_none()
                        || id == &app.server
                        || id.as_ref().is_some_and(|id| saved.servers.contains_key(id))
                });
                let mut targets: Vec<_> = saved
                    .servers
                    .into_iter()
                    .map(|(id, uri)| (Some(id), uri))
                    .collect();
                if app
                    .home
                    .join(".kyotoagent")
                    .join(server::SOCKET_FILE)
                    .exists()
                {
                    targets.push((None, String::new()));
                }
                for (id, uri) in targets {
                    if self
                        .clients
                        .get(&id)
                        .is_some_and(|(saved, _, _)| saved == &uri)
                    {
                        continue;
                    }
                    let connection = if id.is_none() {
                        Ok(Client::at(
                            app.home.join(".kyotoagent").join(server::SOCKET_FILE),
                        ))
                    } else {
                        Client::at_url(&uri)
                    };
                    if let Ok(connection) = connection {
                        self.clients
                            .insert(id.clone(), (uri, connection, poll::BackgroundPoll::new()));
                        if id != app.server {
                            cached.entry(id.clone()).or_insert_with(|| {
                                let mut next = App::new(
                                    if id.is_none() {
                                        current_workspace().unwrap_or_default()
                                    } else {
                                        PathBuf::new()
                                    },
                                    app.home.clone(),
                                    String::new(),
                                );
                                next.server = id;
                                next
                            });
                        }
                    }
                }
            }
        }
        for (id, (_, connection, refresh)) in &mut self.clients {
            if id == &app.server {
                continue;
            }
            if let Some(state) = cached.get_mut(id) {
                refresh.advance(state, connection).await;
            }
        }
        let mut snapshots: BTreeMap<_, _> = cached
            .iter()
            .map(|(id, state)| {
                (
                    id.clone(),
                    Snapshot {
                        sessions: state.sessions.clone(),
                        projects: state.project_rows.clone(),
                    },
                )
            })
            .collect();
        snapshots.insert(
            app.server.clone(),
            Snapshot {
                sessions: app.sessions.clone(),
                projects: app.project_rows.clone(),
            },
        );
        app.servers = Some(snapshots);
        self.clients
            .entry(app.server.clone())
            .or_insert_with(|| (String::new(), client.clone(), poll::BackgroundPoll::new()));
    }

    pub(super) fn activate(
        app: &mut App,
        client: &mut Client,
        cached: &mut BTreeMap<Option<String>, App>,
        target: &Option<String>,
    ) -> Result<(), String> {
        if &app.server == target {
            return Ok(());
        }
        let saved = crate::pairing::Connections::load(&app.home.join(".kyotoagent/config.toml"))?;
        let connection = match target {
            Some(id) => Client::at_url(saved.servers.get(id).ok_or("server is not saved")?)?,
            None => Client::at(app.home.join(".kyotoagent").join(server::SOCKET_FILE)),
        };
        if !cached.contains_key(target) {
            return Err("Server is still connecting. Try again.".into());
        }
        crate::pairing::Connections::select(
            &app.home.join(".kyotoagent/config.toml"),
            target.as_deref(),
        )?;
        let mut next = cached.remove(target).unwrap();
        next.area = app.area;
        next.servers = app.servers.take();
        next.server_names = app.server_names.clone();
        next.collapsed = app.collapsed.clone();
        release_command_surfaces(&mut next);
        release_command_surfaces(app);
        let mut old = std::mem::replace(app, next);
        old.servers = None;
        cached.insert(old.server.clone(), old);
        *client = connection;
        Ok(())
    }

    pub(super) async fn apply(
        app: &mut App,
        client: &mut Client,
        cached: &mut BTreeMap<Option<String>, App>,
        effect: Effect,
    ) -> Result<bool, String> {
        let effect = match effect {
            Effect::SelectNext | Effect::SelectPrev if app.servers.is_some() => {
                let rows = sessions(app);
                let selected = selected(app);
                let index = rows.iter().position(|row| row.id == selected);
                let next = index
                    .and_then(|index| {
                        if matches!(effect, Effect::SelectNext) {
                            index.checked_add(1)
                        } else {
                            index.checked_sub(1)
                        }
                    })
                    .and_then(|index| rows.get(index));
                match next {
                    Some(row) => Effect::SelectSession(row.id.clone()),
                    None => return Ok(true),
                }
            }
            other => other,
        };
        let id = match &effect {
            Effect::SelectSession(id) | Effect::OpenMenu { id, .. } => Some(id),
            _ => None,
        };
        let effect = if let Some((server, id)) =
            id.and_then(|id| serde_json::from_str::<(Option<String>, String)>(id).ok())
        {
            if let Err(error) = Self::activate(app, client, cached, &server) {
                app.notice = Some(error);
                return Ok(true);
            }
            match effect {
                Effect::OpenMenu { column, row, .. } => Effect::OpenMenu { id, column, row },
                _ => Effect::SelectSession(id),
            }
        } else {
            effect
        };
        apply(app, client, effect).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn identical_ids_keep_projects_parents_drafts_and_actions_on_their_server() {
        let home = std::env::temp_dir().join(format!("ka-all-servers-{}", std::process::id()));
        let path = home.join(".kyotoagent/config.toml");
        let first =
            crate::pairing::Connections::remember(&path, "https://first.example", true).unwrap();
        let second =
            crate::pairing::Connections::remember(&path, "https://second.example", false).unwrap();
        let mut app = App::new(home.clone(), home.clone(), "same".into());
        app.server = Some(first.clone());
        app.ask = "first draft".into();
        let mut row = crate::mock::idle().sessions.remove(0);
        row.id = "same".into();
        row.project = Some("project".into());
        let project = ListedProject {
            id: "project".into(),
            name: "Shared name".into(),
            path: "/work/project".into(),
        };
        app.sessions = vec![row.clone()];
        app.project_rows = vec![project.clone()];
        let mut other = App::new(home.clone(), home.clone(), "same".into());
        other.server = Some(second.clone());
        other.ask = "second draft".into();
        other.sessions = vec![row.clone()];
        other.project_rows = vec![project];
        let mut child = row;
        child.id = "child".into();
        child.parent_id = Some("same".into());
        other.sessions.push(child);
        let mut cached = BTreeMap::from([(Some(second.clone()), other)]);
        let mut client = Client::at_url("https://first.example").unwrap();
        let mut connections = Connections::new();
        connections.advance(&mut app, &client, &mut cached).await;
        let model = screen_model(&app);
        assert_eq!(model.projects.len(), 2);
        assert_ne!(model.projects[0].id, model.projects[1].id);
        assert_eq!(model.sessions.len(), 3);
        assert_eq!(
            model.sessions[2].parent_id,
            Some(model.sessions[1].id.clone())
        );
        let first_group = group_key(&Some(first.clone()), Some("project"));
        let second_group = group_key(&Some(second.clone()), Some("project"));
        app.collapsed.insert(first_group.clone());
        app.collapsed.insert(second_group.clone());
        Connections::apply(
            &mut app,
            &mut client,
            &mut cached,
            Effect::SelectSession(key(&Some(second.clone()), "same")),
        )
        .await
        .unwrap();
        assert_eq!(app.server.as_deref(), Some(second.as_str()));
        assert_eq!(client.server_id().as_deref(), Some(second.as_str()));
        assert_eq!(app.selected, "same");
        assert_eq!(app.ask, "second draft");
        assert!(!app.collapsed.contains(&second_group));
        assert!(app.collapsed.contains(&first_group));
        Connections::apply(&mut app, &mut client, &mut cached, Effect::Type('!'))
            .await
            .unwrap();
        Connections::apply(
            &mut app,
            &mut client,
            &mut cached,
            Effect::SelectSession(key(&Some(first.clone()), "same")),
        )
        .await
        .unwrap();
        assert_eq!(app.ask, "first draft");
        assert!(!app.collapsed.contains(&first_group));
        assert_eq!(cached[&Some(second)].ask, "second draft!");
        assert_eq!(
            crate::pairing::Connections::load(&path).unwrap().server,
            Some(first)
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
