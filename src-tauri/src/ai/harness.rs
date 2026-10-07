//! 受控 DeepSeek Harness sidecar：进程监管 + stdio JSON-RPC（步骤 2 任务组 2）。
//!
//! 决策一/二：Harness 以进程外子进程承载，Rust 为状态权威；传输用 stdio 上的
//! JSON-RPC，不监听端口。决策三/六：Profile 与 Skill 只随包，工具授权**只信 Rust 侧**——
//! sidecar 只能以 `tool.invoke` 请求调用白名单内的业务工具，参数与额度都由 Rust 校验。
//!
//! 本模块不依赖任何具体 Harness 运行时：真实运行时是随包资产（见任务 1.x），
//! 协议与监管用任意可执行程序即可验证（测试用 Python 假 harness 真跑工具循环）。

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio_util::sync::CancellationToken;

/// 协议版本：随包清单里钉死，升级必须通过三平台协议与恢复验收。
pub const HARNESS_PROTOCOL_VERSION: &str = "1";
/// 单条 JSON-RPC 消息的最大字节数；超限即判协议违规。
pub const HARNESS_MAX_MESSAGE_BYTES: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

/// 受控错误分类：可区分、可脱敏（不含子进程 stdout 正文、路径或凭据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessError {
    Spawn(&'static str),
    Protocol(&'static str),
    Timeout(&'static str),
    Crashed(&'static str),
    Cancelled,
    ToolDenied(&'static str),
}

impl HarnessError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Spawn(_) => "harness_spawn_failed",
            Self::Protocol(_) => "harness_protocol_error",
            Self::Timeout(_) => "harness_timeout",
            Self::Crashed(_) => "harness_crashed",
            Self::Cancelled => "harness_cancelled",
            Self::ToolDenied(_) => "harness_tool_denied",
        }
    }

    pub fn detail(&self) -> &'static str {
        match self {
            Self::Spawn(detail)
            | Self::Protocol(detail)
            | Self::Timeout(detail)
            | Self::Crashed(detail)
            | Self::ToolDenied(detail) => detail,
            Self::Cancelled => "调用已取消",
        }
    }
}

/// Rust 侧的业务工具执行器：sidecar 不能持有凭据或任意下载地址，实际动作由这里完成。
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// 执行一个白名单业务工具；返回结构化结果（只能含业务任务 ID 与状态）。
    async fn execute(&self, tool: &str, params: &Value) -> Result<Value, HarnessError>;
}

/// 一次调用的工具白名单：sidecar 只能请求这些工具，其余一律 `ToolDenied`。
#[derive(Debug, Clone, Default)]
pub struct ToolAllowlist {
    allowed: HashSet<String>,
}

impl ToolAllowlist {
    pub fn new(tools: impl IntoIterator<Item = &'static str>) -> Self {
        Self {
            allowed: tools.into_iter().map(str::to_owned).collect(),
        }
    }

    pub fn contains(&self, tool: &str) -> bool {
        self.allowed.contains(tool)
    }

    pub fn deny_all() -> Self {
        Self::default()
    }
}

/// 受限子进程的监管约束。
#[derive(Debug, Clone, Copy)]
pub struct HarnessLimits {
    pub startup_timeout: Duration,
    pub per_request_timeout: Duration,
    pub shutdown_grace: Duration,
    pub idle_timeout: Duration,
    pub max_message_bytes: usize,
}

impl Default for HarnessLimits {
    fn default() -> Self {
        Self {
            startup_timeout: Duration::from_secs(10),
            per_request_timeout: Duration::from_secs(120),
            shutdown_grace: Duration::from_secs(3),
            idle_timeout: Duration::from_secs(60),
            max_message_bytes: HARNESS_MAX_MESSAGE_BYTES,
        }
    }
}

/// 一个受监管的 Harness 子进程，一次只允许一个在途请求（串行调用）。
pub struct HarnessSidecar {
    child: Child,
    stdin: ChildStdin,
    stdout_lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    executable: PathBuf,
    workspace_root: PathBuf,
    limits: HarnessLimits,
    last_active: Instant,
    next_id: u64,
}

impl HarnessSidecar {
    pub async fn spawn(
        executable: PathBuf,
        args: Vec<String>,
        workspace_root: PathBuf,
        limits: HarnessLimits,
    ) -> Result<Self, HarnessError> {
        // 受限环境：最小 PATH，HOME/TMPDIR 落工作区，不继承任何代理变量。
        let mut command = Command::new(&executable);
        command
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &workspace_root)
            .env("TMPDIR", &workspace_root)
            .current_dir(&workspace_root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|_| HarnessError::Spawn("无法启动 Harness 子进程"))?;
        let stdin = child.stdin.take().ok_or_else(|| {
            std::mem::drop(child.kill());
            HarnessError::Spawn("无法接管 Harness 标准输入")
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            std::mem::drop(child.kill());
            HarnessError::Spawn("无法接管 Harness 标准输出")
        })?;
        Ok(Self {
            child,
            stdin,
            stdout_lines: BufReader::new(stdout).lines(),
            executable,
            workspace_root,
            limits,
            last_active: Instant::now(),
            next_id: 1,
        })
    }

    pub fn executable(&self) -> &PathBuf {
        &self.executable
    }

    pub fn workspace_root(&self) -> &PathBuf {
        &self.workspace_root
    }

    /// 距上次活动的时间；供按需启动 / 空闲回收判定。
    pub fn idle_for(&self) -> Duration {
        self.last_active.elapsed()
    }

    /// 发送一个请求并等待响应；期间处理 sidecar 发出的 `tool.invoke` 请求。
    pub async fn request(
        &mut self,
        method: &str,
        params: Value,
        executor: &dyn ToolExecutor,
        allowlist: &ToolAllowlist,
        cancellation: &CancellationToken,
    ) -> Result<Value, HarnessError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id,
            method: method.to_owned(),
            params,
        };
        write_json_line(&mut self.stdin, &request, self.limits.max_message_bytes)
            .await
            .map_err(|_| HarnessError::Protocol("无法写入请求"))?;
        self.last_active = Instant::now();
        let deadline = Instant::now() + self.limits.per_request_timeout;
        loop {
            if cancellation.is_cancelled() {
                return Err(HarnessError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(HarnessError::Timeout("请求超时"));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = tokio::time::timeout(remaining, self.stdout_lines.next_line())
                .await
                .map_err(|_| HarnessError::Timeout("等待响应超时"))?
                .map_err(|_| HarnessError::Protocol("读取响应失败"))?;
            let Some(line) = line else {
                // 子进程 stdout 关闭：要么已退出，要么协议违规。
                if let Ok(Some(_)) = self.child.try_wait() {
                    return Err(HarnessError::Crashed("Harness 提前退出"));
                }
                return Err(HarnessError::Protocol("Harness 关闭了标准输出"));
            };
            if line.len() > self.limits.max_message_bytes {
                return Err(HarnessError::Protocol("消息超过大小上限"));
            }
            let value: Value = serde_json::from_str(&line)
                .map_err(|_| HarnessError::Protocol("消息不是合法 JSON"))?;
            if value.get("method").is_some() {
                // 子进程发来的请求（业务工具调用）：由 Rust 侧授权并执行。
                let child_request: JsonRpcRequest = serde_json::from_value(value.clone())
                    .map_err(|_| HarnessError::Protocol("子进程请求结构非法"))?;
                let child_response = self
                    .handle_child_request(&child_request, executor, allowlist)
                    .await?;
                write_json_line(
                    &mut self.stdin,
                    &child_response,
                    self.limits.max_message_bytes,
                )
                .await
                .map_err(|_| HarnessError::Protocol("无法写入工具响应"))?;
                continue;
            }
            let response: JsonRpcResponse = serde_json::from_value(value)
                .map_err(|_| HarnessError::Protocol("响应结构非法"))?;
            if response.id != id {
                continue;
            }
            if let Some(_error) = response.error {
                // 子进程的错误消息是不可信内容，绝不原样传播；只给固定分类。
                return Err(HarnessError::Protocol("harness 返回业务错误"));
            }
            return Ok(response.result.unwrap_or(Value::Null));
        }
    }

    /// 只信 Rust 侧的工具授权：白名单之外的调用一律拒绝，且不执行。
    async fn handle_child_request(
        &self,
        request: &JsonRpcRequest,
        executor: &dyn ToolExecutor,
        allowlist: &ToolAllowlist,
    ) -> Result<JsonRpcResponse, HarnessError> {
        let deny = |message: &'static str| JsonRpcResponse {
            jsonrpc: "2.0".to_owned(),
            id: request.id,
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: message.to_owned(),
                data: None,
            }),
        };
        if request.method != "tool.invoke" {
            return Ok(deny("未知方法"));
        }
        let tool = request
            .params
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !allowlist.contains(tool) {
            return Err(HarnessError::ToolDenied("工具不在本次调用的白名单内"));
        }
        match executor.execute(tool, &request.params).await {
            Ok(result) => Ok(JsonRpcResponse {
                jsonrpc: "2.0".to_owned(),
                id: request.id,
                result: Some(result),
                error: None,
            }),
            Err(error) => Ok(JsonRpcResponse {
                jsonrpc: "2.0".to_owned(),
                id: request.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32000,
                    message: error.code().to_owned(),
                    data: None,
                }),
            }),
        }
    }

    /// 优雅关闭：先关 stdin 通知 EOF，宽限期内退出；超时则强制终止。
    pub async fn shutdown(&mut self) {
        let _ = self.stdin.shutdown().await;
        let _ = tokio::time::timeout(self.limits.shutdown_grace, self.child.wait()).await;
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill().await;
        }
    }
}

/// 进程级 sidecar 管理器：按需启动 + 空闲回收（任务 2.5）。
pub struct HarnessManager {
    executable: PathBuf,
    args: Vec<String>,
    workspace_root: PathBuf,
    limits: HarnessLimits,
    sidecar: Option<HarnessSidecar>,
}

impl HarnessManager {
    pub fn new(
        executable: PathBuf,
        args: Vec<String>,
        workspace_root: PathBuf,
        limits: HarnessLimits,
    ) -> Self {
        Self {
            executable,
            args,
            workspace_root,
            limits,
            sidecar: None,
        }
    }

    pub fn has_live_sidecar(&self) -> bool {
        self.sidecar.is_some()
    }

    /// 空闲回收：空闲超过 `idle_timeout` 就关闭并回收子进程。
    pub async fn reclaim_idle(&mut self) {
        if let Some(sidecar) = self.sidecar.as_mut()
            && sidecar.idle_for() >= self.limits.idle_timeout
        {
            let mut sidecar = self.sidecar.take().expect("just checked");
            sidecar.shutdown().await;
        }
    }

    /// 取一个可用的 sidecar；不存在则按需启动。
    pub async fn acquire(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<&mut HarnessSidecar, HarnessError> {
        self.reclaim_idle().await;
        if self.sidecar.is_none() {
            let sidecar = HarnessSidecar::spawn(
                self.executable.clone(),
                self.args.clone(),
                self.workspace_root.clone(),
                self.limits,
            )
            .await?;
            self.sidecar = Some(sidecar);
        }
        let sidecar = self.sidecar.as_mut().expect("spawned above");
        if cancellation.is_cancelled() {
            return Err(HarnessError::Cancelled);
        }
        Ok(sidecar)
    }

    pub async fn shutdown(&mut self) {
        if let Some(mut sidecar) = self.sidecar.take() {
            sidecar.shutdown().await;
        }
    }
}

async fn write_json_line(
    stdin: &mut ChildStdin,
    value: &impl Serialize,
    max_bytes: usize,
) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    if line.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "消息超过大小上限",
        ));
    }
    line.push(b'\n');
    stdin.write_all(&line).await?;
    stdin.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 只返回固定结果、不触网的测试执行器。
    struct EchoToolExecutor;

    #[async_trait]
    impl ToolExecutor for EchoToolExecutor {
        async fn execute(&self, tool: &str, params: &Value) -> Result<Value, HarnessError> {
            if tool == "echo" {
                Ok(serde_json::json!({ "ok": true, "text": params.get("text").cloned() }))
            } else {
                Err(HarnessError::ToolDenied("未知工具"))
            }
        }
    }

    /// 写一个假 Harness 脚本：跑一次「run_skill → tool.invoke(echo) → 返回结果」的
    /// 工具循环，完全走 stdio，用系统 Python 实现。
    fn fake_harness(directory: &std::path::Path) -> PathBuf {
        let script = directory.join("fake-harness.py");
        let source = r#"
import json, sys
def read():
    return json.loads(sys.stdin.readline())
def write(obj):
    sys.stdout.write(json.dumps(obj) + "\n"); sys.stdout.flush()
req = read()
assert req["method"] == "run_skill"
child_id = req["id"] + 1000
write({"jsonrpc": "2.0", "id": child_id, "method": "tool.invoke",
       "params": {"tool": "echo", "text": "来自 skill 的文本"}})
tool_resp = read()
assert tool_resp["id"] == child_id and tool_resp["result"]["ok"] is True
write({"jsonrpc": "2.0", "id": req["id"], "result": {
       "status": "done", "echoed": tool_resp["result"]["text"]}})
"#;
        fs::write(&script, source).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&script).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script, permissions).unwrap();
        }
        script
    }

    #[tokio::test]
    async fn minimal_skill_tool_loop_round_trips_over_stdio() {
        let directory = tempfile::tempdir().unwrap();
        let script = fake_harness(directory.path());
        let mut sidecar = HarnessSidecar::spawn(
            PathBuf::from("/usr/bin/python3"),
            vec![script.to_string_lossy().into_owned()],
            directory.path().to_path_buf(),
            HarnessLimits::default(),
        )
        .await
        .expect("spawn");
        let allowlist = ToolAllowlist::new(["echo"]);
        let result = sidecar
            .request(
                "run_skill",
                serde_json::json!({ "skill": "minimal" }),
                &EchoToolExecutor,
                &allowlist,
                &CancellationToken::new(),
            )
            .await
            .expect("工具循环必须成功");
        assert_eq!(result["status"], "done");
        assert_eq!(result["echoed"], "来自 skill 的文本");
        sidecar.shutdown().await;
    }

    #[tokio::test]
    async fn tool_denied_is_reported_without_execution() {
        struct CountingDenyExecutor(Arc<AtomicUsize>);
        #[async_trait]
        impl ToolExecutor for CountingDenyExecutor {
            async fn execute(&self, _tool: &str, _params: &Value) -> Result<Value, HarnessError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::json!({}))
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let script = fake_harness(directory.path());
        let mut sidecar = HarnessSidecar::spawn(
            PathBuf::from("/usr/bin/python3"),
            vec![script.to_string_lossy().into_owned()],
            directory.path().to_path_buf(),
            HarnessLimits::default(),
        )
        .await
        .expect("spawn");
        let calls = Arc::new(AtomicUsize::new(0));
        let executor = CountingDenyExecutor(calls.clone());
        // 白名单为空：echo 被拒，执行器不得被调用。
        let denied = sidecar
            .request(
                "run_skill",
                serde_json::json!({}),
                &executor,
                &ToolAllowlist::deny_all(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("必须拒绝");
        assert_eq!(denied.code(), "harness_tool_denied");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "被拒工具不得执行");
        sidecar.shutdown().await;
    }

    #[tokio::test]
    async fn crash_is_reported_as_distinguishable_error() {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("crash.py");
        let mut file = fs::File::create(&script).unwrap();
        // 不读请求、直接退出：读不到响应，且子进程已结束 → Crashed。
        writeln!(file, "import sys; sys.exit(3)").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&script).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script, permissions).unwrap();
        }
        let mut sidecar = HarnessSidecar::spawn(
            PathBuf::from("/usr/bin/python3"),
            vec![script.to_string_lossy().into_owned()],
            directory.path().to_path_buf(),
            HarnessLimits::default(),
        )
        .await
        .expect("spawn");
        let error = sidecar
            .request(
                "run_skill",
                serde_json::json!({}),
                &EchoToolExecutor,
                &ToolAllowlist::new(["echo"]),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "harness_crashed");
    }

    #[tokio::test]
    async fn manager_starts_on_demand_and_reclaims_when_idle() {
        let directory = tempfile::tempdir().unwrap();
        let script = fake_harness(directory.path());
        let limits = HarnessLimits {
            idle_timeout: Duration::from_millis(50),
            ..HarnessLimits::default()
        };
        let mut manager = HarnessManager::new(
            PathBuf::from("/usr/bin/python3"),
            vec![script.to_string_lossy().into_owned()],
            directory.path().to_path_buf(),
            limits,
        );
        assert!(!manager.has_live_sidecar(), "按需启动前不得有常驻进程");
        {
            let sidecar = manager
                .acquire(&CancellationToken::new())
                .await
                .expect("acquire");
            let _ = sidecar.idle_for();
        }
        assert!(manager.has_live_sidecar(), "按需启动后必须有活动子进程");
        // 空闲超过阈值后被回收。
        tokio::time::sleep(Duration::from_millis(80)).await;
        manager.reclaim_idle().await;
        assert!(!manager.has_live_sidecar(), "空闲后必须回收子进程");
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn graceful_shutdown_terminates_the_child_process() {
        let directory = tempfile::tempdir().unwrap();
        let script = fake_harness(directory.path());
        let mut sidecar = HarnessSidecar::spawn(
            PathBuf::from("/usr/bin/python3"),
            vec![script.to_string_lossy().into_owned()],
            directory.path().to_path_buf(),
            HarnessLimits::default(),
        )
        .await
        .expect("spawn");
        assert!(
            sidecar.child.try_wait().unwrap().is_none(),
            "关闭前子进程存活"
        );
        sidecar.shutdown().await;
        assert!(
            sidecar.child.try_wait().unwrap().is_some(),
            "优雅关闭后不得残留子进程"
        );
    }

    #[test]
    fn protocol_messages_carry_no_tokens_or_local_paths() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: 1,
            method: "run_skill".to_owned(),
            params: serde_json::json!({ "skill": "highlight", "input": { "segments": 3 } }),
        };
        let response = JsonRpcResponse {
            jsonrpc: "2.0".to_owned(),
            id: 1,
            result: Some(serde_json::json!({ "status": "done" })),
            error: None,
        };
        for encoded in [
            serde_json::to_string(&request).unwrap(),
            serde_json::to_string(&response).unwrap(),
        ] {
            for forbidden in [
                "st-lt-v1",
                "sk-",
                "/Users/",
                "/tmp/",
                "Authorization",
                "Bearer",
            ] {
                assert!(
                    !encoded.contains(forbidden),
                    "协议消息不得包含「{forbidden}」：{encoded}"
                );
            }
        }
    }

    #[test]
    fn framing_messages_are_bounded_and_parse_round_trips() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: 7,
            method: "run_skill".to_owned(),
            params: serde_json::json!({"skill": "x"}),
        };
        let mut bytes = serde_json::to_vec(&request).unwrap();
        bytes.push(b'\n');
        assert!(bytes.len() <= HARNESS_MAX_MESSAGE_BYTES);
        let parsed: JsonRpcRequest = serde_json::from_slice(&bytes[..bytes.len() - 1]).unwrap();
        assert_eq!(parsed.id, 7);
        assert_eq!(parsed.method, "run_skill");
    }
}
