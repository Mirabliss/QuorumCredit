use crate::errors::ContractError;
use crate::types::{
    ConfidentialCommitment, DataKey, ZkProof, ZkProofRecord, PROOF_TYPE_LOAN_REQUEST,
    PROOF_TYPE_VOUCH,
};
use soroban_sdk::{Address, Bytes, BytesN, Env, Vec};
use sha3::{Digest, Sha3_256};

const MAX_CONFIDENTIAL_STAKE: i128 = 1_000_000_000;

fn hash_bytes(payload: &[u8]) -> [u8; 32] {
    let mut hasher = Sha3_256::new();
    hasher.update(payload);
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest[..]);
    out
}

fn hash_to_bytesn(env: &Env, payload: &[u8]) -> BytesN<32> {
    let digest = hash_bytes(payload);
    BytesN::from_array(env, &digest)
}

fn hash_address(addr: &Address) -> [u8; 32] {
    let addr_str = addr.to_string();
    let mut buf = [0u8; 64];
    let len = addr_str.len() as usize;
    addr_str.to_bytes().copy_into_slice(&mut buf[..len]);
    hash_bytes(&buf[..len])
}

fn hash_i128(value: i128) -> [u8; 32] {
    let mut payload = [0u8; 16];
    payload.copy_from_slice(&value.to_be_bytes());
    hash_bytes(&payload)
}

fn hash_bool(value: bool) -> [u8; 32] {
    hash_bytes(&[if value { 1u8 } else { 0u8 }])
}

fn bound_hash_bytes(env: &Env, proof_type: u32, voucher: &Address, borrower: &Address, token: &Address, stake_amount: i128) -> BytesN<32> {
    let _payload = [0u8; 0];
    let mut hasher = Sha3_256::new();
    hasher.update(&proof_type.to_be_bytes());
    hasher.update(&hash_address(voucher));
    hasher.update(&hash_address(borrower));
    hasher.update(&hash_address(token));
    hasher.update(&hash_i128(stake_amount));
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest[..]);
    BytesN::from_array(env, &out)
}

fn build_vouch_public_inputs(
    env: &Env,
    voucher: &Address,
    borrower: &Address,
    token: &Address,
    stake_amount: i128,
    balance_ok: bool,
    blacklisted: bool,
) -> Vec<BytesN<32>> {
    let mut inputs = Vec::new(env);
    inputs.push_back(bound_hash_bytes(env, PROOF_TYPE_VOUCH, voucher, borrower, token, stake_amount));
    inputs.push_back(hash_to_bytesn(env, &hash_i128(stake_amount)));
    inputs.push_back(hash_to_bytesn(env, &hash_bool(balance_ok)));
    inputs.push_back(hash_to_bytesn(env, &hash_bool(blacklisted)));
    inputs
}

fn build_loan_public_inputs(
    env: &Env,
    borrower: &Address,
    token: &Address,
    amount: i128,
    threshold: i128,
    eligibility_ok: bool,
    sufficient_vouches: bool,
) -> Vec<BytesN<32>> {
    let mut inputs = Vec::new(env);
    inputs.push_back(bound_hash_bytes(env, PROOF_TYPE_LOAN_REQUEST, borrower, &borrower, token, amount));
    inputs.push_back(hash_to_bytesn(env, &hash_i128(amount)));
    inputs.push_back(hash_to_bytesn(env, &hash_i128(threshold)));
    inputs.push_back(hash_to_bytesn(env, &hash_bool(eligibility_ok)));
    inputs.push_back(hash_to_bytesn(env, &hash_bool(sufficient_vouches)));
    inputs
}

fn proof_digest(env: &Env, proof: &ZkProof) -> BytesN<32> {
    let mut hasher = Sha3_256::new();
    hasher.update(&proof.proof_type.to_be_bytes());
    for input in proof.public_inputs.iter() {
        hasher.update(&input.to_array());
    }
    // NOTE: proof_bytes.len() is intentionally NOT hashed here because
    // proof_bytes IS the output of this function — including its length
    // would make create() and verify() produce different digests.
    let mut proof_bytes = [0u8; 32];
    proof_bytes.copy_from_slice(&hasher.finalize()[..]);
    BytesN::from_array(env, &proof_bytes)
}

pub fn create_vouch_proof(
    env: &Env,
    voucher: &Address,
    borrower: &Address,
    token: &Address,
    stake_amount: i128,
    balance_ok: bool,
    blacklisted: bool,
) -> ZkProof {
    let public_inputs = build_vouch_public_inputs(env, voucher, borrower, token, stake_amount, balance_ok, blacklisted);
    let digest = proof_digest(env, &ZkProof {
        proof_bytes: Bytes::new(env),
        public_inputs: public_inputs.clone(),
        proof_type: PROOF_TYPE_VOUCH,
    });
    ZkProof {
        proof_bytes: Bytes::from_array(env, &digest.to_array()),
        public_inputs,
        proof_type: PROOF_TYPE_VOUCH,
    }
}

pub fn create_loan_proof(
    env: &Env,
    borrower: &Address,
    token: &Address,
    amount: i128,
    threshold: i128,
    eligibility_ok: bool,
    sufficient_vouches: bool,
) -> ZkProof {
    let public_inputs = build_loan_public_inputs(env, borrower, token, amount, threshold, eligibility_ok, sufficient_vouches);
    let digest = proof_digest(env, &ZkProof {
        proof_bytes: Bytes::new(env),
        public_inputs: public_inputs.clone(),
        proof_type: PROOF_TYPE_LOAN_REQUEST,
    });
    ZkProof {
        proof_bytes: Bytes::from_array(env, &digest.to_array()),
        public_inputs,
        proof_type: PROOF_TYPE_LOAN_REQUEST,
    }
}

pub fn commit_amount(env: &Env, amount: i128, blinding: &[u8]) -> Result<ConfidentialCommitment, ContractError> {
    let mut hasher = Sha3_256::new();
    hasher.update(&amount.to_be_bytes());
    hasher.update(blinding);
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest[..]);
    Ok(ConfidentialCommitment {
        commitment: BytesN::from_array(env, &out),
    })
}

pub fn verify_vouch_proof(
    env: &Env,
    proof: &ZkProof,
    voucher: &Address,
    borrower: &Address,
    token: &Address,
    stake_amount: i128,
    balance_ok: bool,
    blacklisted: bool,
) -> Result<(), ContractError> {
    if proof.proof_type != PROOF_TYPE_VOUCH {
        return Err(ContractError::InvalidProof);
    }
    if proof.public_inputs.len() != 4 {
        return Err(ContractError::InvalidProof);
    }

    let expected_context = bound_hash_bytes(env, PROOF_TYPE_VOUCH, voucher, borrower, token, stake_amount);
    let expected_amount = hash_to_bytesn(env, &hash_i128(stake_amount));
    let expected_balance = hash_to_bytesn(env, &hash_bool(balance_ok));
    let expected_blacklist = hash_to_bytesn(env, &hash_bool(blacklisted));

    if proof.public_inputs.get(0).unwrap() != expected_context
        || proof.public_inputs.get(1).unwrap() != expected_amount
        || proof.public_inputs.get(2).unwrap() != expected_balance
        || proof.public_inputs.get(3).unwrap() != expected_blacklist
    {
        return Err(ContractError::InvalidProof);
    }

    if stake_amount <= 0 || stake_amount > MAX_CONFIDENTIAL_STAKE {
        return Err(ContractError::InvalidAmount);
    }

    let expected_proof_bytes = proof_digest(env, proof);
    if proof.proof_bytes != soroban_sdk::Bytes::from(&expected_proof_bytes) {
        return Err(ContractError::InvalidProof);
    }

    Ok(())
}

pub fn verify_loan_proof(
    env: &Env,
    proof: &ZkProof,
    borrower: &Address,
    token: &Address,
    amount: i128,
    threshold: i128,
    eligibility_ok: bool,
    sufficient_vouches: bool,
) -> Result<(), ContractError> {
    if proof.proof_type != PROOF_TYPE_LOAN_REQUEST {
        return Err(ContractError::InvalidProof);
    }
    if proof.public_inputs.len() != 5 {
        return Err(ContractError::InvalidProof);
    }

    let expected_context = bound_hash_bytes(env, PROOF_TYPE_LOAN_REQUEST, borrower, &borrower, token, amount);
    let expected_amount = hash_to_bytesn(env, &hash_i128(amount));
    let expected_threshold = hash_to_bytesn(env, &hash_i128(threshold));
    let expected_eligibility = hash_to_bytesn(env, &hash_bool(eligibility_ok));
    let expected_vouches = hash_to_bytesn(env, &hash_bool(sufficient_vouches));

    if proof.public_inputs.get(0).unwrap() != expected_context
        || proof.public_inputs.get(1).unwrap() != expected_amount
        || proof.public_inputs.get(2).unwrap() != expected_threshold
        || proof.public_inputs.get(3).unwrap() != expected_eligibility
        || proof.public_inputs.get(4).unwrap() != expected_vouches
    {
        return Err(ContractError::InvalidProof);
    }

    if amount <= 0 || amount > MAX_CONFIDENTIAL_STAKE {
        return Err(ContractError::InvalidAmount);
    }
    if threshold <= 0 || threshold > MAX_CONFIDENTIAL_STAKE {
        return Err(ContractError::InvalidAmount);
    }

    let expected_proof_bytes = proof_digest(env, proof);
    if proof.proof_bytes != soroban_sdk::Bytes::from(&expected_proof_bytes) {
        return Err(ContractError::InvalidProof);
    }

    Ok(())
}

pub fn record_proof(env: &Env, proof: &ZkProof, operation_type: u32, submitter: &Address) -> ZkProofRecord {
    let proof_id: u64 = env
        .storage()
        .instance()
        .get(&DataKey::ZkProofCounter)
        .unwrap_or(0u64)
        .checked_add(1)
        .expect("proof ID overflow");
    env.storage().instance().set(&DataKey::ZkProofCounter, &proof_id);
    ZkProofRecord {
        proof_id,
        proof: proof.clone(),
        operation_type,
        submitter: submitter.clone(),
        verified: true,
        submitted_at: env.ledger().timestamp(),
    }
}
