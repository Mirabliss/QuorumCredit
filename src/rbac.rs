use crate::errors::ContractError;
use crate::helpers::{config, require_admin_approval};
use crate::types::{AdminPermission, AdminRole, DataKey};
use soroban_sdk::{Address, Env, Vec};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdminAction {
    AddAdmin,
    RemoveAdmin,
    RotateAdmin,
    SetAdminThreshold,
    Pause,
    Unpause,
    Upgrade,
    SetConfig,
    UpdateConfig,
    UpdateFees,
    Slash,
    RevokeAdmin,
    ManageWhitelist,
    ManageBlacklisted,
    SetProtocolFee,
    SetLoanParams,
    SetReputationNft,
    ManageDynamicSlash,
    EmergencyUnpause,
}

/// Map each AdminAction to the required AdminPermission
fn get_required_permission(action: AdminAction) -> AdminPermission {
    match action {
        AdminAction::AddAdmin
        | AdminAction::RemoveAdmin
        | AdminAction::RotateAdmin
        | AdminAction::SetAdminThreshold
        | AdminAction::RevokeAdmin
        | AdminAction::ManageWhitelist => AdminPermission::UpdateConfig,

        AdminAction::Pause
        | AdminAction::Unpause
        | AdminAction::EmergencyUnpause => AdminPermission::Pause,

        AdminAction::Upgrade => AdminPermission::UpdateConfig,

        AdminAction::SetConfig
        | AdminAction::UpdateConfig
        | AdminAction::SetLoanParams
        | AdminAction::SetReputationNft
        | AdminAction::ManageDynamicSlash => AdminPermission::UpdateConfig,

        AdminAction::UpdateFees
        | AdminAction::SetProtocolFee => AdminPermission::ManageFees,

        AdminAction::Slash => AdminPermission::Slash,

        AdminAction::ManageBlacklisted => AdminPermission::UpdateConfig,
    }
}

/// Assigns an admin role to an address. Requires admin authorization.
pub fn assign_admin_role(
    env: &Env,
    admin_signers: Vec<Address>,
    target_admin: Address,
    role: AdminRole,
) {
    require_admin_approval(env, &admin_signers);

    env.storage().persistent().set(&DataKey::AdminRole(target_admin.clone()), &role);

    env.events().publish(
        ("admin", "role_assigned"),
        (admin_signers.get(0), &target_admin, &role),
    );
}

/// Returns the role of an admin, or error if not set.
pub fn get_admin_role(env: &Env, admin: &Address) -> Result<AdminRole, ContractError> {
    env.storage()
        .persistent()
        .get::<_, AdminRole>(&DataKey::AdminRole(admin.clone()))
        .ok_or(ContractError::PermissionDenied)
}

/// Checks if an admin has a specific permission.
pub fn check_admin_permission(
    role: &AdminRole,
    permission: &AdminPermission,
) -> bool {
    match role {
        AdminRole::SuperAdmin => true,
        AdminRole::Treasurer => {
            matches!(permission, AdminPermission::UpdateConfig | AdminPermission::ManageFees)
        }
        AdminRole::Monitor => matches!(permission, AdminPermission::ReadAnalytics),
    }
}

/// Checks if all signers meet the threshold AND all have the required permission.
/// Both checks are required — this is an AND gate, not an OR.
/// Returns Err if ANY signer lacks the required permission, regardless of threshold.
/// This is the primary RBAC enforcement point for all admin actions.
pub fn require_admin_approval_with_permission(
    env: &Env,
    admin_signers: &Vec<Address>,
    required_permission: AdminPermission,
) -> Result<(), ContractError> {
    let cfg = config(env);

    if admin_signers.len() < cfg.admin_threshold {
        return Err(ContractError::UnauthorizedCaller);
    }

    for signer in admin_signers.iter() {
        if !cfg.admins.iter().any(|a| a == signer) {
            return Err(ContractError::UnauthorizedCaller);
        }

        let revoked: bool = env
            .storage()
            .persistent()
            .get(&DataKey::RevokedAdmin(signer.clone()))
            .unwrap_or(false);
        if revoked {
            return Err(ContractError::UnauthorizedCaller);
        }

        let role = get_admin_role(env, &signer)?;
        if !check_admin_permission(&role, &required_permission) {
            return Err(ContractError::PermissionDenied);
        }

        signer.require_auth();
    }

    Ok(())
}

/// Convenience wrapper for require_admin_approval_with_permission that automatically
/// determines the required permission from an AdminAction. Use this in most admin functions.
pub fn require_admin_approval_for_action(
    env: &Env,
    admin_signers: &Vec<Address>,
    action: AdminAction,
) -> Result<(), ContractError> {
    let required_permission = get_required_permission(action);
    require_admin_approval_with_permission(env, admin_signers, required_permission)
}

/// Requires that an admin has a specific permission. Returns Err if denied.
pub fn require_admin_permission(
    env: &Env,
    admin: &Address,
    permission: AdminPermission,
) -> Result<(), ContractError> {
    let role = get_admin_role(env, admin)?;

    if check_admin_permission(&role, &permission) {
        Ok(())
    } else {
        Err(ContractError::PermissionDenied)
    }
}

/// Migration helper: Assigns SuperAdmin role to all existing admins who don't have a role yet.
/// This ensures backward compatibility when upgrading to RBAC-enforced contract.
///
/// Call this once after deploying the RBAC-aware contract to restore full functionality
/// for existing deployments. Admins can then gradually refine their roles.
pub fn migrate_legacy_admins_to_superadmin(env: &Env) {
    let cfg = config(env);

    for admin in cfg.admins.iter() {
        if get_admin_role(env, &admin).is_err() {
            env.storage()
                .persistent()
                .set(&DataKey::AdminRole(admin.clone()), &AdminRole::SuperAdmin);
        }
    }

    env.events().publish(
        ("rbac", "legacy_migration_complete"),
        cfg.admins.len() ,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AdminPermission, AdminRole};

    #[test]
    fn test_superadmin_all_permissions() {
        let permissions = [
            AdminPermission::Slash,
            AdminPermission::Pause,
            AdminPermission::UpdateConfig,
            AdminPermission::ManageFees,
            AdminPermission::ReadAnalytics,
        ];

        for perm in permissions {
            assert!(
                check_admin_permission(&AdminRole::SuperAdmin, &perm),
                "SuperAdmin should have {:?}",
                perm
            );
        }
    }

    #[test]
    fn test_treasurer_config_and_fees() {
        assert!(check_admin_permission(
            &AdminRole::Treasurer,
            &AdminPermission::UpdateConfig
        ));
        assert!(check_admin_permission(
            &AdminRole::Treasurer,
            &AdminPermission::ManageFees
        ));
        assert!(!check_admin_permission(
            &AdminRole::Treasurer,
            &AdminPermission::Slash
        ));
        assert!(!check_admin_permission(
            &AdminRole::Treasurer,
            &AdminPermission::Pause
        ));
        assert!(!check_admin_permission(
            &AdminRole::Treasurer,
            &AdminPermission::ReadAnalytics
        ));
    }

    #[test]
    fn test_monitor_read_only() {
        assert!(check_admin_permission(
            &AdminRole::Monitor,
            &AdminPermission::ReadAnalytics
        ));
        assert!(!check_admin_permission(
            &AdminRole::Monitor,
            &AdminPermission::Slash
        ));
        assert!(!check_admin_permission(
            &AdminRole::Monitor,
            &AdminPermission::Pause
        ));
        assert!(!check_admin_permission(
            &AdminRole::Monitor,
            &AdminPermission::UpdateConfig
        ));
        assert!(!check_admin_permission(
            &AdminRole::Monitor,
            &AdminPermission::ManageFees
        ));
    }

    #[test]
    fn test_action_to_permission_mapping() {
        assert_eq!(
            get_required_permission(AdminAction::Pause),
            AdminPermission::Pause
        );
        assert_eq!(
            get_required_permission(AdminAction::Slash),
            AdminPermission::Slash
        );
        assert_eq!(
            get_required_permission(AdminAction::SetAdminThreshold),
            AdminPermission::UpdateConfig
        );
        assert_eq!(
            get_required_permission(AdminAction::UpdateFees),
            AdminPermission::ManageFees
        );
    }
}
