//! One revision coordinator for every management entry point.
use super::{
    agent::Agent,
    agent_config::{ConfigRecord, NodeSettings},
};
use serde_json::{Value, json};

#[derive(Clone, Copy, Default)]
pub(crate) struct CommitOptions {
    pub verified: Option<(crate::identity::NodeId, u64)>,
    pub restart: bool,
}

impl Agent {
    pub(crate) async fn apply_settings(
        &self,
        expected: u64,
        mut value: Value,
        id: &str,
        digest: &str,
        restart: bool,
    ) -> Result<Value, String> {
        let mut current = self.config.lock().await;
        if current
            .last_request
            .as_ref()
            .is_some_and(|(last, hash)| last == id && hash == digest)
        {
            return Ok(json!({"revision":current.revision,"already_committed":true}));
        }
        if expected != current.revision {
            return Err(format!(
                "revision_conflict: 当前revision为{}",
                current.revision
            ));
        }
        let object = value.as_object_mut().ok_or("配置必须是JSON对象")?;
        if object.contains_key("remote_auth") {
            return Err("密码只能通过专用命令修改，配置草稿不得包含remote_auth".into());
        }
        object.insert(
            "remote_auth".into(),
            serde_json::to_value(&current.settings.remote_auth).map_err(|e| e.to_string())?,
        );
        let settings: NodeSettings = serde_json::from_value(value).map_err(|e| e.to_string())?;
        self.commit_settings(
            &mut current,
            expected,
            settings,
            id,
            digest,
            CommitOptions {
                restart,
                ..Default::default()
            },
        )
        .await
    }
    pub(crate) async fn commit_settings(
        &self,
        current: &mut ConfigRecord,
        expected: u64,
        settings: NodeSettings,
        id: &str,
        digest: &str,
        options: CommitOptions,
    ) -> Result<Value, String> {
        if expected != current.revision {
            return Err(format!(
                "revision_conflict: 当前revision为{}",
                current.revision
            ));
        }
        settings.validate().map_err(|e| e.to_string())?;
        // These fields change network/runtime ownership. An online update must
        // never tear down a healthy node merely to learn whether it can start.
        let mut before = serde_json::to_value(&current.settings).map_err(|e| e.to_string())?;
        let mut after = serde_json::to_value(&settings).map_err(|e| e.to_string())?;
        for key in [
            "resources",
            "services",
            "forwards",
            "service_ids",
            "trusted_devices",
            "trust_sources",
            "remote_auth",
            "send_concurrency",
            "auto_resume",
            "receive_safety_mib",
            "file_limits",
        ] {
            before.as_object_mut().unwrap().remove(key);
            after.as_object_mut().unwrap().remove(key);
        }
        if before != after && !options.restart {
            return Err(
                "restart_required: 信令、协议模式、接收根或业务能力修改须停止Agent后离线应用"
                    .into(),
            );
        }
        for device in &settings.trusted_devices {
            let source = settings.trust_sources[&device.node_id];
            if current.settings.trust_sources.get(&device.node_id) != Some(&source)
                && source != super::authorization::TrustSource::Administrator
                && !(source == super::authorization::TrustSource::PasswordVerified
                    && options
                        .verified
                        .is_some_and(|(peer, _)| peer.to_hex() == device.node_id))
            {
                return Err(
                    "新增或变更授权来源必须显式为administrator；密码证明/旧数据导入有独立入口"
                        .into(),
                );
            }
        }
        let mut forwards = self.forwards.lock().await;
        let prepared = forwards
            .prepare(&settings, &self.control, None)
            .await
            .map_err(|e| e.to_string())?;
        let record = ConfigRecord {
            schema_version: 1,
            revision: current.revision.checked_add(1).ok_or("revision已耗尽")?,
            last_request: Some((id.into(), digest.into())),
            settings,
        };
        // The actor writes the commit root, then publishes policy under the
        // same event-loop turn. No grant is published on disk failure.
        let outcome = match self
            .control
            .commit_agent_policy(record.clone(), self.paths.config(), options)
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                if error.starts_with("recovery_required") {
                    self.control.shutdown();
                }
                return Err(error);
            }
        };
        let retired = if options.restart {
            self.restart_pending
                .store(true, std::sync::atomic::Ordering::Release);
            drop(prepared);
            forwards.shutdown().await;
            vec![]
        } else {
            forwards.commit(prepared)
        };
        *current = record;
        if let Err(error) = self.control.configuration_committed(current.revision) {
            self.control.shutdown();
            return Err(format!(
                "recovery_required: 配置已提交但快照不可用：{error}"
            ));
        }
        *self.last_error.lock().await = outcome.durability_warning.clone();
        let result = json!({"revision":current.revision,"durability_warning":outcome.durability_warning,"restart_required":options.restart});
        drop(forwards);
        for task in retired {
            let _ = task.await;
        }
        Ok(result)
    }
}
