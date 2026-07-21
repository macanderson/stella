//! Managed authority schema and the effective monotonic runtime policy.

use serde::{Deserialize, Serialize};

use super::{
    AgentEngineAgent, AgentEngineAgents, AgentEngineConfig, EngineAgentKind, Settings, Toggle,
    ToolsSettings,
};

/// Authority ceilings accepted from the org-managed settings file.
///
/// `off` denies the corresponding capability. An `on` value permits a later
/// explicit grant (such as repository trust), but never grants by itself.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedAuthoritySettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_prompts: Option<Toggle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_custom_tools: Option<Toggle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bash: Option<Toggle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<Toggle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_requires_host_approval: Option<Toggle>,
}

/// The monotonic authority available to runtime adapters after settings load.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorityPolicy {
    pub project_prompts_allowed: bool,
    pub project_custom_tools_allowed: bool,
    pub bash_allowed: bool,
    pub web_allowed: bool,
    pub media_requires_host_approval: bool,
}

impl Default for AuthorityPolicy {
    fn default() -> Self {
        Self {
            project_prompts_allowed: false,
            project_custom_tools_allowed: false,
            bash_allowed: true,
            web_allowed: true,
            media_requires_host_approval: true,
        }
    }
}

impl AuthorityPolicy {
    pub(super) fn compute(
        managed: Option<&ManagedAuthoritySettings>,
        managed_tools: Option<&ToolsSettings>,
        project_trusted: bool,
    ) -> Self {
        let permits = |toggle: Option<Toggle>| toggle != Some(Toggle::Off);
        Self {
            project_prompts_allowed: project_trusted
                && permits(managed.and_then(|policy| policy.project_prompts)),
            project_custom_tools_allowed: project_trusted
                && permits(managed.and_then(|policy| policy.project_custom_tools)),
            bash_allowed: permits(managed.and_then(|policy| policy.bash))
                && permits(managed_tools.and_then(|tools| tools.bash)),
            web_allowed: permits(managed.and_then(|policy| policy.web))
                && permits(managed_tools.and_then(|tools| tools.web)),
            media_requires_host_approval: managed
                .and_then(|policy| policy.media_requires_host_approval)
                .is_none_or(Toggle::is_on),
        }
    }
}

/// Remove project capability grants while retaining trusted scope values.
/// Project `off` remains effective because lower authority may narrow.
pub(super) fn restore_project_tools(merged: &mut Settings, trusted: &Settings, project: &Settings) {
    let Some(project_tools) = project.tools else {
        return;
    };
    let target = merged.tools.get_or_insert_with(ToolsSettings::default);
    if project_tools.bash == Some(Toggle::On) {
        target.bash = trusted.tools.as_ref().and_then(|tools| tools.bash);
    }
    if project_tools.web == Some(Toggle::On) {
        target.web = trusted.tools.as_ref().and_then(|tools| tools.web);
    }
}

/// Restore only prompt fields supplied by the project. Other engine settings
/// remain normal per-field configuration and cannot execute by themselves.
pub(super) fn restore_project_prompts(
    merged: &mut Settings,
    trusted: &Settings,
    project: &Settings,
) {
    let Some(project_agents) = project
        .agent_engine_config
        .as_ref()
        .and_then(|engine| engine.agents.as_ref())
    else {
        return;
    };
    for kind in EngineAgentKind::ALL {
        if project_agents
            .get(kind)
            .and_then(|agent| agent.prompt.as_ref())
            .is_none()
        {
            continue;
        }
        let trusted_prompt = trusted
            .agent_engine_config
            .as_ref()
            .and_then(|engine| engine.agent(kind))
            .and_then(|agent| agent.prompt.clone());
        let target = merged
            .agent_engine_config
            .get_or_insert_with(AgentEngineConfig::default)
            .agents
            .get_or_insert_with(AgentEngineAgents::default)
            .get_mut(kind)
            .get_or_insert_with(AgentEngineAgent::default);
        target.prompt = trusted_prompt;
    }
}

pub(super) fn apply_tool_ceiling(settings: &mut Settings, authority: AuthorityPolicy) {
    if !authority.bash_allowed || !authority.web_allowed {
        let tools = settings.tools.get_or_insert_with(ToolsSettings::default);
        if !authority.bash_allowed {
            tools.bash = Some(Toggle::Off);
        }
        if !authority.web_allowed {
            tools.web = Some(Toggle::Off);
        }
    }
}
