//! Running one command inside an AppContainer.
//!
//! Windows has no bubblewrap; the matching kernel isolation is an
//! AppContainer. The command runs with a package SID that has no rights
//! anywhere by default: ACL grants open only the skill's folder, read-only,
//! and the conversation's scratch folder, read-write. It sees no home
//! folder, mail, keys or display, and no network unless the skill is
//! allowed one, which adds the internetClient capability. A job object
//! kills the whole tree when the time runs out, however the processes
//! detached themselves.
//!
//! Cygwin and MSYS2 shells cannot start in an AppContainer: their shared
//! memory object lives in the global namespace, which the container
//! denies. The shell is a native Windows busybox build instead, found
//! beside the app binary, on PATH, or through PENGUIN_MAIL_BUSYBOX.

use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, GENERIC_ALL, GENERIC_EXECUTE, GENERIC_READ,
    HANDLE, HANDLE_FLAG_INHERIT, HLOCAL, LocalFree, SetHandleInformation, WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSidToSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW,
    NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
    TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, FreeSid, PSECURITY_DESCRIPTOR, PSID, SECURITY_CAPABILITIES,
    SID_AND_ATTRIBUTES, SUB_CONTAINERS_AND_OBJECTS_INHERIT,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, STARTUPINFOW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows::core::{HRESULT, PCWSTR, PWSTR};

use super::{Jail, Limits, Run, collect};

/// The profile the package SID derives from. Deriving is pure math, so the
/// same name always gives the same SID.
const PROFILE: &str = "io.github.c9dev.PenguinMail.Skills";

/// internetClient: the capability a networked skill gets.
const INTERNET_CLIENT: &str = "S-1-15-3-1";

/// The jail's paths, owned for the blocking run.
struct Owned {
    program: PathBuf,
    skill: PathBuf,
    work: PathBuf,
    network: bool,
}

/// Runs `sh` on the command written to a script file inside the work
/// folder, so no quoting ever reaches a command line.
pub async fn run(
    jail: &Jail<'_>,
    command: &str,
    stdin: Option<&str>,
    limits: Limits,
) -> std::io::Result<Run> {
    let jail = Owned {
        program: jail.program.to_path_buf(),
        skill: jail.skill.to_path_buf(),
        work: jail.work.to_path_buf(),
        network: jail.network,
    };
    let command = command.to_string();
    let stdin = stdin.map(str::to_string);
    tokio::task::spawn_blocking(move || run_sync(&jail, &command, stdin.as_deref(), limits))
        .await
        .map_err(std::io::Error::other)?
}

fn run_sync(
    jail: &Owned,
    command: &str,
    stdin: Option<&str>,
    limits: Limits,
) -> std::io::Result<Run> {
    ensure_profile()?;
    let package = package_sid()?;
    grant(&package, &jail.skill, GENERIC_READ.0 | GENERIC_EXECUTE.0)?;
    grant(&package, &jail.work, GENERIC_ALL.0)?;
    let folder = jail
        .program
        .parent()
        .ok_or_else(|| std::io::Error::other("the shell has no folder"))?;
    // The program itself, not its folder: an inheritable grant on a build
    // tree would rewrite every file's ACL in it.
    grant(&package, &jail.program, GENERIC_READ.0 | GENERIC_EXECUTE.0)?;

    static COUNT: AtomicU64 = AtomicU64::new(0);
    let script = jail.work.join(format!(
        ".command-{}-{}.sh",
        std::process::id(),
        COUNT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&script, command.as_bytes())?;

    let (out_reader, out_writer) = std::io::pipe()?;
    let (in_reader, in_writer) = std::io::pipe()?;
    // std's pipe ends are not inheritable, and a handle the child is
    // handed must be, or process creation rejects the allowlist.
    for handle in [in_reader.as_raw_handle(), out_writer.as_raw_handle()] {
        unsafe {
            SetHandleInformation(
                HANDLE(handle.cast()),
                HANDLE_FLAG_INHERIT.0,
                HANDLE_FLAG_INHERIT,
            )
        }
        .map_err(|err| std::io::Error::other(format!("pipe handle: {err}")))?;
    }
    let environment = environment(jail, folder);
    let spawned = spawn(
        jail,
        &package,
        &script,
        &environment,
        HANDLE(in_reader.as_raw_handle().cast()),
        HANDLE(out_writer.as_raw_handle().cast()),
    );
    // The child holds its own copies; the parent's would keep both pipes
    // open forever.
    drop(in_reader);
    drop(out_writer);
    let (process, _thread, job) = spawned?;

    let (answered, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = answered.send(collect(out_reader, limits.output));
    });
    if let Some(text) = stdin.map(str::to_owned) {
        let mut in_writer = in_writer;
        std::thread::spawn(move || {
            let _ = in_writer.write_all(text.as_bytes());
        });
    } else {
        drop(in_writer);
    }

    let millis = u32::try_from(limits.time.as_millis()).unwrap_or(u32::MAX);
    let timed_out = match unsafe { WaitForSingleObject(process.0, millis) } {
        WAIT_OBJECT_0 => false,
        _ => {
            if let Some(job) = &job {
                let _ = unsafe { TerminateJobObject(job.0, 1) };
            } else {
                let _ = unsafe { TerminateProcess(process.0, 1) };
            }
            let _ = unsafe { WaitForSingleObject(process.0, 5000) };
            true
        }
    };
    let mut code = 0;
    let exit_code = if timed_out {
        None
    } else {
        unsafe { GetExitCodeProcess(process.0, &mut code) }
            .ok()
            .map(|_| code as i32)
    };
    // The collector sees the end once every writer is gone, which a
    // detached process could delay past the timeout, so bound the wait.
    let (bytes, truncated) = received
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_default();
    let _ = std::fs::remove_file(&script);
    Ok(Run {
        output: String::from_utf8_lossy(&bytes).into_owned(),
        truncated,
        exit_code,
        timed_out,
    })
}

/// Creates the AppContainer profile once; the SID derives from the name,
/// so a profile from an earlier run is fine.
fn ensure_profile() -> std::io::Result<()> {
    static ANSWER: OnceLock<Result<(), String>> = OnceLock::new();
    match ANSWER.get_or_init(|| {
        let name = wide(PROFILE);
        let display = wide("Penguin Mail skills");
        let description = wide("Skill scripts run by Penguin Mail");
        match unsafe {
            CreateAppContainerProfile(
                PCWSTR(name.as_ptr()),
                PCWSTR(display.as_ptr()),
                PCWSTR(description.as_ptr()),
                None,
            )
        } {
            Ok(sid) => {
                unsafe { FreeSid(sid) };
                Ok(())
            }
            Err(err) if err.code() == HRESULT::from_win32(ERROR_ALREADY_EXISTS.0) => Ok(()),
            Err(err) => Err(err.to_string()),
        }
    }) {
        Ok(()) => Ok(()),
        Err(reason) => Err(std::io::Error::other(reason.clone())),
    }
}

/// The package SID for this app's profile.
fn package_sid() -> std::io::Result<Sid> {
    let name = wide(PROFILE);
    let sid = unsafe { DeriveAppContainerSidFromAppContainerName(PCWSTR(name.as_ptr())) }
        .map_err(std::io::Error::other)?;
    Ok(Sid(sid))
}

/// A SID text form, such as `S-1-15-3-1` for internetClient.
fn sid_from_name(name: &str) -> std::io::Result<Sid> {
    let name = wide(name);
    let mut sid = PSID::default();
    unsafe { ConvertStringSidToSidW(PCWSTR(name.as_ptr()), &mut sid) }
        .map_err(std::io::Error::other)?;
    Ok(Sid(sid))
}

/// Gives the package SID `access` to `path`, inherited by everything
/// below it. The grant persists: the SID is stable and another run will
/// want it again.
fn grant(package: &Sid, path: &Path, access: u32) -> std::io::Result<()> {
    let name = wide_path(path);
    let mut dacl = std::ptr::null_mut::<ACL>();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let error = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(name.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut dacl),
            None,
            &mut descriptor,
        )
    };
    if error != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(error.0 as i32));
    }
    let _descriptor = Local(descriptor.0);
    let entry = [EXPLICIT_ACCESS_W {
        grfAccessPermissions: access,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: PWSTR(package.0.0.cast()),
        },
    }];
    let mut merged = std::ptr::null_mut::<ACL>();
    let error = unsafe { SetEntriesInAclW(Some(&entry), Some(dacl.cast_const()), &mut merged) };
    if error != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(error.0 as i32));
    }
    let _merged = Local(merged.cast());
    let error = unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(name.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(merged.cast_const()),
            None,
        )
    };
    if error != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(error.0 as i32));
    }
    Ok(())
}

/// The whole environment the command gets: no user profile paths, nothing
/// inherited. HOME and the temp variables point at the work folder, so a
/// command writing dotfiles keeps them inside the sandbox. SKILL and WORK
/// name the two folders the ACLs open, in the `C:/` form busybox uses.
fn environment(jail: &Owned, folder: &Path) -> Vec<u16> {
    let win = |path: &Path| path.to_string_lossy().replace('/', "\\");
    let unix = |path: &Path| path.to_string_lossy().replace('\\', "/");
    let work = unix(&jail.work);
    let mut vars = [
        ("SystemRoot", "C:\\Windows".to_string()),
        ("WINDIR", "C:\\Windows".into()),
        ("OS", "Windows_NT".into()),
        ("ComSpec", "C:\\Windows\\System32\\cmd.exe".into()),
        ("PATHEXT", ".COM;.EXE;.BAT;.CMD".into()),
        ("PATH", format!("{};C:\\Windows\\System32", win(folder))),
        ("HOME", work.clone()),
        ("USERPROFILE", work.clone()),
        ("APPDATA", work.clone()),
        ("LOCALAPPDATA", work.clone()),
        ("TMP", work.clone()),
        ("TEMP", work.clone()),
        ("TMPDIR", work.clone()),
        ("SKILL", unix(&jail.skill)),
        ("WORK", work),
        ("LANG", "C.UTF-8".into()),
    ];
    // The environment block must be sorted case-insensitively or the
    // child's lookups can read the wrong entry.
    vars.sort_by_key(|var| var.0.to_uppercase());
    let mut block = Vec::new();
    for (key, value) in vars {
        block.extend(format!("{key}={value}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    block
}

/// Starts busybox inside the AppContainer, its handles restricted to the
/// pipes it is handed. Returns the process, its thread, and the job that
/// kills the tree; `None` for the job means process-only cleanup.
fn spawn(
    jail: &Owned,
    package: &Sid,
    script: &Path,
    environment: &[u16],
    child_in: HANDLE,
    child_out: HANDLE,
) -> std::io::Result<(KernelHandle, KernelHandle, Option<KernelHandle>)> {
    let mut capabilities = Vec::new();
    let mut sids = Vec::new();
    if jail.network {
        let internet = sid_from_name(INTERNET_CLIENT)?;
        capabilities.push(SID_AND_ATTRIBUTES {
            Sid: internet.0,
            Attributes: 0,
        });
        sids.push(internet);
    }
    let security = SECURITY_CAPABILITIES {
        AppContainerSid: package.0,
        // An empty vector's pointer is dangling, not null, and the call
        // validates it even at a zero count.
        Capabilities: if capabilities.is_empty() {
            std::ptr::null_mut()
        } else {
            capabilities.as_mut_ptr()
        },
        CapabilityCount: capabilities.len() as u32,
        Reserved: 0,
    };

    let mut size = 0;
    let _ = unsafe { InitializeProcThreadAttributeList(None, 2, None, &mut size) };
    let mut buffer = vec![0u8; size];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(buffer.as_mut_ptr().cast());
    unsafe { InitializeProcThreadAttributeList(Some(list), 2, None, &mut size) }
        .map_err(|err| std::io::Error::other(format!("attribute list: {err}")))?;
    let list = AttributeList(list);
    let update = |attribute: u32, value: *const core::ffi::c_void, bytes: usize| unsafe {
        UpdateProcThreadAttribute(
            list.0,
            0,
            attribute as usize,
            Some(value),
            bytes,
            None,
            None,
        )
    };
    let inherit = [child_in, child_out];
    update(
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
        (&raw const security).cast(),
        size_of::<SECURITY_CAPABILITIES>(),
    )
    .and_then(|_| {
        update(
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            inherit.as_ptr().cast(),
            size_of_val(&inherit),
        )
    })
    .map_err(|err| std::io::Error::other(format!("process attributes: {err}")))?;

    let mut cmdline = wide(&format!(
        "\"{}\" sh \"{}\"",
        jail.program.display(),
        script.display()
    ));
    let cwd = wide_path(&jail.work);
    let mut info = PROCESS_INFORMATION::default();
    let startup = STARTUPINFOEXW {
        StartupInfo: STARTUPINFOW {
            cb: size_of::<STARTUPINFOEXW>() as u32,
            dwFlags: STARTF_USESTDHANDLES,
            hStdInput: child_in,
            hStdOutput: child_out,
            hStdError: child_out,
            ..STARTUPINFOW::default()
        },
        lpAttributeList: list.0,
    };
    unsafe {
        CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR::from_raw(cmdline.as_mut_ptr())),
            None,
            None,
            true,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            Some(environment.as_ptr().cast()),
            PCWSTR(cwd.as_ptr()),
            &startup.StartupInfo,
            &mut info,
        )
    }
    .map_err(|err| std::io::Error::other(format!("CreateProcessW: {err}")))?;
    let process = KernelHandle(info.hProcess);
    let job = job_for(process.0).ok();
    Ok((process, KernelHandle(info.hThread), job))
}

/// A job object whose last handle's closing kills every process in it.
fn job_for(process: HANDLE) -> std::io::Result<KernelHandle> {
    let job = KernelHandle(
        unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(std::io::Error::other)?,
    );
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    }
    .map_err(std::io::Error::other)?;
    unsafe { AssignProcessToJobObject(job.0, process) }.map_err(std::io::Error::other)?;
    Ok(job)
}

/// The native busybox build, beside the app binary (where the installer
/// puts it), on PATH, or wherever PENGUIN_MAIL_BUSYBOX points.
pub fn find_busybox() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PENGUIN_MAIL_BUSYBOX")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    {
        return Some(path);
    }
    const NAMES: &[&str] = &["busybox64u.exe", "busybox.exe", "busybox64.exe"];
    // Tests run from target/**/deps, one folder below the binary's own, so
    // a busybox next to the app is found from there too.
    if let Ok(exe) = std::env::current_exe() {
        for folder in [exe.parent(), exe.parent().and_then(Path::parent)]
            .into_iter()
            .flatten()
        {
            if let Some(path) = NAMES
                .iter()
                .map(|name| folder.join(name))
                .find(|path| path.is_file())
            {
                return Some(path);
            }
        }
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .flat_map(|folder| NAMES.iter().map(move |name| folder.join(name)))
        .find(|path| path.is_file())
}

pub(super) fn probe() -> Result<PathBuf, String> {
    use mailrs_domain::translate::{fill, gettext};
    let Some(busybox) = find_busybox() else {
        return Err(gettext(
            "Skill scripts need busybox for Windows, which is not installed. Install it next to the app or set the PENGUIN_MAIL_BUSYBOX variable to it.",
        ));
    };
    // temp_dir() can hand back a Unix path under an MSYS environment,
    // which CreateProcessW rejects outright.
    let work =
        gtk::glib::user_cache_dir().join(format!("penguin-mail-sandbox-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    if let Err(err) = std::fs::create_dir_all(&work) {
        return Err(fill(
            &gettext("Skill scripts cannot run: the sandbox failed to start ({reason})."),
            &[("reason", &err.to_string())],
        ));
    }
    let jail = Owned {
        program: busybox.clone(),
        skill: work.clone(),
        work: work.clone(),
        network: false,
    };
    let answer = run_sync(&jail, "echo penguin-mail-sandbox-ok", None, Limits::DEFAULT);
    let _ = std::fs::remove_dir_all(&work);
    match answer {
        Ok(run) if run.output.contains("penguin-mail-sandbox-ok") => Ok(busybox),
        Ok(run) => Err(fill(
            &gettext("Skill scripts cannot run: the sandbox failed to start ({reason})."),
            &[("reason", run.output.trim())],
        )),
        Err(err) => Err(fill(
            &gettext("Skill scripts cannot run: the sandbox failed to start ({reason})."),
            &[("reason", &err.to_string())],
        )),
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// A kernel handle that closes itself.
struct KernelHandle(HANDLE);

impl Drop for KernelHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// A process-owned SID, freed when it leaves scope.
struct Sid(PSID);

impl Drop for Sid {
    fn drop(&mut self) {
        unsafe { FreeSid(self.0) };
    }
}

/// A LocalFree'd allocation.
struct Local(*mut core::ffi::c_void);

impl Drop for Local {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(Some(HLOCAL(self.0))) };
        }
    }
}

/// The attribute list, deleted when the spawn attempt is over.
struct AttributeList(LPPROC_THREAD_ATTRIBUTE_LIST);

impl Drop for AttributeList {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.0) };
    }
}
