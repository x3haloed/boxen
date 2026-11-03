//! Permissions and attention are independent. Interest never grants authority.
use crate::{Result, messaging::opaque};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}
impl Scope {
    pub fn validate(&self) -> Result<()> {
        if !matches!(
            self.kind.as_str(),
            "all" | "conversation" | "space" | "parent" | "user" | "direct"
        ) {
            return Err("attention and permission scopes are all, conversation, space, parent, user or direct".into());
        }
        if matches!(self.kind.as_str(), "all" | "direct") {
            if self.id.is_some() {
                return Err("all and direct scopes have no id".into());
            }
        } else {
            opaque(self.id.as_deref().ok_or("this scope has an id")?, 256)?;
        }
        Ok(())
    }
    pub fn matches(&self, conversation: &str, native: &Value) -> bool {
        match self.kind.as_str() {
            "all" => true,
            "conversation" => self.id.as_deref() == Some(conversation),
            "space" => native["space"].as_str() == self.id.as_deref() && self.id.is_some(),
            "parent" => native["parent"].as_str() == self.id.as_deref() && self.id.is_some(),
            "user" => native["author"]["id"].as_str() == self.id.as_deref() && self.id.is_some(),
            "direct" => native["direct"] == true,
            _ => false,
        }
    }
    pub fn possible(&self, conversation: &str) -> bool {
        self.kind != "conversation" || self.id.as_deref() == Some(conversation)
    }
}
fn allow() -> String {
    "allow".into()
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub resident: String,
    pub scope: Scope,
    pub actions: Vec<String>,
    #[serde(default = "allow")]
    pub effect: String,
    #[serde(default)]
    pub accept_bots: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub grants: Vec<Grant>,
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        if self.grants.len() > 64 {
            return Err("policy contains at most 64 grants".into());
        }
        for g in &self.grants {
            crate::jobs::valid_id(&g.resident)?;
            g.scope.validate()?;
            if !matches!(g.effect.as_str(), "allow" | "deny")
                || g.actions.is_empty()
                || g.actions.len() > 6
                || g.actions.iter().any(|a| {
                    !matches!(
                        a.as_str(),
                        "read" | "receive" | "send" | "react" | "files" | "presence"
                    )
                })
            {
                return Err("policy grants have allow/deny effect and bounded read, receive, send, react, files or presence actions".into());
            }
        }
        Ok(())
    }
    pub fn permits(
        &self,
        resident: &str,
        action: &str,
        conversation: &str,
        native: &Value,
    ) -> bool {
        let matches: Vec<_> = self
            .grants
            .iter()
            .filter(|g| {
                g.resident == resident
                    && g.actions.iter().any(|a| a == action)
                    && g.scope.matches(conversation, native)
            })
            .collect();
        !matches.iter().any(|g| g.effect == "deny")
            && matches.iter().any(|g| {
                g.effect == "allow"
                    && (action != "receive" || native["author"]["bot"] != true || g.accept_bots)
            })
    }
    // Queuing is provisional when service metadata is needed. Dispatch rechecks
    // complete native context supplied by the trusted connector, never the model.
    pub fn potential(&self, resident: &str, action: &str, conversation: &str) -> bool {
        !self.grants.iter().any(|g| {
            g.resident == resident
                && g.effect == "deny"
                && g.actions.iter().any(|a| a == action)
                && matches!(g.scope.kind.as_str(), "all" | "conversation")
                && g.scope.possible(conversation)
        }) && self.grants.iter().any(|g| {
            g.resident == resident
                && g.effect == "allow"
                && g.actions.iter().any(|a| a == action)
                && g.scope.possible(conversation)
        })
    }
}
fn yes() -> bool {
    true
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Attention {
    pub direct_messages: bool,
    pub mentions: bool,
    pub replies: bool,
    pub reactions: bool,
    pub watched: Vec<Scope>,
    pub muted: Vec<Scope>,
}
impl Default for Attention {
    fn default() -> Self {
        Self {
            direct_messages: yes(),
            mentions: yes(),
            replies: yes(),
            reactions: yes(),
            watched: vec![],
            muted: vec![],
        }
    }
}
impl Attention {
    pub fn validate(&self) -> Result<()> {
        if self.watched.len() > 64 || self.muted.len() > 64 {
            return Err("attention has at most 64 watched and 64 muted scopes".into());
        }
        for s in self.watched.iter().chain(&self.muted) {
            s.validate()?;
        }
        Ok(())
    }
    pub fn interested(&self, conversation: &str, native: &Value) -> bool {
        !self.muted.iter().any(|s| s.matches(conversation, native))
            && (self.watched.iter().any(|s| s.matches(conversation, native))
                || (self.direct_messages && native["direct"] == true)
                || (self.mentions && native["mention"] == true)
                || (self.replies && native["reply"] == true))
    }
}
