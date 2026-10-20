use super::{
    agent_paths::AgentPaths,
    ipc::{self, Command, Request, Response},
};
use std::{io, time::Duration};

#[derive(Clone)]
pub struct AgentClient {
    paths: AgentPaths,
}
impl AgentClient {
    pub fn new(paths: AgentPaths) -> Self {
        Self { paths }
    }
    pub async fn execute(&self, command: Command) -> io::Result<Response> {
        self.request(Request::new(command)).await
    }
    pub async fn request(&self, request: Request) -> io::Result<Response> {
        request.validate()?;
        tokio::time::timeout(
            Duration::from_millis(u64::from(request.timeout_ms) + 5000),
            async {
                let mut stream = super::ipc_transport::connect(&self.paths).await?;
                ipc::write(&mut *stream, &request).await?;
                let response: Response = ipc::read(&mut *stream).await?;
                if response.api_version != ipc::API_VERSION
                    || response.request_id != request.request_id
                {
                    return Err(io::Error::other("IPC响应版本或请求ID不匹配"));
                }
                Ok(response)
            },
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "管理请求超时；变更结果须查询确认"))?
    }
}
