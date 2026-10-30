//! Roles of platform users and what each one may do. Every rights question asks `allows`, so a
//! change of rights is a change of this file (or of `ADMIN_STATS_ROLES`) only.

use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Moderator,
    Admin,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::User, Role::Moderator, Role::Admin];

    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Moderator => "moderator",
            Role::Admin => "admin",
        }
    }

    /// Moderators and admins: the people behind `/admin`.
    pub fn is_staff(self) -> bool {
        self != Role::User
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Role {
    type Err = String;

    fn from_str(s: &str) -> Result<Role, String> {
        Role::ALL.into_iter().find(|r| r.as_str() == s.trim()).ok_or_else(|| s.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// Everything under `/admin` except the rights below: review, reports, bans, addons, lists.
    Moderate,
    /// Gives and takes roles, and bans moderators.
    ManageRoles,
    /// The detailed statistics on `/admin/stats`; who has it comes from `ADMIN_STATS_ROLES`.
    ViewAdminStats,
    /// Grafana and the rest of the monitoring stack.
    ViewGrafana,
}

impl Permission {
    pub const ALL: [Permission; 4] = [Permission::Moderate, Permission::ManageRoles, Permission::ViewAdminStats, Permission::ViewGrafana];

    pub fn as_str(self) -> &'static str {
        match self {
            Permission::Moderate => "moderate",
            Permission::ManageRoles => "manage_roles",
            Permission::ViewAdminStats => "view_admin_stats",
            Permission::ViewGrafana => "view_grafana",
        }
    }
}

/// Whether `role` has `permission`; `stats_roles` are the roles of `ADMIN_STATS_ROLES`.
pub fn allows(role: Role, permission: Permission, stats_roles: &[Role]) -> bool {
    match permission {
        Permission::Moderate => role.is_staff(),
        Permission::ManageRoles | Permission::ViewGrafana => role == Role::Admin,
        Permission::ViewAdminStats => stats_roles.contains(&role),
    }
}

/// Every permission of `role`, as names for clients.
pub fn permissions(role: Role, stats_roles: &[Role]) -> Vec<&'static str> {
    Permission::ALL.into_iter().filter(|p| allows(role, *p, stats_roles)).map(Permission::as_str).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use Permission::*;
    use Role::*;

    #[test]
    fn allows_matrix() {
        let stats = [Moderator, Admin];
        let table = [
            (User, [false, false, false, false]),
            (Moderator, [true, false, true, false]),
            (Admin, [true, true, true, true]),
        ];
        for (role, expected) in table {
            let got: Vec<bool> = Permission::ALL.into_iter().map(|p| allows(role, p, &stats)).collect();
            assert_eq!(got, expected, "{role}");
        }
    }

    #[test]
    fn stats_roles_decide_admin_stats() {
        assert!(!allows(Moderator, ViewAdminStats, &[Admin]));
        assert!(allows(Admin, ViewAdminStats, &[Admin]));
        assert!(!allows(Admin, ViewAdminStats, &[]), "the configuration alone decides");
        assert!(allows(Moderator, Moderate, &[]), "other rights do not follow it");
    }

    #[test]
    fn parses_names() {
        assert_eq!("moderator".parse::<Role>(), Ok(Moderator));
        assert_eq!(" admin ".parse::<Role>(), Ok(Admin));
        assert!("root".parse::<Role>().is_err());
        assert_eq!(permissions(Moderator, &[Moderator]), ["moderate", "view_admin_stats"]);
    }
}
