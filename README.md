# deepseek-code

A small coding agent for DeepSeek models, with a VSCode front end.

This is a personal experiment, not a product. I built it to see how well
DeepSeek's models, `deepseek-flash` in particular, hold up as a coding agent:
reading files, editing them, running commands, and deciding when to ask. It
later became the place where I try out [nanosandbox](https://github.com/Erio-Harrison/nanosandbox)
on real agent workloads.

It works on my machine (macOS) and is shared as-is. Expect rough edges, and
don't expect releases or support.

## What's here

- **Agent core** (Rust, `src/`): a stdio process that talks to the DeepSeek
  chat API, runs tools, and asks its host for permission before each tool
  call. The protocol is newline-delimited JSON, in the spirit of the
  stream-json mode some coding agents expose for embedding.
- **VSCode extension** (`vscode-extension/`): a chat panel that hosts the
  core, with multiple sessions, streaming, Markdown replies, diffs for
  proposed writes, and a model picker.

Tools: `read_file`, `write_file`, `bash`.

## Sandbox

On macOS, `bash` and `write_file` run inside nanosandbox:

- writes are limited to the workspace and the system temp directories;
- no network;
- credentials and shell history in `~` are unreadable;
- only `PATH` and `HOME` come through from the host environment.

When a call fails because of the sandbox, the model may retry it with
`escalate: true` and a reason. The user then decides: allow once, allow for
the rest of the session, or deny.

On other platforms tools run unsandboxed, gated only by the permission
prompt. `read_file` is never sandboxed.

### Settings

Standing grants live in `~/.config/deepseek-code/settings.json` (or under
`$XDG_CONFIG_HOME`). The agent reads this file and never writes it. It lives
outside the workspace so the sandboxed agent can't edit its own permissions.

```json
{
  "sandbox": { "allowWrite": ["~/.cache/build"] },
  "projects": {
    "~/src/some-project": { "sandbox": { "allowUnsandboxedBash": true } }
  }
}
```

The top-level `sandbox` applies everywhere; a `projects` entry adds to it for
that workspace.

## Running it

You need Rust, Node.js, and a DeepSeek API key.

```bash
cargo build
```

**From VSCode:** open `vscode-extension/`, run `npm install && npm run compile`,
then press F5. The first message asks for your API key and keeps it in
VSCode's secret storage (change it later with "DeepSeek Code: Set API Key").
The extension uses `../target/debug/deepseek-code` unless
`deepseekCode.binaryPath` says otherwise.

**From a terminal:**

```bash
export DEEPSEEK_API_KEY=sk-...
export DEEPSEEK_MODEL=deepseek-flash   # optional, default deepseek-chat
cat scripts/test_input_plain.jsonl | ./target/debug/deepseek-code
```

## Protocol

Host to agent:

```json
{"type":"user","message":{"role":"user","content":"..."}}
{"type":"control_response","request_id":"req_1","subtype":"success","decision":{"behavior":"allow"}}
```

Agent to host: `text_delta`, `assistant` (with `stop_reason` `tool_use` or
`end_turn`), `control_request` (one per tool call, answered with
`control_response`), and `tool_result`. A decision may carry
`"remember":"session"`. See `src/protocol.rs` for the full shapes.

## Tests

```bash
cargo test                 # offline
cargo test -- --ignored    # hits the live API; needs DEEPSEEK_API_KEY
```

## License

MIT. See [LICENSE](LICENSE). The vendored `marked` and `DOMPurify` keep their
own licenses, in `vscode-extension/media/vendor/`.
