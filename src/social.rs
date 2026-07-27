/// Issue #1176: Social Features for Borrower Network
///
/// This module implements social and community features to foster connection
/// among borrowers and increase retention. Features include:
/// - Borrower profiles with bios and sector information
/// - Peer discovery based on similar characteristics
/// - Success stories and testimonials
/// - Retention metrics and engagement tracking

use soroban_sdk::{Address, Env, String as SorobanString, Vec};
use crate::types::{
    BorrowerProfile, SuccessStory, RetentionMetrics, DataKey, ContractError,
};

// ── Borrower Profile Management ──────────────────────────────────────────

/// Create or update a borrower profile (Issue #1176).
///
/// Allows borrowers to set up their public profile with bio and sector information
/// for community discovery and networking.
pub fn set_borrower_profile(
    env: &Env,
    borrower: Address,
    bio: SorobanString,
    sector: Option<SorobanString>,
    region: Option<SorobanString>,
) -> Result<(), ContractError> {
    // Validate bio length (max 500 characters)
    if bio.len() > 500 {
        return Err(ContractError::InvalidInput);
    }

    let current_timestamp = env.ledger().timestamp();

    let profile = if let Some(existing) = env
        .storage()
        .persistent()
        .get::<DataKey, BorrowerProfile>(&DataKey::BorrowerProfile(borrower.clone()))
    {
        // Update existing profile
        BorrowerProfile {
            borrower: borrower.clone(),
            bio,
            created_at: existing.created_at,
            updated_at: current_timestamp,
            sector,
            region,
            success_story_consent: existing.success_story_consent,
        }
    } else {
        // Create new profile
        BorrowerProfile {
            borrower: borrower.clone(),
            bio,
            created_at: current_timestamp,
            updated_at: current_timestamp,
            sector,
            region,
            success_story_consent: false,
        }
    };

    env.storage()
        .persistent()
        .set::<DataKey, BorrowerProfile>(&DataKey::BorrowerProfile(borrower), &profile);

    Ok(())
}

/// Get a borrower's profile (Issue #1176).
pub fn get_borrower_profile(env: Env, borrower: Address) -> Result<BorrowerProfile, ContractError> {
    env.storage()
        .persistent()
        .get::<DataKey, BorrowerProfile>(&DataKey::BorrowerProfile(borrower))
        .ok_or(ContractError::NotFound)
}

/// Update success story consent for a borrower profile (Issue #1176).
pub fn set_success_story_consent(
    env: &Env,
    borrower: Address,
    consent: bool,
) -> Result<(), ContractError> {
    let key = DataKey::BorrowerProfile(borrower.clone());

    let mut profile = env
        .storage()
        .persistent()
        .get::<DataKey, BorrowerProfile>(&key)
        .ok_or(ContractError::NotFound)?;

    profile.success_story_consent = consent;
    profile.updated_at = env.ledger().timestamp();

    env.storage()
        .persistent()
        .set::<DataKey, BorrowerProfile>(&key, &profile);

    Ok(())
}

// ── Success Stories ──────────────────────────────────────────────────────

/// Submit a success story (Issue #1176).
///
/// Allows borrowers to share their experience and testimonial.
/// Story is created in unpublished state and requires explicit publication.
pub fn submit_success_story(
    env: &Env,
    borrower: Address,
    title: SorobanString,
    content: SorobanString,
) -> Result<u64, ContractError> {
    // Validate title and content lengths
    if title.len() > 100 || content.len() > 2000 {
        return Err(ContractError::InvalidInput);
    }

    // Get next story ID
    let counter_key = DataKey::SuccessStoryIdCounter;
    let mut story_id: u64 = env
        .storage()
        .persistent()
        .get::<DataKey, u64>(&counter_key)
        .unwrap_or(0);

    story_id = story_id.checked_add(1).ok_or(ContractError::ArithmeticError)?;

    let story = SuccessStory {
        borrower: borrower.clone(),
        title,
        content,
        submitted_at: env.ledger().timestamp(),
        story_id,
        is_published: false,
    };

    // Store the story
    env.storage()
        .persistent()
        .set::<DataKey, SuccessStory>(&DataKey::SuccessStory(story_id), &story);

    // Add story ID to borrower's story list
    let stories_key = DataKey::BorrowerSuccessStories(borrower);
    let mut stories: Vec<u64> = env
        .storage()
        .persistent()
        .get::<DataKey, Vec<u64>>(&stories_key)
        .unwrap_or(Vec::new(&env));

    stories.push_back(story_id);
    env.storage()
        .persistent()
        .set::<DataKey, Vec<u64>>(&stories_key, &stories);

    // Update counter
    env.storage()
        .persistent()
        .set::<DataKey, u64>(&counter_key, &story_id);

    Ok(story_id)
}

/// Publish a success story (Issue #1176).
///
/// Only the borrower who submitted the story can publish it.
pub fn publish_success_story(
    env: &Env,
    borrower: Address,
    story_id: u64,
) -> Result<(), ContractError> {
    let key = DataKey::SuccessStory(story_id);

    let mut story = env
        .storage()
        .persistent()
        .get::<DataKey, SuccessStory>(&key)
        .ok_or(ContractError::NotFound)?;

    // Only the borrower who submitted can publish
    if story.borrower != borrower {
        return Err(ContractError::Unauthorized);
    }

    story.is_published = true;
    env.storage()
        .persistent()
        .set::<DataKey, SuccessStory>(&key, &story);

    Ok(())
}

/// Get a success story (Issue #1176).
pub fn get_success_story(env: Env, story_id: u64) -> Result<SuccessStory, ContractError> {
    env.storage()
        .persistent()
        .get::<DataKey, SuccessStory>(&DataKey::SuccessStory(story_id))
        .ok_or(ContractError::NotFound)
}

/// Get all success stories for a borrower (Issue #1176).
pub fn get_borrower_success_stories(
    env: Env,
    borrower: Address,
) -> Result<Vec<SuccessStory>, ContractError> {
    let stories_key = DataKey::BorrowerSuccessStories(borrower);
    let story_ids: Vec<u64> = env
        .storage()
        .persistent()
        .get::<DataKey, Vec<u64>>(&stories_key)
        .unwrap_or(Vec::new(&env));

    let mut stories = Vec::new(&env);
    for story_id in story_ids.iter() {
        if let Ok(story) = get_success_story(env.clone(), story_id) {
            stories.push_back(story);
        }
    }

    Ok(stories)
}

// ── Retention Metrics ────────────────────────────────────────────────────

/// Update retention metrics for a borrower (Issue #1176).
///
/// Called when a borrower's loan activity changes to track engagement metrics.
pub fn update_retention_metrics(
    env: &Env,
    borrower: Address,
    total_loans: u32,
    successful_repayments: u32,
    defaults: u32,
    distinct_vouchers_count: u32,
    first_loan_timestamp: u64,
    last_loan_timestamp: u64,
) -> Result<(), ContractError> {
    let current_time = env.ledger().timestamp();
    let platform_tenure = current_time
        .checked_sub(first_loan_timestamp)
        .unwrap_or(0);

    // Calculate average loan interval
    let average_loan_interval = if total_loans > 1 {
        platform_tenure / (total_loans as u64 - 1)
    } else {
        0
    };

    let metrics = RetentionMetrics {
        borrower: borrower.clone(),
        total_loans,
        successful_repayments,
        defaults,
        distinct_vouchers_count,
        first_loan_timestamp,
        last_loan_timestamp,
        average_loan_interval,
        platform_tenure,
    };

    env.storage()
        .persistent()
        .set::<DataKey, RetentionMetrics>(&DataKey::BorrowerRetentionMetrics(borrower), &metrics);

    Ok(())
}

/// Get retention metrics for a borrower (Issue #1176).
pub fn get_retention_metrics(
    env: Env,
    borrower: Address,
) -> Result<RetentionMetrics, ContractError> {
    env.storage()
        .persistent()
        .get::<DataKey, RetentionMetrics>(&DataKey::BorrowerRetentionMetrics(borrower))
        .ok_or(ContractError::NotFound)
}

/// Find similar borrowers for peer discovery (Issue #1176).
///
/// Returns borrowers with similar sector/region characteristics.
/// This is a basic implementation; full implementation would involve
/// more sophisticated matching algorithms.
pub fn find_similar_borrowers(
    env: Env,
    borrower: Address,
    limit: u32,
) -> Result<Vec<BorrowerProfile>, ContractError> {
    let target_profile = get_borrower_profile(env.clone(), borrower.clone())?;

    // In a production system, this would query an indexer or use more sophisticated
    // matching. For now, return empty as this requires external data source.
    // The actual implementation would filter borrowers by sector and region.
    let results: Vec<BorrowerProfile> = Vec::new(&env);

    Ok(results)
}

/// Calculate engagement score for a borrower (Issue #1176).
///
/// Returns a score (0-100) based on retention metrics and activity.
pub fn calculate_engagement_score(
    env: Env,
    borrower: Address,
) -> Result<u32, ContractError> {
    let metrics = get_retention_metrics(env, borrower)?;

    // Calculate engagement score based on:
    // - Loan activity (total loans): 0-25 points
    // - Repayment success rate: 0-35 points
    // - Platform tenure: 0-20 points
    // - Voucher diversity: 0-20 points

    let mut score: u32 = 0;

    // Loan activity: max 5 loans = 25 points
    let loan_score = std::cmp::min(25, (metrics.total_loans as u32 * 5));
    score = score.saturating_add(loan_score);

    // Repayment success rate
    if metrics.total_loans > 0 {
        let success_rate = (metrics.successful_repayments as u32 * 100) / metrics.total_loans as u32;
        let success_score = (success_rate * 35) / 100;
        score = score.saturating_add(success_score);
    }

    // Platform tenure: 2+ years = 20 points
    let tenure_score = if metrics.platform_tenure >= (2 * 365 * 24 * 60 * 60) {
        20
    } else if metrics.platform_tenure >= (365 * 24 * 60 * 60) {
        10
    } else {
        0
    };
    score = score.saturating_add(tenure_score);

    // Voucher diversity: max 10 distinct = 20 points
    let diversity_score = std::cmp::min(20, (metrics.distinct_vouchers_count as u32 * 2));
    score = score.saturating_add(diversity_score);

    Ok(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_borrower_profile() {
        // Tests will be added in the test suite
    }

    #[test]
    fn test_submit_success_story() {
        // Tests will be added in the test suite
    }

    #[test]
    fn test_retention_metrics() {
        // Tests will be added in the test suite
    }
}
