//! Throwaway mail servers in Docker, for the tests that talk to a real IMAP
//! or SMTP server: Dovecot for IMAP, and Mailpit as the SMTP sink.
//!
//! Each server listens on 127.0.0.1 at a port Docker picks, and presents a
//! certificate for `localhost` signed by a root made for the run. A test
//! trusts that root with [`trust`], which points `SSL_CERT_FILE` at it.
//! On Linux, rustls-native-certs reads that variable in place of the
//! system's roots, so the client checks the certificate and the host name
//! as it does for a person, and nothing in the client changes for a test.
//!
//! Without Docker or openssl, or when a container cannot start, a start
//! function returns `None` and the test skips. `PENGUIN_MAIL_REQUIRE_IMAP`
//! turns the skip into a failure, for a build machine that must not report
//! a suite it never ran.
//!
//! The crate is a dev-dependency and never reaches the app. It panics
//! where a test would: a doveadm call that fails is a broken test.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::Instant;

/// Dovecot 2.4.5, pinned by digest so a new image cannot change a run.
pub const DOVECOT_IMAGE: &str =
    "dovecot/dovecot:2.4.5@sha256:c807be4fb5a97d9c3a90770569d3a6c4cbdcb36742ad41f90409cbd929166553";

/// Mailpit 1.31.2, the SMTP sink, pinned the same way.
pub const MAILPIT_IMAGE: &str = "axllent/mailpit:v1.31.2@sha256:74d609a42ec279aa63c6b4622a6fa9b5408d1ad5b1d76a1c4be40a265ce0863d";

/// Set on a build machine, it makes a missing Docker fail the run.
pub const REQUIRE: &str = "PENGUIN_MAIL_REQUIRE_IMAP";

/// Every container a test starts carries this label, so one that a killed
/// run left behind shows in `docker ps -a --filter label=...`.
pub const LABEL: &str = "io.github.c9dev.penguin-mail.test";

/// How long a server gets to answer once its container starts. The first
/// run on a computer also pulls the image inside `docker create`, before
/// this clock starts.
const READY_WITHIN: Duration = Duration::from_secs(60);

// The Dovecot image runs unprivileged, so it listens above 1024.
const DOVECOT_IMAPS: u16 = 31993;
const DOVECOT_IMAP: u16 = 31143;
const MAILPIT_SMTP: u16 = 1025;
const MAILPIT_API: u16 = 8025;

/// Skips, or fails when [`REQUIRE`] is set. Returns `None` for the caller
/// to hand back.
pub fn unavailable<T>(why: &str) -> Option<T> {
    if std::env::var_os(REQUIRE).is_some() {
        panic!("{REQUIRE} is set and {why}, so these tests would have proved nothing");
    }
    eprintln!("skipping: {why}");
    None
}

/// Whether Docker answers. A test asks first, before it makes certificates
/// or starts a runtime.
pub fn docker() -> Option<()> {
    let answered = Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match answered {
        Ok(status) if status.success() => Some(()),
        Ok(_) => unavailable("Docker is installed and its daemon does not answer"),
        Err(_) => unavailable("Docker is not installed"),
    }
}

/// A password made for one run, so none sits in the tree.
pub fn password() -> String {
    format!("{:032x}", rand::random::<u128>())
}

/// Makes this process trust `root` and no other root for TLS.
///
/// # Safety
///
/// It changes the environment, which another thread may be reading. Call it
/// first in a test binary's only test, before a runtime or a client starts
/// a thread.
pub unsafe fn trust(root: &Path) {
    // SAFETY: the caller guarantees that no other thread reads or writes
    // the environment while this runs.
    unsafe {
        std::env::remove_var("SSL_CERT_DIR");
        std::env::set_var("SSL_CERT_FILE", root);
    }
}

/// A root made for the run, a certificate for `localhost` it signs, and a
/// second root that signs nothing, for a test that trusts the wrong one.
pub struct Certs {
    dir: tempfile::TempDir,
}

impl Certs {
    pub fn make() -> Option<Certs> {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let at = dir.path();
        std::fs::write(
            at.join("server.ext"),
            "subjectAltName=DNS:localhost\n\
             basicConstraints=CA:FALSE\n\
             keyUsage=critical,digitalSignature\n\
             extendedKeyUsage=serverAuth\n",
        )
        .expect("write the certificate extensions");
        let steps: [Vec<String>; 4] = [
            root_args("root"),
            root_args("stranger"),
            [
                "req",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:P-256",
                "-nodes",
                "-keyout",
                "tls.key",
                "-out",
                "tls.csr",
                "-subj",
                "/CN=localhost",
            ]
            .map(String::from)
            .to_vec(),
            [
                "x509",
                "-req",
                "-in",
                "tls.csr",
                "-CA",
                "root.crt",
                "-CAkey",
                "root.key",
                "-CAcreateserial",
                "-out",
                "tls.crt",
                "-days",
                "2",
                "-extfile",
                "server.ext",
            ]
            .map(String::from)
            .to_vec(),
        ];
        for args in &steps {
            let out = match Command::new("openssl")
                .args(args)
                .current_dir(at)
                .stdin(Stdio::null())
                .output()
            {
                Ok(out) => out,
                Err(_) => return unavailable("openssl is not installed"),
            };
            assert!(
                out.status.success(),
                "openssl {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        // The servers run as their own users inside the containers and must
        // read the key. The directory around it stays private to this user.
        std::fs::set_permissions(at.join("tls.key"), std::fs::Permissions::from_mode(0o644))
            .expect("let the servers read the key");
        Some(Certs { dir })
    }

    /// The root that signed the servers' certificate.
    pub fn root(&self) -> PathBuf {
        self.dir.path().join("root.crt")
    }

    /// A root that signed nothing the servers present.
    pub fn stranger(&self) -> PathBuf {
        self.dir.path().join("stranger.crt")
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

/// The openssl arguments that make a self-signed root called `name`. Two
/// days covers a run that starts before midnight in any zone; nothing keeps
/// these keys afterwards.
fn root_args(name: &str) -> Vec<String> {
    let mut args: Vec<String> = [
        "req",
        "-x509",
        "-newkey",
        "ec",
        "-pkeyopt",
        "ec_paramgen_curve:P-256",
        "-nodes",
        "-days",
        "2",
        "-addext",
        "basicConstraints=critical,CA:TRUE",
        "-addext",
        "keyUsage=critical,keyCertSign",
        "-subj",
    ]
    .map(String::from)
    .to_vec();
    args.push(format!("/CN=Penguin Mail test {name}"));
    args.extend([
        "-keyout".to_string(),
        format!("{name}.key"),
        "-out".to_string(),
        format!("{name}.crt"),
    ]);
    args
}

/// One container, removed by its id when dropped, whether the test passed
/// or not.
pub struct Container {
    id: String,
}

impl Container {
    /// Creates a container from `image`, copies each `(local, inside)` path
    /// into it, and starts it. `None`, after [`unavailable`], when Docker
    /// cannot.
    async fn start(
        options: &[String],
        image: &str,
        command: &[&str],
        files: &[(PathBuf, &str)],
    ) -> Option<Container> {
        let mut create = vec!["create".to_string(), "--label".into(), LABEL.into()];
        create.extend(options.iter().cloned());
        create.push(image.into());
        create.extend(command.iter().map(|arg| arg.to_string()));
        let created = docker_output(&create).await;
        if !created.status.success() {
            return unavailable(&format!(
                "docker could not create a container from {image}: {}",
                String::from_utf8_lossy(&created.stderr).trim()
            ));
        }
        let container = Container {
            id: String::from_utf8_lossy(&created.stdout).trim().to_string(),
        };
        for (local, inside) in files {
            let copied = docker_output(&[
                "cp".into(),
                local.display().to_string(),
                format!("{}:{inside}", container.id),
            ])
            .await;
            assert!(
                copied.status.success(),
                "docker cp {} failed: {}",
                local.display(),
                String::from_utf8_lossy(&copied.stderr)
            );
        }
        let started = docker_output(&["start".into(), container.id.clone()]).await;
        if !started.status.success() {
            return unavailable(&format!(
                "docker could not start a container from {image}: {}",
                String::from_utf8_lossy(&started.stderr).trim()
            ));
        }
        Some(container)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The host port Docker picked for `inner`.
    async fn port(&self, inner: u16) -> u16 {
        let out = docker_output(&["port".into(), self.id.clone(), format!("{inner}/tcp")]).await;
        let text = String::from_utf8_lossy(&out.stdout);
        // One line per address, such as `127.0.0.1:49153`.
        text.lines()
            .find_map(|line| line.rsplit_once(':')?.1.trim().parse().ok())
            .unwrap_or_else(|| panic!("docker names no host port for {inner}: {text:?}"))
    }

    async fn running(&self) -> bool {
        let out = docker_output(&[
            "inspect".into(),
            "--format".into(),
            "{{.State.Running}}".into(),
            self.id.clone(),
        ])
        .await;
        String::from_utf8_lossy(&out.stdout).trim() == "true"
    }

    async fn logs(&self) -> String {
        let out = docker_output(&["logs".into(), self.id.clone()]).await;
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }

    /// Fails the test when a server that stopped answering has exited,
    /// which means its configuration is wrong; skips when it runs but is
    /// slow, which is the computer's trouble.
    async fn not_ready<T>(&self, what: &str) -> Option<T> {
        if !self.running().await {
            panic!("{what} exited as it started:\n{}", self.logs().await);
        }
        unavailable(&format!(
            "{what} did not answer within {READY_WITHIN:?}:\n{}",
            self.logs().await
        ))
    }

    async fn exec(&self, args: &[&str], stdin: Option<&[u8]>) -> Output {
        let mut command = tokio::process::Command::new("docker");
        command.arg("exec");
        if stdin.is_some() {
            command.arg("-i");
        }
        command
            .arg(&self.id)
            .args(args)
            .stdin(match stdin {
                Some(_) => Stdio::piped(),
                None => Stdio::null(),
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("docker runs");
        if let Some(bytes) = stdin {
            let mut pipe = child.stdin.take().expect("a pipe to docker exec");
            pipe.write_all(bytes).await.expect("write to docker exec");
            // Closing the pipe is how doveadm save learns the message ended.
            drop(pipe);
        }
        child
            .wait_with_output()
            .await
            .expect("docker exec finishes")
    }
}

impl Drop for Container {
    fn drop(&mut self) {
        // Drop cannot await, and the container must go even while a failed
        // test unwinds, so this one call blocks.
        let removed = Command::new("docker")
            .args(["rm", "--force", "--volumes", &self.id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !matches!(removed, Ok(status) if status.success()) {
            eprintln!(
                "could not remove container {}; remove it with docker rm -f {}",
                self.id, self.id
            );
        }
    }
}

async fn docker_output(args: &[String]) -> Output {
    tokio::process::Command::new("docker")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .expect("docker runs")
}

/// What a Dovecot server offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// Everything Dovecot offers: SPECIAL-USE, CONDSTORE, QRESYNC, MOVE,
    /// UIDPLUS and IDLE, and an Archive mailbox.
    Full,
    /// CONDSTORE without QRESYNC, and an Archive mailbox.
    NoQresync,
    /// Neither CONDSTORE nor QRESYNC, no MOVE, no SPECIAL-USE and no
    /// Archive mailbox, so the client falls back on every path and knows
    /// the role mailboxes by their names alone.
    Bare,
}

impl Profile {
    pub const ALL: [Profile; 3] = [Profile::Full, Profile::NoQresync, Profile::Bare];

    /// Whether the server lists a mailbox with the `\Archive` flag.
    pub fn has_archive(self) -> bool {
        self != Profile::Bare
    }

    fn config(self) -> (&'static str, &'static str) {
        match self {
            Profile::Full => ("full.conf", include_str!("../dovecot/full.conf")),
            Profile::NoQresync => (
                "no-qresync.conf",
                include_str!("../dovecot/no-qresync.conf"),
            ),
            Profile::Bare => ("bare.conf", include_str!("../dovecot/bare.conf")),
        }
    }
}

/// A message as Dovecot holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub uid: u32,
    pub flags: Vec<String>,
    /// Without its angle brackets.
    pub message_id: String,
}

impl Held {
    pub fn has_flag(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }
}

/// Dovecot with IMAP over TLS and with STARTTLS. Every user name logs in
/// with the one password it started with, and gets an empty mailbox on
/// first use.
pub struct Dovecot {
    container: Container,
    /// The host port for IMAP over TLS.
    pub imaps: u16,
    /// The host port for IMAP with STARTTLS.
    pub imap: u16,
}

impl Dovecot {
    pub async fn start(certs: &Certs, profile: Profile, password: &str) -> Option<Dovecot> {
        let (name, text) = profile.config();
        let config = certs.file(name);
        std::fs::write(&config, text).expect("write the Dovecot profile");
        let container = Container::start(
            &[
                "-p".into(),
                format!("127.0.0.1::{DOVECOT_IMAPS}"),
                "-p".into(),
                format!("127.0.0.1::{DOVECOT_IMAP}"),
                // The image's static passdb takes this password for any
                // user name.
                "-e".into(),
                format!("USER_PASSWORD={password}"),
            ],
            DOVECOT_IMAGE,
            &[],
            &[
                (certs.file("tls.crt"), "/etc/dovecot/ssl/tls.crt"),
                (certs.file("tls.key"), "/etc/dovecot/ssl/tls.key"),
                (config, "/etc/dovecot/conf.d/zz-test.conf"),
            ],
        )
        .await?;
        let imap = container.port(DOVECOT_IMAP).await;
        let imaps = container.port(DOVECOT_IMAPS).await;
        if !greets(imap).await {
            return container.not_ready("Dovecot").await;
        }
        Some(Dovecot {
            container,
            imaps,
            imap,
        })
    }

    pub fn id(&self) -> &str {
        self.container.id()
    }

    /// Runs doveadm in the container and returns what it printed. The tests
    /// use it as a second client whose word they trust.
    pub async fn doveadm(&self, args: &[&str]) -> String {
        self.doveadm_with(args, None).await
    }

    async fn doveadm_with(&self, args: &[&str], stdin: Option<&[u8]>) -> String {
        let mut all = vec!["doveadm"];
        all.extend_from_slice(args);
        let out = self.container.exec(&all, stdin).await;
        assert!(
            out.status.success(),
            "doveadm {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    pub async fn create_mailbox(&self, user: &str, name: &str) {
        self.doveadm(&["mailbox", "create", "-u", user, name]).await;
    }

    /// Every mailbox that exists for `user`. Dovecot's defaults list Sent,
    /// Drafts, Trash and Junk before they exist, and those show here only
    /// once something made them.
    pub async fn mailboxes(&self, user: &str) -> Vec<String> {
        self.doveadm(&["mailbox", "list", "-u", user])
            .await
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect()
    }

    /// Delivers `raw` into `mailbox` as another client would, with
    /// `received`, in seconds since 1970, as its INTERNALDATE when given.
    pub async fn save(&self, user: &str, mailbox: &str, raw: &[u8], received: Option<i64>) {
        let received = received.map(|at| at.to_string());
        let mut args = vec!["save", "-u", user, "-m", mailbox];
        if let Some(at) = &received {
            args.extend(["--received-date", at.as_str()]);
        }
        self.doveadm_with(&args, Some(raw)).await;
    }

    /// Every message in `mailbox`. Asking about a mailbox that is listed
    /// but not yet made makes it, as any client's SELECT would.
    pub async fn messages(&self, user: &str, mailbox: &str) -> Vec<Held> {
        let out = self
            .doveadm(&[
                "-f",
                "tab",
                "fetch",
                "-u",
                user,
                "uid flags hdr.message-id",
                "mailbox",
                mailbox,
            ])
            .await;
        out.lines()
            .skip(1)
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let mut columns = line.split('\t');
                let uid = columns
                    .next()
                    .and_then(|uid| uid.trim().parse().ok())
                    .unwrap_or_else(|| panic!("no UID in {line:?}"));
                let flags = columns
                    .next()
                    .unwrap_or_default()
                    .split_whitespace()
                    .map(String::from)
                    .collect();
                let message_id = columns
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string();
                Held {
                    uid,
                    flags,
                    message_id,
                }
            })
            .collect()
    }

    /// The message in `mailbox` whose Message-ID, without angle brackets, is
    /// `message_id`.
    pub async fn find(&self, user: &str, mailbox: &str, message_id: &str) -> Option<Held> {
        self.messages(user, mailbox)
            .await
            .into_iter()
            .find(|held| held.message_id == message_id)
    }

    pub async fn uidvalidity(&self, user: &str, mailbox: &str) -> u32 {
        let out = self
            .doveadm(&[
                "-f",
                "tab",
                "mailbox",
                "status",
                "-u",
                user,
                "uidvalidity",
                mailbox,
            ])
            .await;
        // A header line, then one line of values; doveadm picks the column
        // order itself.
        let mut lines = out.lines();
        let header: Vec<&str> = lines.next().unwrap_or_default().split('\t').collect();
        let values: Vec<&str> = lines.next().unwrap_or_default().split('\t').collect();
        header
            .iter()
            .position(|name| name.trim() == "uidvalidity")
            .and_then(|at| values.get(at))
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or_else(|| panic!("no UIDVALIDITY for {mailbox} in {out:?}"))
    }

    /// Gives `mailbox` a new UIDVALIDITY, as a server does when it rebuilds
    /// a mailbox, which voids every UID a client holds for it.
    pub async fn set_uidvalidity(&self, user: &str, mailbox: &str, value: u32) {
        let value = value.to_string();
        self.doveadm(&[
            "mailbox",
            "update",
            "-u",
            user,
            "--uid-validity",
            &value,
            mailbox,
        ])
        .await;
    }

    /// Adds `flags`, space-separated, to one message.
    pub async fn add_flags(&self, user: &str, mailbox: &str, uid: u32, flags: &str) {
        let uid = uid.to_string();
        self.doveadm(&[
            "flags", "add", "-u", user, flags, "mailbox", mailbox, "uid", &uid,
        ])
        .await;
    }

    pub async fn expunge(&self, user: &str, mailbox: &str, uid: u32) {
        let uid = uid.to_string();
        self.doveadm(&["expunge", "-u", user, "mailbox", mailbox, "uid", &uid])
            .await;
    }

    /// How many IMAP connections `user` holds open now.
    pub async fn connections(&self, user: &str) -> usize {
        self.doveadm(&["-f", "tab", "who", "-1"])
            .await
            .lines()
            .skip(1)
            .filter(|line| line.split('\t').next() == Some(user))
            .count()
    }
}

/// Whether an IMAP server on `port` greets before [`READY_WITHIN`] passes.
/// Docker's proxy accepts a connection before the server inside listens
/// and then closes it, so a connection alone proves nothing.
async fn greets(port: u16) -> bool {
    let deadline = Instant::now() + READY_WITHIN;
    while Instant::now() < deadline {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)).await {
            let mut greeting = [0u8; 4];
            let read =
                tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut greeting))
                    .await;
            if matches!(read, Ok(Ok(_))) && &greeting == b"* OK" {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

/// How the sink takes a submission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Submission {
    /// TLS from the first byte, as on port 465.
    Tls,
    /// Plain text until STARTTLS, which it requires, as on port 587.
    StartTls,
}

/// A message the sink caught.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caught {
    pub subject: String,
    pub from: String,
    pub to: Vec<String>,
    /// Envelope recipients that no header names.
    pub bcc: Vec<String>,
}

/// Mailpit, an SMTP server that keeps what it receives and answers
/// questions about it over HTTP. It takes one user name and password.
pub struct Mailpit {
    container: Container,
    /// The host port for SMTP.
    pub smtp: u16,
    api: u16,
    http: reqwest::Client,
}

impl Mailpit {
    pub async fn start(
        certs: &Certs,
        submission: Submission,
        user: &str,
        password: &str,
    ) -> Option<Mailpit> {
        let (folder, require) = match submission {
            Submission::Tls => ("mailpit-tls", "--smtp-require-tls"),
            Submission::StartTls => ("mailpit-starttls", "--smtp-require-starttls"),
        };
        let dir = certs.file(folder);
        std::fs::create_dir_all(&dir).expect("make the sink's folder");
        for name in ["tls.crt", "tls.key"] {
            std::fs::copy(certs.file(name), dir.join(name)).expect("copy the certificate");
        }
        std::fs::write(dir.join("auth"), format!("{user}:{password}\n"))
            .expect("write the sink's password file");
        for name in ["tls.key", "auth"] {
            std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(0o644))
                .expect("let the sink read its files");
        }
        let container = Container::start(
            &[
                "-p".into(),
                format!("127.0.0.1::{MAILPIT_SMTP}"),
                "-p".into(),
                format!("127.0.0.1::{MAILPIT_API}"),
            ],
            MAILPIT_IMAGE,
            &[
                "--smtp-tls-cert",
                "/certs/tls.crt",
                "--smtp-tls-key",
                "/certs/tls.key",
                require,
                "--smtp-auth-file",
                "/certs/auth",
                "--smtp-disable-rdns",
            ],
            &[(dir, "/certs")],
        )
        .await?;
        let mailpit = Mailpit {
            smtp: container.port(MAILPIT_SMTP).await,
            api: container.port(MAILPIT_API).await,
            container,
            http: reqwest::Client::new(),
        };
        if !mailpit.answers().await {
            return mailpit.container.not_ready("Mailpit").await;
        }
        Some(mailpit)
    }

    pub fn id(&self) -> &str {
        self.container.id()
    }

    async fn answers(&self) -> bool {
        let deadline = Instant::now() + READY_WITHIN;
        while Instant::now() < deadline {
            let info = self
                .http
                .get(format!("http://127.0.0.1:{}/api/v1/info", self.api))
                .send()
                .await;
            if matches!(info, Ok(response) if response.status().is_success()) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        false
    }

    async fn get(&self, path: &str, query: &[(&str, String)]) -> serde_json::Value {
        self.http
            .get(format!("http://127.0.0.1:{}{path}", self.api))
            .query(query)
            .send()
            .await
            .expect("the sink answers")
            .error_for_status()
            .expect("the sink takes the request")
            .json()
            .await
            .expect("JSON from the sink")
    }

    /// How many messages the sink holds.
    pub async fn count(&self) -> u64 {
        self.get("/api/v1/messages", &[]).await["total"]
            .as_u64()
            .unwrap_or_default()
    }

    /// The message whose Message-ID, without angle brackets, is
    /// `message_id`. The sink files a message a moment after its SMTP
    /// answer, so this waits up to ten seconds for it.
    pub async fn find(&self, message_id: &str) -> Option<Caught> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let found = self
                .get(
                    "/api/v1/search",
                    &[("query", format!("message-id:{message_id}"))],
                )
                .await;
            if let Some(message) = found["messages"].as_array().and_then(|all| all.first()) {
                return Some(Caught {
                    subject: message["Subject"].as_str().unwrap_or_default().to_string(),
                    from: message["From"]["Address"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    to: addresses(&message["To"]),
                    bcc: addresses(&message["Bcc"]),
                });
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

fn addresses(list: &serde_json::Value) -> Vec<String> {
    list.as_array()
        .map(|all| {
            all.iter()
                .filter_map(|one| one["Address"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}
