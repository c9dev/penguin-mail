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
- "Mute this thread."
- "Send my draft to the landlord tomorrow at 8."
- "Reply to Theo with my Thanks template."
- "Unsubscribe me from Trail Notes."
- "Which newsletters do I get? Leave the three I never read."
- "What does the PDF Priya sent say about the deadline?"
- "What is Mara's address at Fernwood?"
- "Send the message to the landlord now instead of tomorrow."
- "What's stuck in the Outbox, and why?"
- "Push my reminder about the kite order to Friday."
- "Undo that."
- "Rename my Receipts label to Money/Receipts and make it green."
- "Add Priya Shah, priya@fernwood.example, to my contacts."
- "Always load images from Trail Notes."
- "Save this thread as an mbox in my Documents folder."

It reads mail with the same tools you use: it lists mailboxes, searches
Gmail, and reads conversations. It looks people up in your address book,
by name, address or organisation, and gets back their addresses, phone
number and organisation. It acts through the app too, so Ctrl+Z
undoes what it archived, trashed, flagged, muted, or marked. It can also
take a change back itself, yours or its own, from the same undo stack
Ctrl+Z reads, one change at a time, newest first.

It reads Send Later and the Outbox as their lists show them: why each
message waits and when it goes. It can send one now, cancel a scheduled
send, which puts the message back in Drafts, give it a new time, or
delete a stuck message from the Outbox. It lists your reminders, moves
one to a new time, or cancels it and puts the conversation back in the
inbox. It lists muted mail and your smart mailboxes, and unmutes.

It looks after what you keep, too: it renames, recolours and deletes
labels, changes and deletes smart mailboxes, saves and deletes templates,
adds and changes Google contacts, and keeps the list of senders whose
remote images load. Adding or changing a contact needs one more Google
permission, which Penguin Mail asks for the first time.

It exports mail as a file: conversations as one mbox file, which other
mail programs import, or one message as an .eml file. The file goes in
your Downloads folder unless you name another place. A file already in
Downloads keeps its name and the new one gets a number; a file you named
is replaced only after the question says so.

It reads an attachment as text when the file is plain text, a web page or
a PDF. PDFs need `pdftotext`, which the `poppler-utils` package installs;
other files, such as pictures, it can name but not read.

It lists the newsletters you have had in the last three months, with how
many messages each sender wrote and how their list lets go, so "the Figma
one" is enough to name. It leaves up to twenty of them in one go. A list
that only lets go through its own page has that page loaded out of sight,
filled in and pressed, the way the Unsubscribe button does it, and one
dialog names every list, the button it will press and the address it will
type. A list you untick there is left alone. One page takes up to 45
seconds, so twenty of them can take minutes. Each list comes back as
done, sent but unconfirmed, failed, opened in your browser for you to
finish, or declined.

## Writing and drafts

The assistant writes mail the way the composer does:

- "Forward Theo's kite plans to Ann, with the map he attached."
- "Send the minutes to the committee and Bcc the treasurer."
- "Email ~/Documents/lease.pdf to the landlord, signed and encrypted."
- "What drafts do I have?"
- "Add Bo to my draft about the fern swap and change the subject."
- "Delete the draft to the plumber."

A message it drafts opens in a composer for you to finish. One it sends
waits for you to allow it, and the question names every recipient, the
blind copies, the files, and whether it goes out signed or encrypted.
Signing and encrypting follow your settings under Preferences unless you
ask otherwise. It encrypts only when it has a key or certificate for
every recipient, and when one is missing it tells you whose; a Bcc stays
hidden inside an OpenPGP message.

It attaches a file from a message you have, or a file on this computer
that you name by its path. Before it reads a file from this computer it
shows you the whole path and waits for you to allow it. It will not
attach anything from a hidden folder, such as `~/.ssh` or `~/.gnupg`,
from the system folders, or from Penguin Mail's own data. It does not
forward an encrypted message or its files; forward those yourself from
the conversation.

It lists your drafts, changes one, and deletes one, asking before each
change. An encrypted draft stays encrypted to your own key when it goes
back to Gmail, and opening it may ask for your passphrase. Gmail deletes
a draft for good, with no copy in the Trash.

## Your calendar

The assistant reads and changes the primary Google calendar of each
account:

- "What's on my calendar next week?"
- "Find me an hour with nobody booked on Thursday or Friday."
- "Put the kite festival on my calendar, the 14th to the 16th."
- "Move the design crit to 3 pm and add Ann."
- "Cancel Friday's dentist appointment."
- "Say yes to Priya's roadmap review."

Times are your local time. Free time counts from 09:00 to 18:00 on
weekdays unless you ask for other hours or the weekend. Google tells the
guests about every event the assistant adds, changes or deletes.

The first time a calendar question comes up, Penguin Mail asks for
permission to use the calendar, and Google confirms it in your browser.
If the Google Cloud project Penguin Mail signs in with has the Calendar
API switched off, a dialog says so and opens the page that turns it on.

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
tools over MCP. It turns off Claude Code's own tools for files and the
shell, allows only the mail tools plus WebSearch and WebFetch while web
search is on, and uses `--permission-mode dontAsk`, so nothing outside that
list runs. Claude Code still reads your global
`~/.claude/CLAUDE.md` and runs your hooks, as it does in any session.

The model field lists what your `claude` install offers: its own default,
the aliases with the version each points at today, and the dated ids for
pinning one. Penguin Mail reads that list from the catalog the CLI keeps,
so it matches whatever version you have.

## Web search

The assistant can search the web and read a page, such as a link in a
message. Pick how under **Web Search** on the AI page:

- **Off**: no web tools for any model.
- **Claude's own only**, the default: Claude searches with Anthropic's
  tools, through the API or your subscription. A local model can read a
  page you point it at but cannot search.
- **Brave Search**: Claude still uses Anthropic's search, and a local
  model searches with Brave. Get a key at
  [brave.com/search/api](https://brave.com/search/api/) and paste it into
  **Brave Search API Key**. It goes into the keyring.
- **SearXNG**: a local model searches with a SearXNG server you run or
  trust. Enter its address, such as `http://localhost:8080`. Penguin Mail
  asks SearXNG for JSON, which a fresh install does not serve: add `json`
  under `search.formats` in its `settings.yml`.

**Test** runs one search and shows the first result's title, or what went
wrong.

A local model reads a page through Penguin Mail: it downloads up to 2 MB
in 20 seconds and reads the text, without scripts or styles. It will not
open an address on your computer or your home network, so a message cannot
send the model to your router. Neither tool asks before it runs, since both
only read. The model is told that search results and pages are written by
strangers and that it must not follow instructions in them.
## Add MCP servers

An MCP server gives the assistant tools from outside Penguin Mail, such as
your files or an issue tracker. Add one in Preferences, on the AI page,
under **MCP Servers**: press **+**, give it a short name, and choose how
Penguin Mail reaches it.

**Command** starts a program on this computer and talks to it over its
input and output. Type the command line the way the server's README
prints it. Quotes work as they do in a shell, and `NAME=value` words in
front set environment variables. The filesystem server, for one folder:

```
npx -y @modelcontextprotocol/server-filesystem /home/ana/Documents
```

Environment variables are saved in the settings file, so a server that
takes a secret there keeps it in that file.

**URL** reaches a server over HTTP. Paste its address, and its bearer
token if it takes one; the token goes to the keyring:

```
https://mcp.example.com/mcp
```

Penguin Mail cannot sign in to a server through a browser. A server that
needs that says so when you press **Test**.

**Test** connects, lists the server's tools, and says how many there are
or what went wrong. The server's row says the same once the assistant has
used it.

A server starts the first time the assistant needs it, stays running for
later questions, and stops when you turn it off, remove it, or quit
Penguin Mail. One that fails to start offers no tools, and its row says
why. The assistant sees each tool as `name__tool`, such as
`files__read_file`, and asks you before each call, showing the tool and
what it will send. **Always Allow** stops the asking for that tool, and
the **Always Allowed** list on the AI page takes that back.

Penguin Mail speaks MCP revision 2026-07-28, and the handshake of
2025-11-25 and earlier for the servers that still use it, over a command
or Streamable HTTP. With your Claude subscription, the tools reach Claude
Code through the same bridge as the mail tools, and the asking holds there
too.

## What it asks before doing

By default, the assistant asks you before it:

- sends mail, now or later,
- turns an automatic reply on or off,
- creates or deletes a Gmail filter,
- blocks a sender or sorts one into a category,
- creates a label that an account lacks, when it labels mail by name
  (say no, and it labels only the mail in accounts that have the label),
- moves more than 25 conversations to the Trash,
- deletes mail forever,
- sends a waiting message now, cancels a scheduled send, gives one a new
  time, or deletes one from the Outbox,
- cancels a reminder or moves it to a new time,
- takes back your last change,
- unsubscribes you from lists, which it asks about in a dialog of its
  own, whatever Ask Before Acting says, since loading a sender's page is
  something you cannot take back,
- adds, changes or deletes a calendar event,
- answers an invitation,
- renames, recolours or deletes a label, and says how many conversations
  carry a label it would delete,
- deletes a smart mailbox,
- saves a template, saying when it replaces one, or deletes one,
- adds or changes a Google contact,
- lets a sender's images load, or stops them,
- writes mail to a file, naming the file.

Deleting forever needs one more Google permission, which Penguin Mail
asks for the first time, as the Delete Forever button does.

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
translate, and only after you press the button. An MCP server you add gets
what the assistant sends it in each call, which can include text from your
mail.

A web search sends its words to the search engine in use: Anthropic for
Claude, or Brave or your SearXNG server for a local model. Those words come
from your request and can include what the assistant read in your mail.

Skill scripts run on this computer, in the sandbox described under
Scripts, and send nothing anywhere unless you allowed that skill the
network.

The assistant treats mail as data. Its instructions tell it never to follow
instructions written inside an email, and the approval cards stop a message
that tries to make it send mail.
