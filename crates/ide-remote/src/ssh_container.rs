//! Docker and the Dev Containers CLI on the far side of an authenticated SSH session.

use crate::devcontainer::{Up, parse_up_with_configuration};
use ide_core::{CancellationToken, IdeError, IdeResult};
use ide_docker::{DockerContainer, parse_containers};
use ide_ssh::{Output, Session, shell::quote};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use specta::Type;
use std::{
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite};

const QUICK: Duration = Duration::from_secs(25);
const INSTALL: Duration = Duration::from_secs(180);
const UP_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const CLI_REQUIRED: &str = "Install the Dev Containers CLI on the SSH host (Settings > Tools).";
const MARKER: &str = "__OPENIDEA_DOCKER__";
const SOCKET: &str = r#"if [ -n "${DOCKER_CONTEXT:-}" ]; then
    docker context inspect --format '{{(index .Endpoints "docker").Host}}' -- "$DOCKER_CONTEXT"
elif [ -n "${DOCKER_HOST:-}" ]; then
    printf '%s\n' "$DOCKER_HOST"
else
    # Older Docker can return its top-level help for unknown commands.
    help=$(docker --help) || exit "$?"
    if printf '%s\n' "$help" | grep -Eq '^[[:blank:]]+context[[:blank:]]'; then
        docker context inspect --format '{{(index .Endpoints "docker").Host}}'
    else
        printf '%s\n' 'unix:///var/run/docker.sock'
    fi
fi"#;

#[derive(Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SshContainers {
    pub daemon: String,
    pub containers: Vec<DockerContainer>,
}

#[derive(Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ContainerFolders {
    pub directory: String,
    pub folders: Vec<String>,
    pub user: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerInfo {
    name: String,
    user: String,
    working_dir: String,
    running: bool,
    paused: bool,
    restarting: bool,
    mounts: Option<Vec<Mount>>,
}

impl ContainerInfo {
    pub fn name(&self) -> &str {
        self.name.trim_start_matches('/')
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Mount {
    #[serde(rename = "Type")]
    kind: String,
    source: String,
    destination: String,
}

/// Keeps the validated selection while browsing, without keeping its SSH session alive.
#[derive(Default)]
pub struct ContainerBrowser {
    selection: Mutex<Option<ContainerSelection>>,
}

#[derive(Clone)]
struct ContainerSelection {
    session: Weak<Session>,
    daemon: String,
    container: String,
    socket: String,
    user: String,
}

impl ContainerBrowser {
    pub async fn folders(
        &self,
        session: Arc<Session>,
        daemon: &str,
        id: &str,
        path: Option<&str>,
        host_root: Option<&str>,
    ) -> IdeResult<ContainerFolders> {
        if let Some(path) = path {
            let selection = self
                .selection
                .lock()
                .as_ref()
                .filter(|selection| {
                    selection.session.as_ptr() == Arc::as_ptr(&session)
                        && selection.daemon == daemon
                        && selection.container == id
                })
                .cloned();
            if let Some(selection) = selection {
                let docker = DockerOverSsh { session, socket: selection.socket };
                return docker.directory(id, path, &selection.user, true).await;
            }
        }

        let (docker, daemon) = DockerOverSsh::connect(session, Some(daemon)).await?;
        let info = docker.inspect(id).await?;
        let folders = match path {
            Some(path) => docker.directory(id, path, &info.user, true).await?,
            None => docker.initial_folders(id, &info, host_root).await?,
        };
        *self.selection.lock() = Some(ContainerSelection {
            session: Arc::downgrade(&docker.session),
            daemon,
            container: id.to_owned(),
            socket: docker.socket,
            user: info.user,
        });
        Ok(folders)
    }
}

/// Pins the effective Docker socket for every command, including port forwards.
#[derive(Clone)]
pub struct DockerOverSsh {
    session: Arc<Session>,
    socket: String,
}

impl DockerOverSsh {
    pub async fn connect(session: Arc<Session>, expected_daemon: Option<&str>) -> IdeResult<(Self, String)> {
        let command = format!(
            "printf '%s\\n' {MARKER}; command -v docker >/dev/null 2>&1 || {{ echo 'Docker CLI is not installed on the SSH host' >&2; exit 127; }}; {SOCKET}"
        );
        let socket = output(&session, &command, "checking Docker on the SSH host").await?;
        let socket = marked(&socket)?.trim();
        if !socket.strip_prefix("unix://").is_some_and(absolute) {
            return Err(IdeError::unavailable("The SSH host's Docker context must use a Unix socket"));
        }
        let docker = Self { session, socket: socket.to_owned() };
        let info = docker.capture(&["info", "--format", "{{.OSType}} {{.ID}}"], "connecting to Docker").await?;
        let (os, daemon) = info.trim().split_once(' ').unwrap_or_default();
        if os != "linux" {
            return Err(IdeError::unavailable("The SSH host must use a Linux Docker engine"));
        }
        if daemon.is_empty() || daemon.len() > 128 || daemon.chars().any(char::is_whitespace) {
            return Err(IdeError::unavailable("Docker did not report a daemon ID"));
        }
        if expected_daemon.is_some_and(|expected| expected != daemon) {
            return Err(IdeError::unavailable("The SSH host's Docker daemon changed. Select the container again."));
        }
        Ok((docker, daemon.to_owned()))
    }

    pub async fn containers(&self) -> IdeResult<Vec<DockerContainer>> {
        let stdout = self
            .capture(
                &["ps", "--no-trunc", "--filter", "status=running", "--format", "{{json .}}"],
                "listing containers",
            )
            .await?;
        Ok(parse_containers(&stdout).into_iter().filter(|c| c.state == "running" && valid_id(&c.id)).collect())
    }

    pub async fn inspect(&self, id: &str) -> IdeResult<ContainerInfo> {
        if !valid_id(id) {
            return Err(IdeError::invalid("Expected a full Docker container ID"));
        }
        // Project metadata only: full inspect output includes environment secrets.
        let format = r#"{"Name":{{json .Name}},"User":{{json .Config.User}},"WorkingDir":{{json .Config.WorkingDir}},"Running":{{json .State.Running}},"Paused":{{json .State.Paused}},"Restarting":{{json .State.Restarting}},"Mounts":{{json .Mounts}}}"#;
        let stdout = self.capture(&["container", "inspect", "--format", format, id], "checking the container").await?;
        let info: ContainerInfo = serde_json::from_str(&stdout)
            .map_err(|e| IdeError::unavailable(format!("Invalid Docker metadata: {e}")))?;
        if !info.running || info.paused || info.restarting {
            return Err(IdeError::unavailable("The selected container is no longer running"));
        }
        Ok(info)
    }

    async fn initial_folders(
        &self,
        id: &str,
        info: &ContainerInfo,
        host_root: Option<&str>,
    ) -> IdeResult<ContainerFolders> {
        if let Some(mapped) = host_root.and_then(|root| mounted_path(root, info.mounts.as_deref().unwrap_or_default()))
            && let Ok(folders) = self.directory(id, &mapped, &info.user, true).await
        {
            return Ok(folders);
        }
        if absolute(&info.working_dir)
            && let Ok(folders) = self.directory(id, &info.working_dir, &info.user, true).await
        {
            return Ok(folders);
        }
        match self.directory(id, "~", &info.user, true).await {
            Ok(folders) => Ok(folders),
            Err(_) => self.directory(id, "/", &info.user, true).await,
        }
    }

    pub async fn resolve_directory(&self, id: &str, path: &str) -> IdeResult<String> {
        Ok(self.directory(id, path, "", false).await?.directory)
    }

    async fn directory(&self, id: &str, path: &str, user: &str, list: bool) -> IdeResult<ContainerFolders> {
        let path = if path.is_empty() { "~" } else { path };
        if !(absolute(path) || path == "~" || path.strip_prefix('~').is_some_and(absolute)) {
            return Err(IdeError::invalid("Enter an absolute container path or a path under ~"));
        }
        let script = r#"case "$1" in
    '~') set -- "${HOME:?}" "$2";;
    '~/'*) set -- "${HOME:?}${1#~}" "$2";;
esac
cd "$1" && [ -r . ] && [ -x . ] || { echo "Cannot read directory: $1" >&2; exit 1; }
printf '%s\000' "$(pwd -P)"
if [ "$2" = list ]; then
    for f in ./* ./.[!.]* ./..?*; do
        if [ -d "$f" ] && [ -x "$f" ]; then printf '%s\000' "${f#./}"; fi
    done
fi"#;
        let mode = if list { "list" } else { "resolve" };
        let result = self
            .capture(&["exec", id, "sh", "-c", script, "sh", path, mode], "reading the container directory")
            .await?;
        let mut fields = result.split('\0');
        let directory = fields
            .next()
            .filter(|dir| absolute(dir))
            .ok_or_else(|| IdeError::unavailable("Could not resolve the container directory"))?;
        let mut folders: Vec<String> =
            fields.filter(|name| !name.is_empty() && !name.chars().any(char::is_control)).map(str::to_owned).collect();
        folders.sort_unstable();
        Ok(ContainerFolders { directory: directory.to_owned(), folders, user: user.to_owned() })
    }

    pub async fn capture(&self, args: &[&str], operation: &str) -> IdeResult<String> {
        let command = format!("printf '%s\\n' {MARKER}; {}", self.command(args));
        let result = output(&self.session, &command, operation).await?;
        Ok(marked(&result)?.to_owned())
    }

    pub async fn capture_in(&self, id: &str, user: Option<&str>, args: &[&str], operation: &str) -> IdeResult<String> {
        self.capture(&exec_args(id, user, false, args), operation).await
    }

    pub async fn stream_in(
        &self,
        id: &str,
        user: Option<&str>,
        args: &[&str],
    ) -> IdeResult<impl AsyncRead + AsyncWrite + Unpin + use<>> {
        self.stream(&exec_args(id, user, true, args)).await
    }

    pub async fn stream(&self, args: &[&str]) -> IdeResult<impl AsyncRead + AsyncWrite + Unpin + use<>> {
        self.open_stream(&self.command(args)).await
    }

    async fn open_stream(&self, command: &str) -> IdeResult<impl AsyncRead + AsyncWrite + Unpin + use<>> {
        tokio::time::timeout(QUICK, self.session.exec_stream(command))
            .await
            .map_err(|_| IdeError::unavailable("Timed out starting Docker over SSH"))?
            .map_err(|e| IdeError::unavailable(format!("Could not start Docker over SSH: {e}")))
    }

    /// Checks the length before publishing the binary, including after an SSH channel closes early.
    pub async fn install(
        &self,
        id: &str,
        user: Option<&str>,
        bin: &str,
        data: &[u8],
        cancel: &CancellationToken,
        progress: &(dyn Fn(f64) + Send + Sync),
    ) -> IdeResult<()> {
        if cancel.is_cancelled() {
            return Err(IdeError::cancelled());
        }
        let script = r#"set -e
mkdir -p "${1%/*}"
p="$1.part.$$"
trap 'rm -f "$p"' EXIT
umask 077
cat > "$p"
[ "$(wc -c < "$p")" -eq "$2" ]
chmod 700 "$p"
mv -f "$p" "$1""#;
        let len = data.len().to_string();
        let args = exec_args(id, user, true, &["sh", "-c", script, "sh", bin, &len]);
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let sink = stderr.clone();
        let process = tokio::time::timeout(
            QUICK,
            self.session.spawn(
                &self.command(&args),
                None,
                Box::new(move |out| {
                    if let Output::Stderr(bytes) = out {
                        let mut tail = sink.lock();
                        let remaining = 4096usize.saturating_sub(tail.len());
                        tail.extend_from_slice(&bytes[..remaining.min(bytes.len())]);
                    }
                }),
            ),
        )
        .await
        .map_err(|_| IdeError::unavailable("Timed out starting the container installation"))??;
        let upload = async {
            let mut sent = 0;
            for chunk in data.chunks(128 * 1024) {
                process.write(chunk).await?;
                sent += chunk.len();
                progress(sent as f64 / data.len() as f64);
            }
            process.eof().await
        };
        let finished = async {
            match process.wait().await {
                Some(0) => Ok(()),
                code => Err(IdeError::unavailable(format!(
                    "Installing openidea-server in the container failed ({code:?}): {}",
                    String::from_utf8_lossy(&stderr.lock()).trim()
                ))),
            }
        };
        let result = tokio::select! {
            _ = cancel.cancelled() => Err(IdeError::cancelled()),
            result = tokio::time::timeout(INSTALL, async { tokio::try_join!(upload, finished).map(|_| ()) }) =>
                result.unwrap_or_else(|_| Err(IdeError::unavailable("Timed out installing openidea-server in the container"))),
        };
        if result.is_err() {
            let _ = tokio::time::timeout(QUICK, process.kill()).await;
        }
        result
    }

    fn command(&self, args: &[&str]) -> String {
        let args = args.iter().map(|arg| quote(arg)).collect::<Vec<_>>().join(" ");
        format!(
            "unset DOCKER_CONTEXT DOCKER_HOST DOCKER_TLS_VERIFY DOCKER_TLS; exec docker --host {} {args}",
            quote(&self.socket)
        )
    }

    /// Runs the CLI on the SSH host so workspace bind mounts use that host's filesystem.
    pub async fn devcontainer_up(
        &self,
        root: &str,
        config: &str,
        cancel: &CancellationToken,
    ) -> IdeResult<(Up, Option<Value>)> {
        if cancel.is_cancelled() {
            return Err(IdeError::cancelled());
        }
        let command = self.devcontainer_command(root, config, "up", &["--include-merged-configuration"]);
        let output = Arc::new(Mutex::new((Vec::new(), Vec::new())));
        let sink = output.clone();
        let process = tokio::time::timeout(
            QUICK,
            self.session.spawn(
                &command,
                None,
                Box::new(move |event| {
                    let mut sink = sink.lock();
                    match event {
                        Output::Stdout(bytes) => append_tail(&mut sink.0, bytes, 1024 * 1024),
                        Output::Stderr(bytes) => append_tail(&mut sink.1, bytes, 8192),
                    }
                }),
            ),
        )
        .await
        .map_err(|_| IdeError::unavailable("Timed out starting the Dev Containers CLI on the SSH host"))??;
        let status = tokio::select! {
            _ = cancel.cancelled() => Err(IdeError::cancelled()),
            result = tokio::time::timeout(UP_TIMEOUT, process.wait()) =>
                result.map_err(|_| IdeError::unavailable("devcontainer up did not finish in time")),
        };
        let status = match status {
            Ok(code) => code,
            Err(e) => {
                let _ = tokio::time::timeout(QUICK, process.kill()).await;
                return Err(e);
            }
        };
        let (stdout, stderr) = &*output.lock();
        let log = String::from_utf8_lossy(stderr);
        if status == Some(127) && log.contains(CLI_REQUIRED) {
            return Err(IdeError::unavailable(CLI_REQUIRED));
        }
        let result = parse_up_with_configuration(&String::from_utf8_lossy(stdout));
        match (status, result) {
            (Some(0), Ok(up)) => Ok(up),
            (_, result) => {
                let cause = result
                    .err()
                    .map(|e| e.message)
                    .unwrap_or_else(|| format!("devcontainer up exited with {status:?}"));
                let log = log.trim();
                Err(IdeError::unavailable(if log.is_empty() { cause } else { format!("{cause}\n{log}") }))
            }
        }
    }

    /// The CLI applies remoteUser, remoteEnv and the user environment probe before starting the backend.
    pub async fn devcontainer_stream(
        &self,
        root: &str,
        config: &str,
        up: &Up,
        bin: &str,
        environment: &[String],
    ) -> IdeResult<impl AsyncRead + AsyncWrite + Unpin + use<>> {
        let mut args = vec!["--container-id", &up.container];
        for value in environment {
            args.extend(["--remote-env", value]);
        }
        args.extend(["--", bin, "--stdio"]);
        self.open_stream(&self.devcontainer_command(root, config, "exec", &args)).await
    }

    fn devcontainer_command(&self, root: &str, config: &str, subcommand: &str, args: &[&str]) -> String {
        let args = args.iter().map(|arg| quote(arg)).collect::<Vec<_>>().join(" ");
        let root_arg = quote(root);
        let config_arg = quote(&ide_ssh::sftp::join(root, config));
        // Match the server's data directory and the IDE's npm tool installation path.
        format!(
            r#"unset DOCKER_CONTEXT DOCKER_TLS_VERIFY DOCKER_TLS
export DOCKER_HOST={}
export PATH="${{OPENIDEA_SERVER_HOME:-$HOME/.openidea}}/data/tools/node_modules/.bin:$PATH"
command -v devcontainer >/dev/null 2>&1 || {{ echo '{CLI_REQUIRED}' >&2; exit 127; }}
cd {root_arg} && exec devcontainer {subcommand} --workspace-folder {root_arg} --config {config_arg} {args}"#,
            quote(&self.socket)
        )
    }
}

fn exec_args<'a>(id: &'a str, user: Option<&'a str>, interactive: bool, args: &[&'a str]) -> Vec<&'a str> {
    let mut command = vec!["exec"];
    if interactive {
        command.push("-i");
    }
    if let Some(user) = user {
        command.extend(["--user", user]);
    }
    command.push(id);
    command.extend_from_slice(args);
    command
}

fn append_tail(output: &mut Vec<u8>, bytes: &[u8], limit: usize) {
    let bytes = &bytes[bytes.len().saturating_sub(limit)..];
    let excess = (output.len() + bytes.len()).saturating_sub(limit);
    output.drain(..excess);
    output.extend_from_slice(bytes);
}

fn valid_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn absolute(path: &str) -> bool {
    path.starts_with('/') && !path.chars().any(char::is_control)
}

fn mounted_path(root: &str, mounts: &[Mount]) -> Option<String> {
    mounts
        .iter()
        .filter(|mount| mount.kind == "bind" && absolute(&mount.source) && absolute(&mount.destination))
        .filter_map(|mount| {
            let source = mount.source.trim_end_matches('/');
            let rest = root.trim_end_matches('/').strip_prefix(source)?;
            (rest.is_empty() || rest.starts_with('/')).then_some((source.len(), mount, rest))
        })
        .max_by_key(|(len, _, _)| *len)
        .map(|(_, mount, rest)| {
            let path = format!("{}{rest}", mount.destination.trim_end_matches('/'));
            if path.is_empty() { "/".into() } else { path }
        })
}

fn marked(stdout: &str) -> IdeResult<&str> {
    stdout
        .split_once(&format!("{MARKER}\n"))
        .map(|(_, body)| body)
        .ok_or_else(|| IdeError::unavailable("Docker over SSH did not return a result"))
}

async fn output(session: &Session, command: &str, operation: &str) -> IdeResult<String> {
    tokio::time::timeout(QUICK, session.exec(command, None))
        .await
        .map_err(|_| IdeError::unavailable(format!("Timed out {operation} on {}", session.label())))??
        .into_stdout(operation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn folder_navigation_reuses_only_the_validated_selection() {
        use ide_core::remote_settings::SshAuth;
        use ide_ssh::{ConnectError, SshFiles, SshTarget, testing::sshd};

        let tmp = tempfile::tempdir().unwrap();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let recorded = commands.clone();
        let port = sshd(
            tmp.path().to_owned(),
            Arc::new(move |cmd| {
                let (operation, result) = if cmd.contains("command -v docker") {
                    ("socket", "unix:///var/run/docker.sock")
                } else if cmd.contains(" info --format ") {
                    ("info", "linux daemon")
                } else if cmd.contains(" container inspect --format ") {
                    ("inspect", r#"{"Name":"/dev","User":"dev","WorkingDir":"/work","Running":true,"Paused":false,"Restarting":false}"#)
                } else if cmd.ends_with(" sh /work/src list") {
                    ("list", "/work/src\0lib\0")
                } else if cmd.ends_with(" sh /work list") {
                    ("list", "/work\0src\0")
                } else {
                    panic!("unexpected SSH command: {cmd}");
                };
                recorded.lock().push(operation);
                (format!("{MARKER}\n{result}"), 0)
            }),
        )
        .await;
        let files = SshFiles { known_hosts: tmp.path().join("known_hosts"), config: None };
        let target = SshTarget {
            host: "127.0.0.1".into(),
            port: Some(port),
            user: "dev".into(),
            auth: SshAuth::Password,
            key_file: String::new(),
        };
        let session = Arc::new(match Session::connect(&target, Some("secret"), &files).await {
            Err(ConnectError::UnknownHostKey(key)) => {
                key.trust(&files.known_hosts).unwrap();
                Session::connect(&target, Some("secret"), &files).await.ok().unwrap()
            }
            result => result.ok().unwrap(),
        });
        let browser = ContainerBrowser::default();
        let container = "a".repeat(64);

        let first = browser.folders(session.clone(), "daemon", &container, None, None).await.unwrap();
        assert_eq!(first.directory, "/work");
        assert_eq!(first.folders, ["src"]);
        let next = browser.folders(session, "daemon", &container, Some("/work/src"), None).await.unwrap();
        assert_eq!(next.directory, "/work/src");
        assert_eq!(next.folders, ["lib"]);
        assert_eq!(next.user, "dev");
        assert_eq!(commands.lock().as_slice(), ["socket", "info", "inspect", "list", "list"]);

        let reconnected = Arc::new(Session::connect(&target, Some("secret"), &files).await.ok().unwrap());
        browser.folders(reconnected.clone(), "daemon", &container, Some("/work/src"), None).await.unwrap();
        assert_eq!(&commands.lock()[5..], ["socket", "info", "inspect", "list"]);
        assert!(browser.folders(reconnected, "other-daemon", &container, Some("/work"), None).await.is_err());
        assert_eq!(&commands.lock()[9..], ["socket", "info"]);
    }

    #[cfg(unix)]
    #[test]
    fn uses_default_socket_with_a_legacy_docker_cli() {
        let docker = r#"docker() {
    if [ "$1" = --help ] || [ "$2" = --help ]; then
        printf '%s\n' 'Usage: docker [OPTIONS] COMMAND' '  container  Manage containers'
    else
        printf '%s\n' 'unknown flag: --format' >&2
        return 125
    fi
}"#;
        let output = std::process::Command::new("sh")
            .args(["-c", &format!("{docker}\n{SOCKET}")])
            .env_remove("DOCKER_CONTEXT")
            .env_remove("DOCKER_HOST")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(output.stdout, b"unix:///var/run/docker.sock\n");
    }

    #[test]
    fn picks_the_deepest_bind_mount_at_a_path_boundary() {
        let mount = |kind: &str, source: &str, destination: &str| Mount {
            kind: kind.into(),
            source: source.into(),
            destination: destination.into(),
        };
        let mounts = [
            mount("bind", "/home/dev", "/work"),
            mount("volume", "/home/dev/app", "/ignored"),
            mount("bind", "/home/dev/app", "/project space"),
        ];
        assert_eq!(mounted_path("/home/dev/app/backend", &mounts).as_deref(), Some("/project space/backend"));
        assert_eq!(mounted_path("/home/dev/app2", &mounts).as_deref(), Some("/work/app2"));
        assert_eq!(mounted_path("/home/developer/app", &mounts), None);
    }
}
