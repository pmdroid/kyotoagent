# Kyoto Agent

<p align="center">
  <img src="assets/kyoto-logo.png" alt="Kyoto Agent" width="360" />
</p>

The agent that focuses on what you build, not what it does.

Kyoto runs on the machine with your code. Drive it from a terminal or connect
remotely. See your requests, questions, permissions, results, and the checks
that prove the work is done.

## Quick start

Install the stable [Rust toolchain](https://rustup.rs/), then build:

```sh
git clone https://github.com/pmdroid/kyotoagent.git
cd kyotoagent
cargo install --path .
```

[Configure a model provider](docs/model.md), or sign in through **Providers**
in the command palette after opening Kyoto Agent.

Start the server:

```sh
kyotoagent serve
```

In a second terminal, open your project:

```sh
cd /path/to/your/project
kyotoagent
```

Press **Ctrl-K** and choose **Providers** to sign in to your provider.
Press **Ctrl-T** to start a session, then describe what you want to build.
Kyoto asks before writing files or running commands. **Ctrl-C** detaches;
the server keeps running.

## Documentation

[Read the guides](docs/README.md) for setup, model configuration, terminal
controls, the server API, and development. Configuration lives in
`~/.kyotoagent/config.toml`.

## Website

The landing page and searchable docs use Astro and Starlight. With Node.js
22.12 or later:

```sh
cd site
npm ci
npm run dev
```

See the [website guide](docs/website.md) for builds and deployment.
