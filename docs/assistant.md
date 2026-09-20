# The assistant

Press Ctrl+J, or the sparkle button above the message list, to open the
assistant on the right. Ask it about your mail in plain words:

- "Summarize what came in today."
- "Archive every newsletter older than a week."
- "Flag anything from my landlord blue."
- "Draft a reply to Priya saying Friday works."
- "Turn on an out-of-office reply from Monday to Friday next week."
- "Which mail I sent is still waiting on an answer?"
- "Move everything from Trail Notes to Promotions."
- "Make me a Hide My Email address for the bike shop."

It reads mail with the same tools you use: it lists mailboxes, searches
Gmail, and reads conversations. It acts through the app too, so Ctrl+Z
undoes what it archived, trashed, flagged, or marked.

## Pick a model

Open Preferences (Ctrl+,) and go to the Assistant page. **Found on This
Computer** lists the servers and tools Penguin Mail found, each with a
**Use** button. You can also set one up by hand.

### LM Studio

1. In LM Studio, load a model and start the server on the Developer tab.
   It listens on `http://localhost:1234/v1`.
2. In Penguin Mail, choose **Local or OpenAI-compatible server**. The
   default URL already points at LM Studio. Pick the model from the list
   next to the Model field.

Pick a model trained for tool calls, such as Qwen 3, Llama 3.1 or newer, or
Mistral Small. A model without tool calls can still chat and summarize what
you paste, but it cannot touch your mail.

### Unsloth Studio

Unsloth Studio serves its API on port 8888 and asks for a key.

1. Copy the API key from Unsloth Studio.
2. Choose **Local or OpenAI-compatible server**, set the URL to
   `http://localhost:8888/v1`, and paste the key into **API Key
   (Optional)**.

Ollama (port 11434), llama.cpp's server (8080), and vLLM (8000) work the
same way. Any server that speaks the OpenAI chat completions API does.

### Claude with an API key

Choose **Anthropic API key** and paste a key from console.anthropic.com. The
default model is `claude-opus-5`. If `ANTHROPIC_API_KEY` is set when
Penguin Mail starts, the Found list offers it.

### Your Claude subscription

If Claude Code is installed and signed in, the Found list shows it. Press
**Use**, or choose **Claude subscription (Claude Code)**, and the assistant
runs on your Pro or Max plan with no API key. Penguin Mail looks for
`claude` on your PATH and in `~/.local/bin`.

Penguin Mail starts `claude -p` for each message and hands it the mail
tools over MCP. It turns off Claude Code's own tools (files, shell, web),
allows only the mail tools, and uses `--permission-mode dontAsk`, so
nothing outside that list runs. Claude Code still reads your global
`~/.claude/CLAUDE.md` and runs your hooks, as it does in any session.

The model field lists what your `claude` install offers: its own default,
the aliases with the version each points at today, and the dated ids for
pinning one. Penguin Mail reads that list from the catalog the CLI keeps,
so it matches whatever version you have.

## What it asks before doing

By default, the assistant asks you before it:

- sends mail,
- turns an automatic reply on or off,
- creates or deletes a Gmail filter,
- blocks a sender or sorts one into a category,
- moves more than 25 conversations to the Trash.

A card appears in the chat with the details and **Allow** and **Don't
Allow** buttons. Turn **Ask Before Acting** off under Safety to skip the
cards. Drafts always open in a composer window for you to send.

## Keys and privacy

API keys live in the GNOME keyring, under the service `mailrs-ai`. The
settings file never holds them.

A local server keeps your mail on your computer. The Anthropic API and
Claude Code send what the assistant reads to Anthropic. The assistant reads
only what a task needs, but that can be whole conversations.

The assistant treats mail as data. Its instructions tell it never to follow
instructions written inside an email, and the approval cards stop a message
that tries to make it send mail.
