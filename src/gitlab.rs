//! GitLab metadata and authenticated API access through `glab`.

use std::{
    collections::BTreeMap,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use url::Url;

use crate::{
    comments::DiffSide,
    conductor::{Conductor, ConductorStartup},
    editor::remap_range,
    gitlab_comments::{Discussion, DiscussionReply, matching_change},
    gitlab_review::Publication,
    vcs::{Change, Comparison, Repository, validate_commit_id},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub host: String,
    pub path: String,
}

impl Project {
    pub fn from_remote(remote: &str) -> Result<Self> {
        let address = if remote.contains("://") {
            remote.to_owned()
        } else {
            let (host, path) = remote
                .split_once(':')
                .context("Expected an SSH or HTTPS GitLab remote")?;
            format!("ssh://{host}/{path}")
        };
        let url = Url::parse(&address).context("parsing GitLab remote")?;
        ensure!(
            matches!(url.scheme(), "ssh" | "https" | "http"),
            "Expected an SSH or HTTPS GitLab remote"
        );
        let mut host = url
            .host_str()
            .context("GitLab remote has no hostname")?
            .to_owned();
        if url.scheme() != "ssh"
            && let Some(port) = url.port()
        {
            host.push_str(&format!(":{port}"));
        }
        let path = percent_decode_str(url.path())
            .decode_utf8()?
            .trim_matches('/')
            .trim_end_matches(".git")
            .to_owned();
        ensure!(
            path.contains('/')
                && path.split('/').all(|part| !part.is_empty()
                    && !matches!(part, "." | "..")
                    && !part.chars().any(char::is_control)),
            "GitLab remote must contain a namespace and project"
        );
        Ok(Self { host, path })
    }

    fn endpoint(&self) -> String {
        format!("projects/{}", encode(&self.path))
    }
}

#[derive(Debug)]
pub struct MrReference {
    pub project: Option<Project>,
    pub iid: u64,
}

impl MrReference {
    pub fn parse(value: &str) -> Result<Self> {
        if let Ok(iid) = value.trim_start_matches('!').parse::<u64>() {
            ensure!(iid > 0, "MR number must be positive");
            return Ok(Self { project: None, iid });
        }
        let mut url = Url::parse(value).context("Use an MR number or a GitLab MR URL")?;
        ensure!(
            matches!(url.scheme(), "https" | "http"),
            "MR URL must use HTTPS or HTTP"
        );
        let (project, rest) = url
            .path()
            .split_once("/-/merge_requests/")
            .context("Expected a GitLab MR URL")?;
        let iid = rest
            .split('/')
            .next()
            .context("MR URL has no number")?
            .parse::<u64>()
            .context("Invalid MR number in URL")?;
        ensure!(iid > 0, "MR number must be positive");
        let path = project.to_owned();
        url.set_path(&path);
        Ok(Self {
            project: Some(Project::from_remote(url.as_str())?),
            iid,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffRefs {
    pub base_sha: String,
    pub head_sha: String,
    pub start_sha: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct User {
    pub id: u64,
    pub username: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeRequest {
    pub iid: u64,
    pub title: String,
    pub state: String,
    pub web_url: String,
    pub source_branch: String,
    pub target_branch: String,
    pub source_project_id: Option<u64>,
    pub target_project_id: u64,
    pub author: User,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub diff_refs: Option<DiffRefs>,
    #[serde(default)]
    pub sha: Option<String>,
}

impl MergeRequest {
    pub(crate) fn endpoint(&self) -> String {
        format!(
            "projects/{}/merge_requests/{}",
            self.target_project_id, self.iid
        )
    }

    pub(crate) fn same_version(&self, other: &Self) -> Result<bool> {
        Ok(self.iid == other.iid
            && self.target_project_id == other.target_project_id
            && self.source_project_id == other.source_project_id
            && self.target_branch == other.target_branch
            && self.source_branch == other.source_branch
            && self.refs()? == other.refs()?)
    }

    pub fn label(&self) -> String {
        format!(
            "!{} {}{} · @{} · {} → {}",
            self.iid,
            if self.draft { "[Draft] " } else { "" },
            self.title,
            self.author.username,
            self.source_branch,
            self.target_branch
        )
    }

    pub fn refs(&self) -> Result<&DiffRefs> {
        let refs = self
            .diff_refs
            .as_ref()
            .context("GitLab is still preparing the MR diff; retry shortly")?;
        for id in [&refs.base_sha, &refs.head_sha, &refs.start_sha] {
            validate_commit_id(id)?;
        }
        ensure!(
            self.sha.as_ref().is_none_or(|sha| sha == &refs.head_sha),
            "GitLab is still preparing the latest MR diff; retry shortly"
        );
        Ok(refs)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GitLabReview {
    pub project: Project,
    pub remote: String,
    pub merge_request: MergeRequest,
    #[serde(default)]
    pub conductor_managed: bool,
    #[serde(default)]
    pub publication: Publication,
    #[serde(default)]
    pub discussions: Vec<Discussion>,
    #[serde(default)]
    pub replies: Vec<DiscussionReply>,
}

impl GitLabReview {
    pub fn label(&self) -> String {
        format!(
            "{}!{} {}",
            self.project.path, self.merge_request.iid, self.merge_request.title
        )
    }
}

pub struct PreparedReview {
    pub gitlab: GitLabReview,
    pub comparison: Comparison,
    pub changes: Vec<Change>,
    pub conductor_startup: Option<ConductorStartup>,
}

pub enum Checkout {
    Direct,
    Conductor(Conductor),
    PreserveWorkspace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReviewStatus {
    Unreviewed,
    Approved,
    ChangesRequested,
}

pub struct OpenMergeRequest {
    pub request: MergeRequest,
    pub status: ReviewStatus,
    pub approved_by_me: bool,
}

#[derive(Deserialize)]
pub(crate) struct ApprovalState {
    user_has_approved: Option<bool>,
    pub(crate) user_can_approve: Option<bool>,
    #[serde(default)]
    approved_by: Vec<Approver>,
}

impl ApprovalState {
    pub(crate) fn approved(&self, user_id: u64) -> bool {
        self.user_has_approved.unwrap_or_else(|| {
            self.approved_by
                .iter()
                .any(|entry| entry.user.id == user_id)
        })
    }
}

#[derive(Deserialize)]
struct Approver {
    user: User,
}

#[derive(Deserialize)]
pub(crate) struct Reviewer {
    pub(crate) user: User,
    pub(crate) state: String,
}

#[derive(Deserialize)]
struct ProjectInfo {
    id: u64,
    path_with_namespace: String,
    ssh_url_to_repo: String,
    http_url_to_repo: String,
}

pub struct GitLab {
    repo: Repository,
    project: Project,
}

impl GitLab {
    pub fn require_cli(directory: &Path) -> Result<()> {
        crate::command::require("glab", directory, None)
    }

    pub fn new(repo: &Repository, project: &Project) -> Self {
        Self {
            repo: repo.clone(),
            project: project.clone(),
        }
    }

    pub fn login(hostname: Option<&str>, web: bool, device: bool) -> Result<()> {
        let mut command = Command::new("glab");
        command.args(["auth", "login"]);
        if let Some(hostname) = hostname {
            command.args(["--hostname", hostname]);
        }
        if web {
            command.arg("--web");
        }
        if device {
            command.arg("--device");
        }
        let status = crate::command::launch(&mut command, None, Command::status)?;
        ensure!(status.success(), "GitLab authentication failed");
        Ok(())
    }

    pub(crate) fn api(
        &self,
        method: &str,
        endpoint: &str,
        body: Option<&serde_json::Value>,
        paginate: bool,
    ) -> Result<Vec<u8>> {
        self.api_with_timeout(method, endpoint, body, paginate, Duration::from_secs(90))
    }

    fn api_with_timeout(
        &self,
        method: &str,
        endpoint: &str,
        body: Option<&serde_json::Value>,
        paginate: bool,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let mut command = Command::new("glab");
        command
            .current_dir(&self.repo.root)
            .env_remove("GLAB_DEBUG_HTTP")
            .args([
                "api",
                "--hostname",
                &self.project.host,
                "--method",
                method,
                endpoint,
            ]);
        if paginate {
            command.arg("--paginate");
        }
        let input = body.map(serde_json::to_vec).transpose()?;
        if input.is_some() {
            command.args(["--input", "-"]);
        }
        let output =
            crate::command::output_with_timeout(&mut command, input.as_deref(), timeout, || false)
                .context("GitLab API request")?;
        ensure!(
            output.status.success(),
            "GitLab API on {}: {}. Authenticate with review gitlab login --hostname {}",
            self.project.host,
            String::from_utf8_lossy(&output.stderr).trim(),
            self.project.host
        );
        Ok(output.stdout)
    }

    pub(crate) fn get<T: DeserializeOwned>(&self, endpoint: &str) -> Result<T> {
        serde_json::from_slice(&self.api("GET", endpoint, None, false)?)
            .context("decoding GitLab response")
    }

    pub(crate) fn list<T: DeserializeOwned>(&self, endpoint: &str) -> Result<Vec<T>> {
        // Older glab releases emit one JSON array per page; newer releases combine them.
        let output = self.api("GET", endpoint, None, true)?;
        let mut items = Vec::new();
        for page in serde_json::Deserializer::from_slice(&output).into_iter::<Vec<T>>() {
            items.extend(page.context("decoding GitLab result page")?);
        }
        Ok(items)
    }

    pub fn open_merge_requests(&self) -> Result<Vec<OpenMergeRequest>> {
        let mut requests = self
            .list::<MergeRequest>(&format!(
                "{}/merge_requests?state=opened&scope=all&order_by=updated_at&sort=desc&per_page=100",
                self.project.endpoint()
            ))?
            .into_iter();
        let mut open = Vec::with_capacity(requests.len());
        // Bound concurrent glab processes while retaining the server's update order.
        while requests.len() > 0 {
            thread::scope(|scope| -> Result<()> {
                let workers: Vec<_> = requests
                    .by_ref()
                    .take(8)
                    .map(|mr| {
                        scope.spawn(move || {
                            let iid = mr.iid;
                            self.open_merge_request(mr)
                                .with_context(|| format!("Loading review status for MR !{iid}"))
                        })
                    })
                    .collect();
                for worker in workers {
                    open.push(worker.join().expect("GitLab status worker")?);
                }
                Ok(())
            })?;
        }
        Ok(open)
    }

    fn open_merge_request(&self, request: MergeRequest) -> Result<OpenMergeRequest> {
        let endpoint = request.endpoint();
        let reviewers = self.reviewers(&endpoint)?;
        let approvals = self.approval_state(&endpoint)?;
        let approved_by_me = approvals.user_has_approved == Some(true);
        let status = if reviewers
            .iter()
            .any(|reviewer| reviewer.state == "requested_changes")
        {
            ReviewStatus::ChangesRequested
        } else if approved_by_me || !approvals.approved_by.is_empty() {
            // The aggregate `approved` field can be true when no approvals are required.
            ReviewStatus::Approved
        } else {
            ReviewStatus::Unreviewed
        };
        Ok(OpenMergeRequest {
            request,
            status,
            approved_by_me,
        })
    }

    pub(crate) fn approval_state(&self, endpoint: &str) -> Result<ApprovalState> {
        self.get(&format!("{endpoint}/approvals"))
            .context("Checking GitLab approval status")
    }

    pub(crate) fn reviewers(&self, endpoint: &str) -> Result<Vec<Reviewer>> {
        self.list(&format!("{endpoint}/reviewers?per_page=100"))
            .context("Checking GitLab reviewer status")
    }

    pub fn merge_request(&self, iid: u64) -> Result<MergeRequest> {
        for attempt in 0..6 {
            let mr: MergeRequest =
                self.get(&format!("{}/merge_requests/{iid}", self.project.endpoint()))?;
            ensure!(mr.state == "opened", "MR !{iid} is {}", mr.state);
            if mr.refs().is_ok() {
                return Ok(mr);
            }
            if attempt < 5 {
                thread::sleep(Duration::from_millis(300));
            }
        }
        bail!("GitLab is still preparing MR !{iid}; retry shortly")
    }

    pub(crate) fn load_discussions(
        &self,
        context: &mut GitLabReview,
        comparison: &Comparison,
        changes: &[Change],
    ) -> Result<()> {
        let mut discussions: Vec<Discussion> = self.list(&format!(
            "{}/discussions?per_page=100",
            context.merge_request.endpoint()
        ))?;
        let previous: BTreeMap<_, _> = context
            .discussions
            .iter()
            .map(|discussion| (discussion.id.as_str(), discussion))
            .collect();
        // Source verification is best-effort; bound the extra API work across all threads.
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut remaining_bytes = 16 * 1024 * 1024usize;
        let mut sources = BTreeMap::new();
        for discussion in &mut discussions {
            discussion.retain_position(previous.get(discussion.id.as_str()).copied(), comparison);
            if Instant::now() >= deadline {
                continue;
            }
            let Some((commit, mut target)) = discussion.unmapped_position(comparison) else {
                continue;
            };
            let Some(change) = matching_change(changes, &target) else {
                continue;
            };
            let old = target.side == DiffSide::Old;
            let project = if old {
                context.merge_request.target_project_id
            } else {
                context
                    .merge_request
                    .source_project_id
                    .unwrap_or(context.merge_request.target_project_id)
            };
            let before = sources
                .entry((project, commit.to_owned(), target.path.clone()))
                .or_insert_with(|| {
                    if remaining_bytes == 0 {
                        return None;
                    }
                    let content = self
                        .discussion_source(project, commit, &target.path, deadline)
                        .ok()?;
                    remaining_bytes = remaining_bytes.checked_sub(content.len())?;
                    Some(content)
                });
            let Some(before) = before else {
                continue;
            };
            let Ok(after) = self.repo.content(comparison, change, old) else {
                continue;
            };
            if std::str::from_utf8(before).is_ok()
                && std::str::from_utf8(&after).is_ok()
                && let Ok(Some(lines)) =
                    remap_range(change, before, &after, target.start_line..=target.end_line)
            {
                target.start_line = *lines.start();
                target.end_line = *lines.end();
                discussion.pin(comparison, target);
            }
        }
        context.discussions = discussions;
        Ok(())
    }

    fn discussion_source(
        &self,
        project: u64,
        commit: &str,
        path: &str,
        deadline: Instant,
    ) -> Result<Vec<u8>> {
        validate_commit_id(commit)?;
        if let Ok(content) = self.repo.content_at(commit, path) {
            return Ok(content);
        }
        let path = encode(path);
        let timeout = deadline.saturating_duration_since(Instant::now());
        ensure!(!timeout.is_zero(), "GitLab comment source lookup timed out");
        self.api_with_timeout(
            "GET",
            &format!("projects/{project}/repository/files/{path}/raw?ref={commit}"),
            None,
            false,
            timeout,
        )
    }

    pub fn prepare(
        &self,
        repo: &Repository,
        remote: &str,
        iid: u64,
        checkout: Checkout,
    ) -> Result<PreparedReview> {
        if !matches!(checkout, Checkout::PreserveWorkspace) {
            repo.ensure_clean()?;
        }
        let remotes = repo.remotes()?;
        let remote_url = remotes
            .iter()
            .find(|(name, _)| name == remote)
            .map(|(_, url)| url)
            .context("MR's repository remote is missing")?;
        let mut project = Project::from_remote(remote_url)?;
        project.host.clone_from(&self.project.host);
        ensure!(
            project == self.project,
            "Selected remote does not match the MR project"
        );
        let mr = self.merge_request(iid)?;
        let refs = mr.refs()?.clone();
        let target: ProjectInfo = self.get(&self.project.endpoint())?;
        ensure!(
            target.id == mr.target_project_id,
            "MR target differs from the selected repository"
        );
        let source_id = mr
            .source_project_id
            .context("MR's source project has been deleted")?;
        let source_remote = if source_id == target.id {
            repo.fetch(remote, &[&mr.target_branch, &mr.source_branch])?;
            remote.to_owned()
        } else {
            let source: ProjectInfo = self.get(&format!("projects/{source_id}"))?;
            let project = Project {
                host: self.project.host.clone(),
                path: source.path_with_namespace,
            };
            let source_remote = if let Some((name, _)) = remotes.iter().find(|(_, url)| {
                Project::from_remote(url).is_ok_and(|candidate| candidate == project)
            }) {
                name.clone()
            } else {
                let name = format!("review-gitlab-{source_id}");
                ensure!(
                    remotes.iter().all(|(existing, _)| existing != &name),
                    "Remote {name:?} already points elsewhere; rename it before reviewing this fork"
                );
                let url = if remote_url.starts_with("http") {
                    &source.http_url_to_repo
                } else {
                    &source.ssh_url_to_repo
                };
                ensure!(
                    Project::from_remote(url)? == project,
                    "GitLab returned an unexpected source repository URL"
                );
                repo.add_remote(&name, url)?;
                name
            };
            repo.fetch(remote, &[&mr.target_branch])?;
            repo.fetch(&source_remote, &[&mr.source_branch])?;
            source_remote
        };
        ensure!(
            repo.remote_head(&source_remote, &mr.source_branch)? == refs.head_sha,
            "Fetched branch differs from the MR head; the MR may be updating. Retry shortly"
        );
        let current = self.merge_request(iid)?;
        ensure!(
            current.same_version(&mr)?,
            "MR changed during fetch; retry to review its latest version"
        );
        repo.resolve(&refs.start_sha, false)
            .context("MR target snapshot is unavailable; fetch complete repository history")?;
        let comparison = repo
            .snapshot_commits(&refs.base_sha, &refs.head_sha)
            .context(
                "opening MR snapshots; fetch complete repository history if objects are missing",
            )?;
        let changes = repo.changes(&comparison)?;
        let mut gitlab = GitLabReview {
            project: self.project.clone(),
            remote: remote.to_owned(),
            merge_request: current,
            conductor_managed: !matches!(checkout, Checkout::Direct),
            publication: Publication::default(),
            discussions: Vec::new(),
            replies: Vec::new(),
        };
        self.load_discussions(&mut gitlab, &comparison, &changes)?;
        let conductor_startup = match checkout {
            Checkout::Direct => {
                repo.checkout_review(
                    &mr.source_branch,
                    &refs.head_sha,
                    &source_remote,
                    &mr.source_branch,
                )?;
                None
            }
            Checkout::Conductor(conductor) => {
                Some(conductor.start(repo, &mr.source_branch, &source_remote, refs.head_sha)?)
            }
            Checkout::PreserveWorkspace => None,
        };
        Ok(PreparedReview {
            gitlab,
            comparison,
            changes,
            conductor_startup,
        })
    }
}

fn encode(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}
