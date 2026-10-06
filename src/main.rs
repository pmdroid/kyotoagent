//! The `kyotoagent` binary.
//!
//! `kyotoagent serve` runs the server on the unix socket, and on HTTPS when
//! `listen` is set. The other subcommands talk to that socket, or to `--url`
//! / `KYOTOAGENT_URL`. With no arguments, and with `kyotoagent attach <id>`, the
//! binary opens the live session screen.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

use kyotoagent::auth;
use kyotoagent::config::Config;
use kyotoagent::server::{self, Server};
use kyotoagent::tui::{self, Client, WORKSPACE_HERE, WORKSPACE_QUESTION, WORKSPACE_WORKTREE};

const USAGE: &str = "\
Usage:
  kyotoagent
  kyotoagent --yolo
  kyotoagent attach <id>
  kyotoagent attach <id> --yolo
  kyotoagent serve
  kyotoagent serve --listen 0.0.0.0:7841
  kyotoagent --url https://box.tailnet.ts.net:7841 sessions
  kyotoagent new [repo-id]
  kyotoagent sessions
  kyotoagent repos
  kyotoagent log [id]
  kyotoagent cancel [id]
  kyotoagent provider
  kyotoagent provider use <id>
  kyotoagent provider add <id> --base-url <url> --model <model> [--api-key-env <name>]
  kyotoagent provider add opencode [--model <model>] [--api-key-env <name>]
  kyotoagent doctor
  kyotoagent systemprompt
  kyotoagent pair hostname:7841
  kyotoagent doctor --url https://box.tailnet.ts.net:7841

Commands:
  (none)    Open the session list for the current directory.
  attach    Open the session list with that session selected.

  --yolo    Allow writes and commands as they appear. Ctrl-Y toggles it.
  serve     Serve the sessions on the unix socket, and on HTTPS when listen is set.
  new       Create a session in a registered repository or the current directory.
  sessions  List the sessions, one plain-text row each.
  repos     List registered repositories with their id, name, and path.
  log       Print the event log of a session: the newest in this directory, or an id.
  cancel    Cancel the current turn of a session: the newest in this directory, or an id.
  provider  Print or change the model server in ~/.kyotoagent/config.toml.
  doctor    Check the socket, the model, and closeout.yaml. Print the system prompt with systemprompt.
  systemprompt  Print the system prompt for the current directory.
  pair      Pair with a server, or show a temporary pairing QR code.

The server speaks HTTP on ~/.kyotoagent/kyotoagent.sock. Optional listen in
~/.kyotoagent/config.toml, or `kyotoagent serve --listen`, also serves HTTPS with
HTTP/2. Certs default to ~/.kyotoagent/certs/server.crt and server.key, filled
with `tailscale cert <magicdns-name>`. `kyotoagent --url` or KYOTOAGENT_URL uses
that HTTPS address. Remote clients authenticate using the kyotoagent:// link
from `kyotoagent pair hostname:7841`, through --url or the mobile QR scanner.
Import a link with `kyotoagent pair 'kyotoagent://hostname:7841?token=...'`.
Codes expire after 10 minutes or one exchange. The connecting device saves
the returned JWT and selected server in ~/.kyotoagent/config.toml.
Use /server or the Server command in the TUI to switch servers or choose Local.
Start the local server with `kyotoagent serve`.

To see the three session screens in a real terminal:

  cargo run --example screens

The same three states are checked against tests/screens/*.txt by cargo test.

To watch one real chat completion against a loopback server:

  cargo run --example chat
  cargo run --example chat -- --with-key
";

#[derive(Parser)]
#[command(
    version,
    about = "Kyoto Agent, a small remote-first coding agent.",
    after_help = USAGE
)]
struct Cli {
    #[arg(long, global = true)]
    yolo: bool,
    #[arg(long, global = true, env = "KYOTOAGENT_URL", hide_env_values = true)]
    url: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the sessions on the unix socket.
    Serve {
        #[arg(long)]
        listen: Option<String>,
    },
    /// Create a session for the current directory.
    New {
        repo: Option<String>,
        #[arg(long)]
        task: Option<String>,
    },
    /// List the sessions, one plain-text row each.
    Sessions,
    #[command(
        about = "List registered repositories with their id, name, and path.",
        alias = "repo"
    )]
    Repos,
    /// Print the event log of a session: the newest in this directory, or an id.
    Log { id: Option<String> },
    /// Cancel the current turn of a session: the newest in this directory, or an id.
    Cancel { id: Option<String> },
    #[command(about = "Open the session list with that session selected.")]
    Attach { id: String },
    #[command(
        about = "Check the socket, the model, and closeout.yaml. Print the system prompt with systemprompt."
    )]
    Doctor,
    /// Print the system prompt for the current directory.
    Systemprompt,
    #[command(
        about = "Exchange a pairing URI for a saved JWT, or show a 10-minute code for HOST:PORT."
    )]
    Pair {
        #[arg(value_name = "HOST:PORT_OR_URI")]
        host: String,
    },
    #[command(about = "Print or change the model server in ~/.kyotoagent/config.toml.")]
    Provider {
        #[command(subcommand)]
        command: Option<ProviderCommand>,
    },
}

#[derive(Subcommand)]
enum ProviderCommand {
    #[command(about = "Select a provider already in the file.")]
    Use { id: String },
    #[command(about = "Insert a provider table and select it.")]
    Add {
        id: String,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        api_key_env: Option<String>,
    },
}

fn main() -> ExitCode {
    let name = launch_name();
    let cli = match parsed_cli(&name) {
        Ok(cli) => cli,
        Err(error) => error.exit(),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("the runtime is built");
    match runtime.block_on(run(cli.command, cli.yolo, server_url(cli.url))) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(message) => {
            eprintln!("{name}: {message}");
            ExitCode::FAILURE
        }
    }
}

fn launch_name() -> String {
    std::env::args_os()
        .next()
        .as_ref()
        .map(std::path::Path::new)
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "kyotoagent".to_string())
}

fn parsed_cli(name: &str) -> Result<Cli, clap::Error> {
    let name = static_launch_name(name);
    let mut command = Cli::command().name(name);
    command.set_bin_name(name);
    Cli::from_arg_matches(&command.try_get_matches()?)
}

fn static_launch_name(name: &str) -> &'static str {
    match name {
        "kyoto" => "kyoto",
        "kyotoagent" => "kyotoagent",
        _ => Box::leak(name.to_string().into_boxed_str()),
    }
}

fn server_url(value: Option<String>) -> Option<String> {
    nonempty(value)
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    })
}

async fn run(command: Option<Command>, yolo: bool, url: Option<String>) -> Result<bool, String> {
    let url = url.and_then(|value| {
        let value = value.trim().to_string();
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    });
    if let Some(Command::Doctor) = command {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let cwd = std::env::current_dir().ok();
        let mut out = std::io::stdout();
        return Ok(kyotoagent::doctor::run(
            home.as_deref(),
            cwd.as_deref(),
            url.as_deref(),
            &mut out,
        )
        .await);
    }
    match command {
        None => tui::attach(None, yolo, url).await,
        Some(Command::Attach { id }) => tui::attach(Some(id), yolo, url).await,
        Some(Command::Serve { listen }) => serve(listen).await,
        Some(Command::New { repo, task }) => new_session(repo, task, url).await,
        Some(Command::Sessions) => list_sessions(url).await,
        Some(Command::Repos) => list_repos(url).await,
        Some(Command::Log { id }) => log(id, url).await,
        Some(Command::Cancel { id }) => cancel(id, url).await,
        Some(Command::Provider { command }) => provider(command),
        Some(Command::Doctor) => unreachable!(),
        Some(Command::Systemprompt) => systemprompt(),
        Some(Command::Pair { host }) => pair(&host).await,
    }
    .map(|()| true)
}

fn systemprompt() -> Result<(), String> {
    let workspace = current_workspace()?;
    let path = PathBuf::from(&workspace);
    let config = Config::default_path()
        .and_then(|path| Config::load(&path).ok())
        .unwrap_or_default();
    let closeout = kyotoagent::closeout::read(&path).unwrap_or(None);
    let prompt = kyotoagent::prompt::system_prompt(
        &workspace,
        &kyotoagent::skills::index(&path),
        closeout.as_ref(),
        &kyotoagent::agents_doc::load(&path),
        config.provider_context_window(),
    );
    print!("{prompt}");
    Ok(())
}

async fn pair(host: &str) -> Result<(), String> {
    let path = Config::default_path().ok_or("no home directory for the config")?;
    if host.starts_with("kyotoagent://") || host.starts_with("https://") {
        let (uri, info) = kyotoagent::pairing::verify(host, "Kyoto Agent CLI").await?;
        let id = kyotoagent::pairing::Connections::remember(&path, &uri, true)?;
        println!(
            "Paired with {} {} at {id}.\nModels: {}. Repositories: {}.",
            info.server.name,
            info.server.version,
            info.models.len(),
            info.repositories.len()
        );
        return Ok(());
    }
    let uri = format!("kyotoagent://{host}?token=placeholder");
    kyotoagent::pairing::connection(&uri)?;
    let root = server::default_root().ok_or("no home directory for the server root")?;
    let token = kyotoagent::pairing::PairingKey::load(&root)?.code()?;
    let uri = format!("kyotoagent://{host}?token={token}");
    let code = qrcode::QrCode::new(uri.as_bytes()).map_err(|error| error.to_string())?;
    println!(
        "{}",
        code.render::<qrcode::render::unicode::Dense1x2>()
            .quiet_zone(true)
            .build()
    );
    println!("Pairing code expires in 10 minutes and can be used once.\n{uri}");
    Ok(())
}

fn add_provider(
    path: &std::path::Path,
    id: &str,
    base_url: Option<String>,
    model: Option<String>,
    api_key_env: Option<String>,
) -> Result<(), String> {
    if id == "opencode" {
        return add_opencode(path, base_url, model, api_key_env);
    }
    let base_url = required_flag(base_url, "--base-url")?;
    let model = required_flag(model, "--model")?;
    Config::add_provider(path, id, &base_url, &model, api_key_env.as_deref())
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn required_flag(value: Option<String>, flag: &str) -> Result<String, String> {
    match value
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
    {
        Some(value) => Ok(value),
        None => Err(format!("{flag} is required")),
    }
}

fn add_opencode(
    path: &std::path::Path,
    base_url: Option<String>,
    model: Option<String>,
    api_key_env: Option<String>,
) -> Result<(), String> {
    let root = auth::default_root().ok_or("no home directory for the session")?;
    let auth_path = root.join(auth::OPENCODE_AUTH_FILE);
    if let Some(name) = api_key_env.as_deref() {
        Config::add_opencode(path, base_url.as_deref(), model.as_deref(), Some(name))
            .map_err(|error| error.to_string())?;
        if auth_path.exists() {
            std::fs::remove_file(&auth_path).map_err(|error| error.to_string())?;
        }
        return Ok(());
    }
    let key = read_opencode_key()?;
    auth::write_opencode_key(&auth_path, &key).map_err(|error| error.to_string())?;
    Config::add_opencode(path, base_url.as_deref(), model.as_deref(), None)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn read_opencode_key() -> Result<String, String> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        let _hidden = EchoOff::enable()?;
        let mut out = io::stdout();
        write!(out, "OpenCode API key: ").map_err(|error| error.to_string())?;
        out.flush().map_err(|error| error.to_string())?;
        let mut line = String::new();
        stdin
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        drop(_hidden);
        writeln!(out).map_err(|error| error.to_string())?;
        let key = line.trim().to_string();
        if key.is_empty() {
            return Err("empty OpenCode API key".into());
        }
        return Ok(key);
    }
    let mut line = String::new();
    stdin
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    let key = line.trim().to_string();
    if key.is_empty() {
        return Err("empty OpenCode API key".into());
    }
    Ok(key)
}

struct EchoOff {
    fd: i32,
    original: libc::termios,
}

impl EchoOff {
    fn enable() -> Result<EchoOff, String> {
        let fd = libc::STDIN_FILENO;
        let mut original = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
            return Err("cannot read the terminal".into());
        }
        let mut hidden = original;
        hidden.c_lflag &= !libc::ECHO;
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &hidden) } != 0 {
            return Err("cannot hide the key".into());
        }
        Ok(EchoOff { fd, original })
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.original);
        }
    }
}

fn provider(command: Option<ProviderCommand>) -> Result<(), String> {
    let path = Config::default_path().ok_or("no home directory for the config")?;
    match command {
        None => {
            let config = Config::load(&path).map_err(|error| error.to_string())?;
            match &config.provider {
                Some(id) => println!("{id}"),
                None => println!(),
            }
            println!("{}", config.base_url);
            println!("{}", config.model);
            Ok(())
        }
        Some(ProviderCommand::Use { id }) => {
            Config::use_provider(&path, &id).map_err(|error| error.to_string())?;
            Ok(())
        }
        Some(ProviderCommand::Add {
            id,
            base_url,
            model,
            api_key_env,
        }) => add_provider(&path, &id, base_url, model, api_key_env),
    }
}

async fn serve(listen: Option<String>) -> Result<(), String> {
    let root = server::default_root().ok_or("no home directory for the Kyoto Agent root")?;
    let config_path = Config::default_path().ok_or("no home directory for the config")?;
    if let Some(addr) = &listen {
        Config::set_listen(&config_path, addr).map_err(|error| error.to_string())?;
    }
    let mut config = Config::load(&config_path).unwrap_or_default();
    if let Some(addr) = listen {
        config.listen = Some(addr);
    }
    let server = Server::new(&root, &config).map_err(|error| error.to_string())?;
    server.serve().await.map_err(|error| error.to_string())
}

fn ask_worktree() -> Result<bool, String> {
    if !io::stdin().is_terminal() {
        return Ok(false);
    }
    let mut out = io::stdout();
    writeln!(out, "{WORKSPACE_QUESTION}").map_err(|error| error.to_string())?;
    writeln!(out, "1  {WORKSPACE_HERE}").map_err(|error| error.to_string())?;
    writeln!(out, "2  {WORKSPACE_WORKTREE}").map_err(|error| error.to_string())?;
    out.flush().map_err(|error| error.to_string())?;
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    Ok(tui::worktree_choice(&line))
}

async fn new_workspace(client: &Client, repo: Option<&str>) -> Result<String, String> {
    if repo.is_none() && !io::stdin().is_terminal() {
        return current_workspace();
    }
    let (status, response) = client.request("GET", "/v1/projects", None).await?;
    if status != 200 {
        return Err(unexpected(&response, status));
    }
    let projects: Vec<serde_json::Value> =
        serde_json::from_str(&response).map_err(|error| error.to_string())?;
    if let Some(id) = repo {
        return projects
            .iter()
            .find(|project| project["id"].as_str() == Some(id))
            .and_then(|project| project["path"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| format!("unknown repository {id}"));
    }
    if projects.is_empty() {
        return current_workspace();
    }
    let mut out = io::stdout();
    writeln!(out, "Which repository should this session use?")
        .map_err(|error| error.to_string())?;
    writeln!(out, "1  Current directory").map_err(|error| error.to_string())?;
    for (index, project) in projects.iter().enumerate() {
        let id = project["id"].as_str().unwrap_or("");
        let name = project["name"].as_str().unwrap_or(id);
        let path = project["path"].as_str().unwrap_or("");
        writeln!(out, "{}  {name} ({id})  {path}", index + 2).map_err(|error| error.to_string())?;
    }
    out.flush().map_err(|error| error.to_string())?;
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    if line.trim().is_empty() || line.trim() == "1" {
        return current_workspace();
    }
    line.trim()
        .parse::<usize>()
        .ok()
        .and_then(|number| number.checked_sub(2))
        .and_then(|index| projects.get(index))
        .and_then(|project| project["path"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| "invalid repository choice".to_string())
}

fn current_workspace() -> Result<String, String> {
    let workspace = std::env::current_dir().map_err(|error| error.to_string())?;
    let workspace = std::fs::canonicalize(&workspace).unwrap_or(workspace);
    Ok(workspace.display().to_string())
}

async fn new_session(
    repo: Option<String>,
    task: Option<String>,
    url: Option<String>,
) -> Result<(), String> {
    let client = api_client(url).await?;
    let workspace = new_workspace(&client, repo.as_deref()).await?;
    let worktree = ask_worktree()?;
    let body = serde_json::json!({
        "workspace": workspace,
        "worktree": worktree,
        "taskId": task
    });
    let (status, response) = client
        .request("POST", "/v1/sessions", Some(&body.to_string()))
        .await?;
    if status != 201 {
        return Err(unexpected(&response, status));
    }
    let json: serde_json::Value =
        serde_json::from_str(&response).map_err(|error| error.to_string())?;
    let id = json["id"].as_str().unwrap_or("");
    let workspace = json["workspace"].as_str().unwrap_or("");
    println!("Created session {id} for {workspace}");
    Ok(())
}

async fn list_repos(url: Option<String>) -> Result<(), String> {
    let client = api_client(url).await?;
    let (status, response) = client.request("GET", "/v1/projects", None).await?;
    if status != 200 {
        return Err(unexpected(&response, status));
    }
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&response).map_err(|error| error.to_string())?;
    for row in rows {
        let id = row["id"].as_str().unwrap_or("");
        let name = row["name"].as_str().unwrap_or("");
        let path = row["path"].as_str().unwrap_or("");
        println!("{id}\t{name}\t{path}");
    }
    Ok(())
}

/// Print one plain-text row per session.
async fn list_sessions(url: Option<String>) -> Result<(), String> {
    let client = api_client(url).await?;
    let (status, response) = client.request("GET", "/v1/sessions", None).await?;
    if status != 200 {
        return Err(unexpected(&response, status));
    }
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&response).map_err(|error| error.to_string())?;
    for row in &rows {
        let id = row["id"].as_str().unwrap_or("");
        let workspace = row["workspace"].as_str().unwrap_or("");
        let status = row["status"].as_str().unwrap_or("");
        match row["waiting"].as_str() {
            Some(waiting) => println!("{id} {workspace} {status} {waiting}"),
            None => println!("{id} {workspace} {status}"),
        }
    }
    Ok(())
}

/// Print the event log of a session: the one named, or the newest in this
/// directory.
async fn log(id: Option<String>, url: Option<String>) -> Result<(), String> {
    let client = api_client(url).await?;
    let id = session_id(&client, id).await?;
    let (status, response) = client
        .request("GET", &format!("/v1/sessions/{id}/events"), None)
        .await?;
    if status != 200 {
        return Err(unexpected(&response, status));
    }
    print!("{response}");
    Ok(())
}

/// Cancel the current turn of a session: the one named, or the newest in this
/// directory.
async fn cancel(id: Option<String>, url: Option<String>) -> Result<(), String> {
    let client = api_client(url).await?;
    let id = session_id(&client, id).await?;
    let (status, _) = client
        .request("POST", &format!("/v1/sessions/{id}/cancel"), None)
        .await?;
    if status != 204 {
        return Err(format!(
            "the cancel did not land: the server answered {status}"
        ));
    }
    println!("Cancelled {id}.");
    Ok(())
}

/// The id of the session to act on: the one named, or the newest session in
/// the current directory.
async fn session_id(client: &Client, id: Option<String>) -> Result<String, String> {
    if let Some(id) = id {
        return Ok(id);
    }
    let (_, response) = client.request("GET", "/v1/sessions", None).await?;
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&response).map_err(|error| error.to_string())?;
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    let current = std::fs::canonicalize(&current).unwrap_or(current);
    let mut best: Option<&serde_json::Value> = None;
    for row in &rows {
        let workspace = PathBuf::from(row["workspace"].as_str().unwrap_or(""));
        let same_dir = workspace == current
            || std::fs::canonicalize(&workspace)
                .map(|path| path == current)
                .unwrap_or(false);
        if !same_dir {
            continue;
        }
        let newer = match best {
            None => true,
            Some(best) => {
                row["createdAt"].as_str().unwrap_or("") > best["createdAt"].as_str().unwrap_or("")
            }
        };
        if newer {
            best = Some(row);
        }
    }
    best.and_then(|row| row["id"].as_str().map(str::to_string))
        .ok_or_else(|| "no session for this directory".to_string())
}

fn unexpected(response: &str, status: u16) -> String {
    let json: serde_json::Value = serde_json::from_str(response).unwrap_or(serde_json::Value::Null);
    let error = json["error"].as_str().unwrap_or("");
    if error.is_empty() {
        format!("the server answered {status}")
    } else {
        format!("the server answered {status}: {error}")
    }
}

async fn api_client(url: Option<String>) -> Result<Client, String> {
    match url {
        Some(url) => Client::connect_with(Some(url)).await,
        None => Client::configured(None),
    }
}
