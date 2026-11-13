//! Role based access control. Every gRPC command maps to (verb, kind, namespace)
//! and is checked against the caller's roles.

use serde::{Deserialize, Serialize};

pub mod verbs {
    pub const GET: &str = "get";
    pub const LIST: &str = "list";
    pub const WATCH: &str = "watch";
    pub const CREATE: &str = "create";
    pub const UPDATE: &str = "update";
    pub const DELETE: &str = "delete";
    pub const SCALE: &str = "scale";
    pub const LOGS: &str = "logs";
    pub const STATUS: &str = "status";

    pub const ALL: &[&str] = &[GET, LIST, WATCH, CREATE, UPDATE, DELETE, SCALE, LOGS, STATUS];
}

/// Pseudo kinds protecting the identity / administration surface.
pub mod system_kinds {
    pub const USERS: &str = "system:users";
    pub const ROLES: &str = "system:roles";
    pub const TOKENS: &str = "system:tokens";
    pub const OPERATORS: &str = "system:operators";
}

pub const WILDCARD: &str = "*";
pub const SYSTEM_WILDCARD: &str = "system:*";
pub const SYSTEM_PREFIX: &str = "system:";

pub mod roles {
    pub const ADMIN: &str = "admin";
    pub const EDITOR: &str = "editor";
    pub const VIEWER: &str = "viewer";
    pub const OPERATOR: &str = "operator";
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRule {
    pub verbs: Vec<String>,
    pub kinds: Vec<String>,
    /// Empty = every namespace (and cluster-scoped objects).
    #[serde(default)]
    pub namespaces: Vec<String>,
}

impl PolicyRule {
    pub fn new(verbs: &[&str], kinds: &[&str], namespaces: &[&str]) -> Self {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect();
        Self { verbs: v(verbs), kinds: v(kinds), namespaces: v(namespaces) }
    }

    /// Does this rule grant `verb` on `kind` in `namespace`?
    ///
    /// Requests may themselves contain wildcards ("*"), in which case only rules that
    /// grant the wildcard match. This makes `matches` usable as a conservative
    /// "is this permission a subset of mine?" check, preventing privilege escalation.
    pub fn matches(&self, verb: &str, kind: &str, namespace: &str) -> bool {
        let verb_ok = self.verbs.iter().any(|v| v == WILDCARD || v == verb);
        let is_system = kind.starts_with(SYSTEM_PREFIX);
        let kind_ok = self.kinds.iter().any(|k| {
            k == kind || (k == WILDCARD && !is_system) || (k == SYSTEM_WILDCARD && is_system)
        });
        let ns_ok = self.namespaces.is_empty() || self.namespaces.iter().any(|n| n == WILDCARD || n == namespace);
        verb_ok && kind_ok && ns_ok
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    pub name: String,
    pub rules: Vec<PolicyRule>,
    #[serde(default)]
    pub builtin: bool,
}

impl Role {
    pub fn allows(&self, verb: &str, kind: &str, namespace: &str) -> bool {
        self.rules.iter().any(|r| r.matches(verb, kind, namespace))
    }
}

pub fn is_allowed(roles: &[Role], verb: &str, kind: &str, namespace: &str) -> bool {
    roles.iter().any(|r| r.allows(verb, kind, namespace))
}

pub fn builtin_roles() -> Vec<Role> {
    let role = |name: &str, rules: Vec<PolicyRule>| Role { name: name.into(), rules, builtin: true };
    vec![
        role(
            roles::ADMIN,
            vec![PolicyRule::new(&[WILDCARD], &[WILDCARD], &[]), PolicyRule::new(&[WILDCARD], &[SYSTEM_WILDCARD], &[])],
        ),
        role(roles::EDITOR, vec![PolicyRule::new(verbs::ALL, &[WILDCARD], &[])]),
        role(
            roles::VIEWER,
            vec![PolicyRule::new(&[verbs::GET, verbs::LIST, verbs::WATCH, verbs::LOGS], &[WILDCARD], &[])],
        ),
        // Base role for out-of-process operators: read everything. Each operator also
        // receives a generated `operator:<name>` role for its own CRDs.
        role(
            roles::OPERATOR,
            vec![PolicyRule::new(&[verbs::GET, verbs::LIST, verbs::WATCH], &[WILDCARD], &[])],
        ),
    ]
}

pub fn builtin_role(name: &str) -> Option<Role> {
    builtin_roles().into_iter().find(|r| r.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles(names: &[&str]) -> Vec<Role> {
        names.iter().map(|n| builtin_role(n).unwrap()).collect()
    }

    #[test]
    fn admin_can_do_everything() {
        let r = roles(&[roles::ADMIN]);
        assert!(is_allowed(&r, verbs::DELETE, "Pod", "prod"));
        assert!(is_allowed(&r, verbs::CREATE, system_kinds::USERS, ""));
        assert!(is_allowed(&r, WILDCARD, WILDCARD, WILDCARD));
    }

    #[test]
    fn editor_cannot_touch_system_kinds() {
        let r = roles(&[roles::EDITOR]);
        assert!(is_allowed(&r, verbs::CREATE, "Deployment", "default"));
        assert!(is_allowed(&r, verbs::SCALE, "Deployment", "default"));
        assert!(!is_allowed(&r, verbs::LIST, system_kinds::USERS, ""));
        assert!(!is_allowed(&r, verbs::CREATE, system_kinds::TOKENS, ""));
    }

    #[test]
    fn viewer_is_read_only() {
        let r = roles(&[roles::VIEWER]);
        assert!(is_allowed(&r, verbs::GET, "Pod", "default"));
        assert!(is_allowed(&r, verbs::LOGS, "Pod", "default"));
        assert!(!is_allowed(&r, verbs::CREATE, "Pod", "default"));
        assert!(!is_allowed(&r, verbs::DELETE, "Pod", "default"));
    }

    #[test]
    fn namespace_scoped_rules() {
        let r = vec![Role {
            name: "dev-deployer".into(),
            rules: vec![PolicyRule::new(&[verbs::CREATE, verbs::GET], &["Deployment"], &["dev"])],
            builtin: false,
        }];
        assert!(is_allowed(&r, verbs::CREATE, "Deployment", "dev"));
        assert!(!is_allowed(&r, verbs::CREATE, "Deployment", "prod"));
        assert!(!is_allowed(&r, verbs::CREATE, "Pod", "dev"));
        assert!(!is_allowed(&r, verbs::DELETE, "Deployment", "dev"));
        // listing across all namespaces requires an all-namespace grant
        assert!(!is_allowed(&r, verbs::GET, "Deployment", WILDCARD));
    }

    #[test]
    fn wildcard_requests_only_match_wildcard_grants() {
        let r = roles(&[roles::EDITOR]);
        assert!(!is_allowed(&r, WILDCARD, "Pod", "default"));
        assert!(is_allowed(&r, verbs::GET, WILDCARD, "default"));
    }

    #[test]
    fn no_roles_no_access() {
        assert!(!is_allowed(&[], verbs::GET, "Pod", "default"));
    }
}
