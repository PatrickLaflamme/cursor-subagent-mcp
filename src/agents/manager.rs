use crate::agents::model::{AgentInfo, AgentOutputBuffer, CreateAgentRequest, CreateAgentResponse};
use crate::errors::AgentError;
use dashmap::DashMap;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
// no direct tokio::time imports needed at module scope
use uuid::Uuid;

#[derive(Clone)]
pub struct AgentManagerImpl {
    cursor_agent_path: Option<String>,
    buffer_bytes: usize,
    agents: Arc<DashMap<String, Arc<AgentHandle>>>,
    metrics: Arc<AgentMetrics>,
}

pub struct AgentHandle {
    pub id: String,
    pub name: Option<String>,
    pub created_at: OffsetDateTime,
    pub child: Mutex<Child>,
    pub buffer: Mutex<AgentOutputBuffer>,
    pub orig_args: Vec<String>,
    pub orig_env: HashMap<String, String>,
    pub orig_working_dir: Option<PathBuf>,
    // Last time this agent produced output or received input
    pub last_used: Mutex<OffsetDateTime>,
}

impl AgentManagerImpl {
    pub fn new(cursor_agent_path: Option<String>, buffer_bytes: usize) -> Self {
        Self {
            cursor_agent_path,
            buffer_bytes,
            agents: Arc::new(DashMap::new()),
            metrics: Arc::new(AgentMetrics::default()),
        }
    }

    fn resolve_binary(&self) -> Result<String, AgentError> {
        if let Some(p) = &self.cursor_agent_path {
            return Ok(p.clone());
        }
        which::which("cursor-agent")
            .map_err(|e| AgentError::Spawn(format!("cursor-agent not found: {e}")))
            .map(|p| p.to_string_lossy().to_string())
    }

    pub async fn create(&self, req: CreateAgentRequest) -> Result<CreateAgentResponse, AgentError> {
        let id = Uuid::new_v4().to_string();
        let bin = self.resolve_binary()?;

        let mut cmd = Command::new(&bin);
        for a in &req.args {
            cmd.arg(a);
        }

        if let Some(dir) = &req.working_dir {
            cmd.current_dir(dir);
        }

        // Allowlist env pass-through: only explicit provided entries
        if !req.env.is_empty() {
            cmd.envs(&req.env);
        }

        let child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| AgentError::Spawn(format!("Failed to spawn cursor-agent: {}. Ensure cursor-agent is installed and on PATH or set CURSOR_AGENT_PATH.", e)))?;

        let pid = child.id().unwrap_or_default();

        let handle = Arc::new(AgentHandle {
            id: id.clone(),
            name: req.name.clone(),
            created_at: OffsetDateTime::now_utc(),
            child: Mutex::new(child),
            buffer: Mutex::new(AgentOutputBuffer::new(self.buffer_bytes)),
            orig_args: req.args.clone(),
            orig_env: req.env.clone(),
            orig_working_dir: req.working_dir.clone(),
            last_used: Mutex::new(OffsetDateTime::now_utc()),
        });

        // Start stdout/stderr pumps
        self.spawn_pumps(handle.clone());

        self.agents.insert(id.clone(), handle);
        self.metrics.created_count.fetch_add(1, Ordering::Relaxed);
        Ok(CreateAgentResponse { agent_id: id, pid })
    }

    pub async fn send_input(&self, agent_id: &str, input: &str) -> Result<(), AgentError> {
        let Some(handle) = self.agents.get(agent_id).map(|e| e.clone()) else {
            return Err(AgentError::NotFound(agent_id.to_string()));
        };
        // Avoid holding the lock across await: temporarily take stdin
        let mut stdin_pipe = {
            let mut child = handle.child.lock();
            child
                .stdin
                .take()
                .ok_or_else(|| AgentError::InvalidState("stdin not available".into()))?
        };
        use tokio::io::AsyncWriteExt;
        stdin_pipe
            .write_all(input.as_bytes())
            .await
            .map_err(|e| AgentError::Io(e.to_string()))?;
        stdin_pipe
            .write_all(b"\n")
            .await
            .map_err(|e| AgentError::Io(e.to_string()))?;
        stdin_pipe
            .flush()
            .await
            .map_err(|e| AgentError::Io(e.to_string()))?;
        // Return stdin to child
        {
            let mut child = handle.child.lock();
            child.stdin.replace(stdin_pipe);
        }
        *handle.last_used.lock() = OffsetDateTime::now_utc();
        self.metrics
            .total_input_bytes
            .fetch_add(input.len() as u64 + 1, Ordering::Relaxed);
        Ok(())
    }

    pub async fn reset(&self, agent_id: &str, hard: bool) -> Result<(), AgentError> {
        let Some(entry) = self.agents.get(agent_id) else {
            return Err(AgentError::NotFound(agent_id.to_string()));
        };
        if !hard {
            entry.buffer.lock().lines.clear();
            entry.buffer.lock().current_bytes = 0;
            return Ok(());
        }
        // Hard reset: kill child and respawn with same config under same ID
        let bin = self.resolve_binary()?;
        // Best-effort terminate without holding the lock across awaits
        {
            let mut child = entry.child.lock();
            let _ = child.start_kill();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
        while std::time::Instant::now() < deadline {
            let exited = {
                let mut child = entry.child.lock();
                child.try_wait().ok().flatten().is_some()
            };
            if exited {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let mut cmd = Command::new(&bin);
        for a in &entry.orig_args {
            cmd.arg(a);
        }
        if let Some(dir) = &entry.orig_working_dir {
            cmd.current_dir(dir);
        }
        if !entry.orig_env.is_empty() {
            cmd.envs(&entry.orig_env);
        }
        let new_child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| AgentError::Spawn(e.to_string()))?;
        // swap child and clear buffer
        {
            let mut child = entry.child.lock();
            *child = new_child;
        }
        entry.buffer.lock().lines.clear();
        entry.buffer.lock().current_bytes = 0;
        // restart pumps
        self.spawn_pumps(entry.clone());
        Ok(())
    }

    pub async fn stop(&self, agent_id: &str, signal: StopSignal) -> Result<(), AgentError> {
        let Some((_, handle)) = self.agents.remove(agent_id) else {
            return Err(AgentError::NotFound(agent_id.to_string()));
        };
        match signal {
            StopSignal::Term | StopSignal::Kill => {
                {
                    let mut child = handle.child.lock();
                    let _ = child.start_kill();
                }
                // Poll until the process exits or timeout, without holding the lock across await
                let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
                while std::time::Instant::now() < deadline {
                    let exited = {
                        let mut child = handle.child.lock();
                        child.try_wait().ok().flatten().is_some()
                    };
                    if exited {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        }
        self.metrics.stopped_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub async fn list(&self) -> Vec<AgentInfo> {
        self.agents
            .iter()
            .map(|e| AgentInfo {
                agent_id: e.id.clone(),
                name: e.name.clone(),
                pid: e.child.lock().id().unwrap_or_default(),
                created_at: e.created_at,
                status: "running".to_string(),
            })
            .collect()
    }

    pub async fn get_buffer(&self, agent_id: &str) -> Result<String, AgentError> {
        let Some(handle) = self.agents.get(agent_id).map(|e| e.clone()) else {
            return Err(AgentError::NotFound(agent_id.to_string()));
        };
        let concatenated = {
            let lock = handle.buffer.lock();
            lock.concat()
        };
        Ok(concatenated)
    }

    fn spawn_pumps(&self, handle: Arc<AgentHandle>) {
        // stdout
        let mut child_for_out = handle.child.lock();
        let stdout = child_for_out.stdout.take();
        let stderr = child_for_out.stderr.take();
        drop(child_for_out);

        if let Some(stdout) = stdout {
            let handle_out = handle.clone();
            let metrics = self.metrics.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let len = line.len();
                    handle_out.buffer.lock().push_line(line);
                    *handle_out.last_used.lock() = OffsetDateTime::now_utc();
                    metrics
                        .total_output_bytes
                        .fetch_add(len as u64 + 1, Ordering::Relaxed);
                }
            });
        }
        if let Some(stderr) = stderr {
            let handle_err = handle.clone();
            let metrics = self.metrics.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    handle_err
                        .buffer
                        .lock()
                        .push_line(format!("[stderr] {line}"));
                    *handle_err.last_used.lock() = OffsetDateTime::now_utc();
                    metrics
                        .total_output_bytes
                        .fetch_add(line.len() as u64 + 1, Ordering::Relaxed);
                }
            });
        }
    }

    pub async fn stop_all(&self) {
        let ids: Vec<String> = self.agents.iter().map(|e| e.id.clone()).collect();
        for id in ids {
            let _ = self.stop(&id, StopSignal::Term).await;
        }
    }

    pub fn metrics_snapshot(&self) -> AgentMetricsSnapshot {
        AgentMetricsSnapshot {
            created_count: self.metrics.created_count.load(Ordering::Relaxed),
            stopped_count: self.metrics.stopped_count.load(Ordering::Relaxed),
            active_count: self.agents.len() as u64,
            total_input_bytes: self.metrics.total_input_bytes.load(Ordering::Relaxed),
            total_output_bytes: self.metrics.total_output_bytes.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum StopSignal {
    Term,
    Kill,
}

impl AgentManagerImpl {
    pub fn resolve_binary_path(&self) -> Result<String, AgentError> {
        self.resolve_binary()
    }
}

impl Drop for AgentHandle {
    fn drop(&mut self) {
        // Best-effort termination to avoid zombie processes
        if let Some(_id) = self.child.get_mut().id() {
            let _ = self.child.get_mut().start_kill();
        }
    }
}

#[derive(Default)]
pub struct AgentMetrics {
    pub created_count: AtomicU64,
    pub stopped_count: AtomicU64,
    pub total_input_bytes: AtomicU64,
    pub total_output_bytes: AtomicU64,
}

#[derive(serde::Serialize)]
pub struct AgentMetricsSnapshot {
    pub created_count: u64,
    pub stopped_count: u64,
    pub active_count: u64,
    pub total_input_bytes: u64,
    pub total_output_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::model::CreateAgentRequest;
    use tokio::time::{sleep, Duration};

    fn test_bin() -> String {
        // Cross-platform stand-in that echoes stdin → stdout
        #[cfg(unix)]
        { "/bin/cat".to_string() }
        #[cfg(windows)]
        { "cmd.exe".to_string() }
    }

    fn test_args() -> Vec<String> {
        #[cfg(unix)]
        { vec![] }
        #[cfg(windows)]
        { vec!["/C".into(), "more".into()] }
    }

    #[tokio::test]
    async fn lifecycle_create_send_buffer() {
        let manager = AgentManagerImpl::new(Some(test_bin()), 64 * 1024);
        let req = CreateAgentRequest {
            name: Some("t1".into()),
            working_dir: None,
            env: Default::default(),
            args: test_args(),
        };
        let created = manager.create(req).await.expect("create");
        manager
            .send_input(&created.agent_id, "hello world")
            .await
            .expect("send");
        sleep(Duration::from_millis(100)).await;
        let buf = manager.get_buffer(&created.agent_id).await.expect("buffer");
        assert!(buf.contains("hello world"));
    }

    #[tokio::test]
    async fn lifecycle_reset_soft_clears_buffer() {
        let manager = AgentManagerImpl::new(Some(test_bin()), 64 * 1024);
        let created = manager
            .create(CreateAgentRequest {
                name: None,
                working_dir: None,
                env: Default::default(),
                args: test_args(),
            })
            .await
            .unwrap();
        manager
            .send_input(&created.agent_id, "line one")
            .await
            .unwrap();
        sleep(Duration::from_millis(100)).await;
        manager.reset(&created.agent_id, false).await.unwrap();
        let buf = manager.get_buffer(&created.agent_id).await.unwrap();
        assert!(buf.trim().is_empty());
    }

    #[tokio::test]
    async fn lifecycle_reset_hard_respawns_process() {
        let manager = AgentManagerImpl::new(Some(test_bin()), 64 * 1024);
        let created = manager
            .create(CreateAgentRequest {
                name: None,
                working_dir: None,
                env: Default::default(),
                args: test_args(),
            })
            .await
            .unwrap();
        let before = manager
            .list()
            .await
            .into_iter()
            .find(|a| a.agent_id == created.agent_id)
            .unwrap();
        manager.reset(&created.agent_id, true).await.unwrap();
        sleep(Duration::from_millis(150)).await;
        let after = manager
            .list()
            .await
            .into_iter()
            .find(|a| a.agent_id == created.agent_id)
            .unwrap();
        assert_ne!(before.pid, after.pid);
    }

    #[tokio::test]
    async fn lifecycle_stop_removes_agent() {
        let manager = AgentManagerImpl::new(Some(test_bin()), 64 * 1024);
        let created = manager
            .create(CreateAgentRequest {
                name: None,
                working_dir: None,
                env: Default::default(),
                args: vec![],
            })
            .await
            .unwrap();
        manager
            .stop(&created.agent_id, StopSignal::Term)
            .await
            .unwrap();
        sleep(Duration::from_millis(100)).await;
        let ids: Vec<_> = manager
            .list()
            .await
            .into_iter()
            .map(|a| a.agent_id)
            .collect();
        assert!(!ids.contains(&created.agent_id));
        let snap = manager.metrics_snapshot();
        assert!(snap.stopped_count >= 1);
    }
}
