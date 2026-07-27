use crate::errors::ContractError;
use crate::helpers::{config, require_not_paused};
use crate::types::{
    BondInsuranceRecord, BondStats, BondStatus, DataKey, InsuranceStatus, VouchProtectionBond,
};
use soroban_sdk::{Address, Env};

/// Issue #1175: Bond insurance premium rate (3% = 300 basis points)
const BOND_INSURANCE_PREMIUM_BPS: i128 = 300;
/// Issue #1175: Maximum bond coverage as percentage of vouch stake (50%)
const MAX_BOND_COVERAGE_BPS: i128 = 5000;

/// Issue #1175: Stake a bond for vouch protection.
/// The bond covers up to 50% of the vouch amount against slashing.
pub fn stake_bond_for_vouch_protection(
    env: Env,
    loan_id: u64,
    vouch_id: u64,
    voucher: Address,
    protected_stake: i128,
    bond_amount: i128,
) -> Result<(), ContractError> {
    require_not_paused(&env)?;

    // Validate bond amount
    if bond_amount <= 0 {
        return Err(ContractError::InvalidAmount);
    }

    // Bond can cover up to 50% of the vouch stake
    let max_bond = (protected_stake * MAX_BOND_COVERAGE_BPS) / 10_000;
    if bond_amount > max_bond {
        return Err(ContractError::InvalidAmount);
    }

    // Voucher must authorize the bond
    voucher.require_auth();

    // Create the protection bond
    let bond = VouchProtectionBond {
        voucher: voucher.clone(),
        loan_id,
        vouch_id,
        bond_amount,
        protected_stake,
        created_at: env.ledger().timestamp(),
        amount_used: 0,
        released_at: None,
        status: BondStatus::Active,
        has_insurance: false,
    };

    // Store the bond
    env.storage()
        .persistent()
        .set(&DataKey::VouchProtectionBond(voucher.clone(), loan_id), &bond);

    // Update bond stats
    let mut stats: BondStats = env
        .storage()
        .persistent()
        .get(&DataKey::BondStats(voucher.clone()))
        .unwrap_or(BondStats {
            voucher: voucher.clone(),
            total_bonded: 0,
            total_used: 0,
            active_bonds: 0,
            times_bond_used: 0,
            total_insurance_premiums: 0,
            insurance_claims_paid: 0,
            total_insurance_payout: 0,
            last_activity: env.ledger().timestamp(),
        });

    stats.total_bonded = stats.total_bonded.checked_add(bond_amount)
        .ok_or(ContractError::ArithmeticError)?;
    stats.active_bonds += 1;
    stats.last_activity = env.ledger().timestamp();

    env.storage()
        .persistent()
        .set(&DataKey::BondStats(voucher), &stats);

    Ok(())
}

/// Issue #1175: Purchase optional bond insurance (3% premium).
pub fn purchase_bond_insurance(
    env: Env,
    loan_id: u64,
    voucher: Address,
    bond_amount: i128,
) -> Result<(), ContractError> {
    require_not_paused(&env)?;

    let mut bond: VouchProtectionBond = env
        .storage()
        .persistent()
        .get(&DataKey::VouchProtectionBond(voucher.clone(), loan_id))
        .ok_or(ContractError::InvalidAmount)?;

    if bond.has_insurance {
        return Err(ContractError::InvalidAmount);
    }

    // Calculate premium: 3% of bond amount
    let premium = (bond_amount * BOND_INSURANCE_PREMIUM_BPS) / 10_000;

    // Create insurance record
    let insurance = BondInsuranceRecord {
        voucher: voucher.clone(),
        loan_id,
        insured_bond_amount: bond_amount,
        premium_paid: premium,
        max_coverage: bond_amount,
        amount_claimed: 0,
        status: InsuranceStatus::Active,
        purchased_at: env.ledger().timestamp(),
    };

    env.storage()
        .persistent()
        .set(&DataKey::BondInsurance(voucher.clone(), loan_id), &insurance);

    // Mark bond as having insurance
    bond.has_insurance = true;
    env.storage()
        .persistent()
        .set(&DataKey::VouchProtectionBond(voucher.clone(), loan_id), &bond);

    // Update stats
    let mut stats: BondStats = env
        .storage()
        .persistent()
        .get(&DataKey::BondStats(voucher))
        .ok_or(ContractError::InvalidAmount)?;

    stats.total_insurance_premiums = stats.total_insurance_premiums.checked_add(premium)
        .ok_or(ContractError::ArithmeticError)?;
    stats.last_activity = env.ledger().timestamp();

    env.storage()
        .persistent()
        .set(&DataKey::BondStats(voucher), &stats);

    Ok(())
}

/// Issue #1175: Use bond to cover a slash.
/// Called when a vouch is slashed to apply bond coverage first.
pub fn apply_bond_coverage(
    env: &Env,
    loan_id: u64,
    voucher: &Address,
    slash_amount: i128,
) -> Result<i128, ContractError> {
    let mut bond: VouchProtectionBond = env
        .storage()
        .persistent()
        .get(&DataKey::VouchProtectionBond(voucher.clone(), loan_id))
        .ok_or(ContractError::InvalidAmount)?;

    if bond.status == BondStatus::Released || bond.status == BondStatus::Exhausted {
        return Err(ContractError::InvalidAmount);
    }

    let available_bond = bond.bond_amount - bond.amount_used;
    let bond_used = slash_amount.min(available_bond);

    bond.amount_used = bond.amount_used.checked_add(bond_used)
        .ok_or(ContractError::ArithmeticError)?;

    // Update bond status
    if bond.amount_used >= bond.bond_amount {
        bond.status = BondStatus::Exhausted;
    } else {
        bond.status = BondStatus::PartiallyUsed;
    }

    env.storage()
        .persistent()
        .set(&DataKey::VouchProtectionBond(voucher.clone(), loan_id), &bond);

    // Update stats
    let mut stats: BondStats = env
        .storage()
        .persistent()
        .get(&DataKey::BondStats(voucher.clone()))
        .ok_or(ContractError::InvalidAmount)?;

    stats.total_used = stats.total_used.checked_add(bond_used)
        .ok_or(ContractError::ArithmeticError)?;
    stats.times_bond_used += 1;

    // Check if insurance should cover the shortfall
    if bond.has_insurance && bond_used < slash_amount {
        let insurance: Option<BondInsuranceRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::BondInsurance(voucher.clone(), loan_id));

        if let Some(mut insurance_record) = insurance {
            if insurance_record.status == InsuranceStatus::Active {
                let shortfall = slash_amount - bond_used;
                let insurance_payout = shortfall.min(insurance_record.max_coverage - insurance_record.amount_claimed);

                if insurance_payout > 0 {
                    insurance_record.amount_claimed = insurance_record.amount_claimed.checked_add(insurance_payout)
                        .ok_or(ContractError::ArithmeticError)?;

                    if insurance_record.amount_claimed >= insurance_record.max_coverage {
                        insurance_record.status = InsuranceStatus::Claimed;
                    }

                    env.storage()
                        .persistent()
                        .set(&DataKey::BondInsurance(voucher.clone(), loan_id), &insurance_record);

                    stats.insurance_claims_paid += 1;
                    stats.total_insurance_payout = stats.total_insurance_payout.checked_add(insurance_payout)
                        .ok_or(ContractError::ArithmeticError)?;

                    return Ok(bond_used + insurance_payout);
                }
            }
        }
    }

    stats.last_activity = env.ledger().timestamp();
    env.storage()
        .persistent()
        .set(&DataKey::BondStats(voucher.clone()), &stats);

    Ok(bond_used)
}

/// Issue #1175: Release bond after loan completion.
/// Refund any unused bond amount to the voucher.
pub fn release_bond(
    env: Env,
    loan_id: u64,
    voucher: Address,
) -> Result<i128, ContractError> {
    require_not_paused(&env)?;

    let mut bond: VouchProtectionBond = env
        .storage()
        .persistent()
        .get(&DataKey::VouchProtectionBond(voucher.clone(), loan_id))
        .ok_or(ContractError::InvalidAmount)?;

    if bond.status == BondStatus::Released {
        return Err(ContractError::InvalidAmount);
    }

    // Calculate refund amount
    let refund_amount = bond.bond_amount - bond.amount_used;

    // Update bond status
    bond.status = BondStatus::Released;
    bond.released_at = Some(env.ledger().timestamp());

    env.storage()
        .persistent()
        .set(&DataKey::VouchProtectionBond(voucher.clone(), loan_id), &bond);

    // Release insurance if present
    if bond.has_insurance {
        let mut insurance: BondInsuranceRecord = env
            .storage()
            .persistent()
            .get(&DataKey::BondInsurance(voucher.clone(), loan_id))
            .ok_or(ContractError::InvalidAmount)?;

        if insurance.status == InsuranceStatus::Active {
            insurance.status = InsuranceStatus::Released;
            env.storage()
                .persistent()
                .set(&DataKey::BondInsurance(voucher.clone(), loan_id), &insurance);
        }
    }

    // Update stats
    let mut stats: BondStats = env
        .storage()
        .persistent()
        .get(&DataKey::BondStats(voucher))
        .ok_or(ContractError::InvalidAmount)?;

    stats.active_bonds = stats.active_bonds.saturating_sub(1);
    stats.last_activity = env.ledger().timestamp();

    env.storage()
        .persistent()
        .set(&DataKey::BondStats(voucher), &stats);

    Ok(refund_amount)
}

/// Issue #1175: Get bond protection record.
pub fn get_bond(env: Env, loan_id: u64, voucher: Address) -> Result<VouchProtectionBond, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::VouchProtectionBond(voucher, loan_id))
        .ok_or(ContractError::InvalidAmount)
}

/// Issue #1175: Get bond insurance record.
pub fn get_bond_insurance(
    env: Env,
    loan_id: u64,
    voucher: Address,
) -> Result<BondInsuranceRecord, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::BondInsurance(voucher, loan_id))
        .ok_or(ContractError::InvalidAmount)
}

/// Issue #1175: Get bond statistics for a voucher.
pub fn get_bond_stats(env: Env, voucher: Address) -> Result<BondStats, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::BondStats(voucher))
        .ok_or(ContractError::InvalidAmount)
}

/// Issue #1175: Get bond utilization rate (percentage of bonds used).
pub fn get_bond_utilization_rate(env: Env, voucher: Address) -> Result<u32, ContractError> {
    let stats = get_bond_stats(env, voucher)?;

    if stats.total_bonded == 0 {
        return Ok(0);
    }

    let utilization_bps = (stats.total_used * 10_000) / stats.total_bonded;
    Ok(utilization_bps.min(10_000) as u32)
}
