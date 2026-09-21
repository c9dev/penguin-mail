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

## What it did

Under your question, the assistant's answer shows what the model did, in
the order it happened:

- **Thought for N seconds**: what the model thought before acting. Claude
  and reasoning models such as Qwen 3 and DeepSeek R1 send it; other
  models skip this row.
- One row per tool it ran, such as **Reading a mailbox** with what it
  asked for beside it, and a mark for done or failed.
- The reply.

Each row folds to one line. Click it, or press Space on it, to see the
thinking, or the tool's input and the result the model read. While the
model works, a line at the bottom says what it is doing now.

## Pick a model

Open Preferences (Ctrl+,) and go to the AI page. It has two groups:

- **Connections** holds each place a model can run: a local server, the
  Anthropic API, and your Claude subscription. Set up each one once, with
  its address, key, or command, and test it there.
- **Used For** has a row for each feature that sends words to a model: the
  assistant and translation. Each row picks a connection and a model on
  it. Translation starts on **Same as the Assistant**; pick a connection
  for it to run on a model of its own, such as a small, fast local model
  while the assistant runs on Claude. Choose **Off** to turn a feature off.

**Found on This Computer** lists the servers and tools Penguin Mail found,
each with a **Use** button that sets up the connection and puts the
assistant on it. You can also set one up by hand.

### LM Studio

1. In LM Studio, load a model and start the server on the Developer tab.
   It listens on `http://localhost:1234/v1`.
2. In Penguin Mail, choose **Local server** for the assistant. The
   default address already points at LM Studio. Pick the model from the
   list next to the Model field.

Pick a model trained for tool calls, such as Qwen 3, Llama 3.1 or newer, or
Mistral Small. A model without tool calls can still chat and summarize what
you paste, but it cannot touch your mail.

### Unsloth Studio

Unsloth Studio serves its API on port 8888 and asks for a key.

1. Copy the API key from Unsloth Studio.
2. Under **Local server** in Connections, set the address to
   `http://localhost:8888/v1` and paste the key into **API Key
   (Optional)**. Then choose **Local server** for the assistant.

Ollama (port 11434), llama.cpp's server (8080), and vLLM (8000) work the
same way. Any server that speaks the OpenAI chat completions API does.

### Claude with an API key

Paste a key from console.anthropic.com under **Anthropic API** in
Connections, then choose **Anthropic API** for the assistant. The default
model is `claude-opus-5`. If `ANTHROPIC_API_KEY` is set when
Penguin Mail starts, the Found list offers it.

### Your Claude subscription

If Claude Code is installed and signed in, the Found list shows it. Press
**Use**, or choose **Claude subscription**, and the assistant
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

## Skills

A skill is a folder of instructions for one kind of task, such as
totalling receipts or filling in a form. When a request fits a skill you
turned on, the assistant reads its instructions and follows them.

Each skill folder holds a `SKILL.md` that starts with a name and a
description between two `---` lines:

```markdown
---
name: receipts
description: Totals the receipts in a conversation and drafts an expense report.
---

Read each receipt in the conversation, add up the amounts by currency, and
draft a reply with the totals as a table.
```

Below the second `---` go the instructions themselves. Files beside
`SKILL.md`, such as reference notes or scripts, are for the instructions
to point at.

Penguin Mail reads skills from two folders:

- `~/.config/penguin-mail/skills/`, its own. **Open Folder** on the AI page
  creates it and opens it.
- `~/.claude/skills/`, where Claude Code keeps skills, so the ones you
  already have work here too. Links to skill folders elsewhere work.

Under **Skills** on the AI page, each skill has a switch. Every skill is
off until you turn it on, and a change applies from the next new chat.
The assistant sees the name and description of each skill you turned on,
and reads the rest only when a request calls for it.

### Scripts

A skill with scripts (a `scripts/` folder, or files such as `.py` and
`.sh`) can run commands. The assistant asks before each one and shows you
the exact command. The command runs in a sandbox made with
[bubblewrap](https://github.com/containers/bubblewrap), which must be
installed (`sudo apt install bubblewrap`). Inside the sandbox, the command:

- can read the system's programs and libraries, and the skill's own
  folder at `/skill`;
- can write only to `/work`, a scratch folder that lasts until you start
  a new chat, and to an empty `/tmp`;
- cannot see your home folder, so it has no access to your mail, your
  keys, your settings or any of your files;
- cannot reach the internet unless you turn on **Allow Network** for that
  skill;
- is stopped after 60 seconds, and the assistant reads at most 1 MB of
  what it prints.

The assistant can also feed a command text on standard input, such as a
message it read. A script sees only what the assistant hands it.

## Keys and privacy

API keys live in the GNOME keyring, under the service `mailrs-ai`. The
settings file never holds them.

A local server keeps your mail on your computer. The Anthropic API and
Claude Code send what a feature reads to Anthropic. Each feature sends to
the model chosen for it, so the assistant and translation can go to
different places. The assistant reads only what a task needs, but that can
be whole conversations. Translation sends the message you asked it to
translate, and only after you press the button.

Skill scripts run on this computer, in the sandbox described under
Scripts, and send nothing anywhere unless you allowed that skill the
network.

The assistant treats mail as data. Its instructions tell it never to follow
instructions written inside an email, and the approval cards stop a message
that tries to make it send mail.
