-- Gmail's labels become server mailboxes with integer keys, keywords and
-- categories, which fit Gmail's labels and IMAP's folders alike. The
-- mapping spells out mailrs_domain::gmail: six labels carry a role;
-- STARRED and MUTE stand for $flagged and $muted; UNREAD stands for the
-- absence of $seen; CATEGORY_* are categories; every other label is a
-- mailbox. Comparisons use substr rather than LIKE because LIKE ignores
-- case and Gmail's ids do not.

CREATE TABLE mailboxes (
    key        INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    id         TEXT NOT NULL,
    name       TEXT NOT NULL,
    parent     TEXT,
    role       TEXT,
    kind       TEXT NOT NULL,
    color      TEXT,
    hidden     INTEGER NOT NULL DEFAULT 0,
    -- 1 for a mailbox the server listed, 0 for one the store met on a
    -- message before any listing named it. Only listed ones are shown.
    named      INTEGER NOT NULL DEFAULT 1,
    UNIQUE (account_id, id)
);
CREATE INDEX mailboxes_by_role ON mailboxes(role, account_id) WHERE role IS NOT NULL;
CREATE INDEX mailboxes_by_id ON mailboxes(id);

CREATE TABLE message_mailboxes (
    account_id INTEGER NOT NULL,
    message_id TEXT NOT NULL,
    mailbox    INTEGER NOT NULL REFERENCES mailboxes(key) ON DELETE CASCADE,
    PRIMARY KEY (account_id, message_id, mailbox),
    FOREIGN KEY (account_id, message_id) REFERENCES messages(account_id, id) ON DELETE CASCADE
);
CREATE INDEX message_mailboxes_by_mailbox ON message_mailboxes(mailbox, account_id, message_id);

-- One row per thread and mailbox. `listed` applies the rule that hides a
-- thread whose every message in the mailbox also sits in the Trash or in
-- Spam (other than the mailbox itself), and `unread` copies the thread's,
-- so lists and counts read the partial index and nothing else.
CREATE TABLE thread_mailboxes (
    account_id INTEGER NOT NULL,
    thread_id  TEXT NOT NULL,
    mailbox    INTEGER NOT NULL REFERENCES mailboxes(key) ON DELETE CASCADE,
    listed     INTEGER NOT NULL,
    unread     INTEGER NOT NULL,
    PRIMARY KEY (account_id, thread_id, mailbox),
    FOREIGN KEY (account_id, thread_id) REFERENCES threads(account_id, id) ON DELETE CASCADE
);
CREATE INDEX thread_mailboxes_listed ON thread_mailboxes(mailbox, account_id, thread_id, unread) WHERE listed = 1;

-- `local` marks a keyword the server cannot store; it never syncs.
CREATE TABLE message_keywords (
    account_id INTEGER NOT NULL,
    message_id TEXT NOT NULL,
    keyword    TEXT NOT NULL,
    local      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account_id, message_id, keyword),
    FOREIGN KEY (account_id, message_id) REFERENCES messages(account_id, id) ON DELETE CASCADE
);
CREATE INDEX message_keywords_by_keyword ON message_keywords(keyword, account_id, message_id);

CREATE TABLE message_categories (
    account_id INTEGER NOT NULL,
    message_id TEXT NOT NULL,
    category   TEXT NOT NULL,
    PRIMARY KEY (account_id, message_id, category),
    FOREIGN KEY (account_id, message_id) REFERENCES messages(account_id, id) ON DELETE CASCADE
);
CREATE INDEX message_categories_by_category ON message_categories(category, account_id, message_id);

CREATE TABLE thread_categories (
    account_id INTEGER NOT NULL,
    thread_id  TEXT NOT NULL,
    category   TEXT NOT NULL,
    listed     INTEGER NOT NULL,
    unread     INTEGER NOT NULL,
    PRIMARY KEY (account_id, thread_id, category),
    FOREIGN KEY (account_id, thread_id) REFERENCES threads(account_id, id) ON DELETE CASCADE
);
CREATE INDEX thread_categories_by_category ON thread_categories(category, account_id, thread_id);

-- What the server calls each message. For Gmail it is the store's own id;
-- for IMAP it will be the mailbox, UIDVALIDITY and UID, which change when
-- a message moves while the store's id does not.
CREATE TABLE remote_refs (
    account_id  INTEGER NOT NULL,
    message_id  TEXT NOT NULL,
    remote      TEXT NOT NULL,
    mailbox     TEXT,
    uidvalidity INTEGER,
    uid         INTEGER,
    PRIMARY KEY (account_id, message_id),
    UNIQUE (account_id, remote),
    FOREIGN KEY (account_id, message_id) REFERENCES messages(account_id, id) ON DELETE CASCADE
);

ALTER TABLE messages ADD COLUMN seen INTEGER NOT NULL DEFAULT 1;
ALTER TABLE threads ADD COLUMN muted INTEGER NOT NULL DEFAULT 0;
ALTER TABLE threads ADD COLUMN listed INTEGER NOT NULL DEFAULT 1;
ALTER TABLE accounts ADD COLUMN provider TEXT NOT NULL DEFAULT 'gmail';
ALTER TABLE accounts ADD COLUMN sync_state TEXT;

INSERT INTO mailboxes (account_id, id, name, role, kind, color)
SELECT account_id, id, name,
       CASE id WHEN 'INBOX' THEN 'inbox' WHEN 'SENT' THEN 'sent' WHEN 'DRAFT' THEN 'drafts'
               WHEN 'TRASH' THEN 'trash' WHEN 'SPAM' THEN 'junk' WHEN 'IMPORTANT' THEN 'important' END,
       CASE kind WHEN 'user' THEN 'label' ELSE 'system' END,
       color
FROM labels;

INSERT INTO mailboxes (account_id, id, name, role, kind, named)
SELECT DISTINCT ml.account_id, ml.label_id, ml.label_id,
       CASE ml.label_id WHEN 'INBOX' THEN 'inbox' WHEN 'SENT' THEN 'sent' WHEN 'DRAFT' THEN 'drafts'
                        WHEN 'TRASH' THEN 'trash' WHEN 'SPAM' THEN 'junk' WHEN 'IMPORTANT' THEN 'important' END,
       CASE WHEN substr(ml.label_id, 1, 6) = 'Label_' THEN 'label' ELSE 'system' END,
       0
FROM message_labels ml
WHERE ml.label_id NOT IN ('UNREAD', 'STARRED', 'MUTE')
  AND substr(ml.label_id, 1, 9) <> 'CATEGORY_'
  AND NOT EXISTS (SELECT 1 FROM labels l WHERE l.account_id = ml.account_id AND l.id = ml.label_id);

INSERT INTO message_mailboxes (account_id, message_id, mailbox)
SELECT ml.account_id, ml.message_id, b.key
FROM message_labels ml
JOIN mailboxes b ON b.account_id = ml.account_id AND b.id = ml.label_id
WHERE ml.label_id NOT IN ('UNREAD', 'STARRED', 'MUTE')
  AND substr(ml.label_id, 1, 9) <> 'CATEGORY_';

INSERT INTO message_keywords (account_id, message_id, keyword)
SELECT m.account_id, m.id, '$seen' FROM messages m
WHERE NOT EXISTS (SELECT 1 FROM message_labels u
                  WHERE u.account_id = m.account_id AND u.message_id = m.id AND u.label_id = 'UNREAD');
INSERT INTO message_keywords (account_id, message_id, keyword)
SELECT account_id, message_id, CASE label_id WHEN 'STARRED' THEN '$flagged' ELSE '$muted' END
FROM message_labels WHERE label_id IN ('STARRED', 'MUTE');

INSERT INTO message_categories (account_id, message_id, category)
SELECT account_id, message_id, label_id FROM message_labels
WHERE substr(label_id, 1, 9) = 'CATEGORY_';

UPDATE messages SET seen = EXISTS (
    SELECT 1 FROM message_keywords k
    WHERE k.account_id = messages.account_id AND k.message_id = messages.id AND k.keyword = '$seen');

UPDATE threads SET
    muted = EXISTS (
        SELECT 1 FROM messages m
        JOIN message_keywords k ON k.account_id = m.account_id AND k.message_id = m.id AND k.keyword = '$muted'
        WHERE m.account_id = threads.account_id AND m.thread_id = threads.id),
    listed = EXISTS (
        SELECT 1 FROM messages m
        WHERE m.account_id = threads.account_id AND m.thread_id = threads.id
          AND NOT EXISTS (SELECT 1 FROM message_mailboxes h JOIN mailboxes hb ON hb.key = h.mailbox
                          WHERE h.account_id = m.account_id AND h.message_id = m.id
                            AND hb.role IN ('trash', 'junk')));

INSERT INTO thread_mailboxes (account_id, thread_id, mailbox, listed, unread)
SELECT m.account_id, m.thread_id, l.mailbox,
       MAX(NOT EXISTS (SELECT 1 FROM message_mailboxes h JOIN mailboxes hb ON hb.key = h.mailbox
                       WHERE h.account_id = l.account_id AND h.message_id = l.message_id
                         AND h.mailbox <> l.mailbox AND hb.role IN ('trash', 'junk'))),
       t.unread
FROM messages m
JOIN message_mailboxes l ON l.account_id = m.account_id AND l.message_id = m.id
JOIN threads t ON t.account_id = m.account_id AND t.id = m.thread_id
GROUP BY m.account_id, m.thread_id, l.mailbox;

INSERT INTO thread_categories (account_id, thread_id, category, listed, unread)
SELECT m.account_id, m.thread_id, c.category,
       MAX(NOT EXISTS (SELECT 1 FROM message_mailboxes h JOIN mailboxes hb ON hb.key = h.mailbox
                       WHERE h.account_id = c.account_id AND h.message_id = c.message_id
                         AND hb.role IN ('trash', 'junk'))),
       t.unread
FROM messages m
JOIN message_categories c ON c.account_id = m.account_id AND c.message_id = m.id
JOIN threads t ON t.account_id = m.account_id AND t.id = m.thread_id
GROUP BY m.account_id, m.thread_id, c.category;

INSERT INTO remote_refs (account_id, message_id, remote)
SELECT account_id, id, id FROM messages;

UPDATE accounts SET sync_state = json_object('history_id', history_id) WHERE history_id IS NOT NULL;
ALTER TABLE accounts DROP COLUMN history_id;

-- Unread and starred stay positive facts with partial indexes: an
-- anti-join on "no $seen" measured 12.4 ms where this reads 0.12 ms.
CREATE INDEX messages_unseen ON messages(account_id, id) WHERE seen = 0;
CREATE INDEX threads_unread ON threads(account_id, id) WHERE unread = 1;
CREATE INDEX threads_starred ON threads(account_id, id) WHERE starred = 1;

DROP TABLE thread_labels;
DROP TABLE message_labels;
DROP TABLE labels;
