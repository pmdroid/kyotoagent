//! `kyotoagent` is a small coding agent that runs on a remote machine and shows
//! only what matters: what it asked, what it needs from you, what it did, and
//! the proof that it did it.
//!
//! The crate starts with the quiet screen. [`screen`] holds the plain screen
//! model and the one [`render`](screen::render) function that draws it, and
//! [`mock`] holds the three hardcoded states. The golden frames in
//! `tests/screens` and the `screens` example are both drawn from those, so the
//! live TUI later keeps the same layout.
//!
//! Underneath the screen is a session on disk. [`session`] is one directory
//! holding `meta.json` and an append-only `events.jsonl`, [`events`] is what a
//! line of that log is made of, and [`view`] projects the log to the cards the
//! screen draws. The projection is the point: a tool call and its result are in
//! the log for the model, and on no screen, so a file the agent read quietly
//! leaves nothing behind.
//!
//! Over the log is the half of the agent that touches the machine.
//! [`tools`] holds `read_file`, `list_dir`, `write_file`, and `run` against one
//! workspace, and [`permit`] holds the gate they stop at: a write, a read or a
//! listing outside the workspace, and every command become a card and wait for
//! an answer, while a read inside the workspace runs and leaves no trace.
//!
//! The model itself is two modules. [`config`] is `~/.kyotoagent/config.toml`: named
//! OpenAI-compatible providers, the one currently selected, and the *name* of
//! the environment variable holding the key, never the key itself. [`chat`] is
//! the one request: a non-streaming completion to `{base_url}/chat/completions`
//! that comes back as text or as tool calls. A server with no key configured is
//! not an error, so one file covers both a hosted model and Ollama on the same
//! machine.
//!
//! Over the client is the turn loop. [`prompt`] builds the short system prompt
//! from the workspace, the tool rules, and the skill index.
//! [`turn`] runs one ask as a tool loop against the chat client: the model
//! sees every tool call and tool result, a write or a command waits on the
//! permission gate, and [`hooks`] runs the command hooks from `.agents` before
//! a write or a command, after it, and when finish would stop.
//!
//! Over the loop is the server. [`server`] speaks HTTP on a unix socket under
//! `~/.kyotoagent`, and the same JSON API over HTTPS with HTTP/2 when `listen` is
//! set. The TUI uses the socket, or `KYOTOAGENT_URL` / `--url` for the HTTPS
//! address.

pub mod agents_doc;
pub mod auth;
pub mod chat;
pub mod closeout;
pub mod compact;
pub mod config;
pub mod doctor;
pub mod events;
pub mod goal;
pub mod hooks;
pub mod mock;
pub mod pairing;
pub mod permit;
pub mod prompt;
pub mod proof;
pub mod schedule;
pub mod screen;
pub mod server;
pub mod session;
pub mod skills;
pub mod splash;
pub mod subagent;
pub mod task;
pub mod tools;
pub mod tui;
pub mod turn;
pub mod view;
mod web;

pub mod attachment;

mod image_preview;
