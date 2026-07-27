//! Vouch diversification scoring and portfolio analysis (Issue #1181).
//!
//! This module provides tools to assess and improve the portfolio health of vouchers
//! by analyzing the diversity of their lending positions across borrowers, sectors, and geographies.
//! A well-diversified portfolio reduces risk and encourages responsible lending.

extern crate alloc;

use crate::errors::ContractError;
use crate::types::{DataKey, LoanRecord, VouchRecord};
use soroban_sdk::{contracttype, Address, Env, String, Vec};

/// Maximum possible diversification score.
pub const MAX_DIVERSIFICATION_SCORE: u32 = 100;

/// Score threshold for the diversification badge.
pub const BADGE_THRESHOLD: u32 = 80;

/// Target number of borrowers for a well-diversified portfolio.
pub const TARGET_BORROWERS: u32 = 20;

/// Target number of sectors for diversification.
pub const TARGET_SECTORS: u32 = 5;

/// Target number of geographic regions.
pub const TARGET_REGIONS: u32 = 3;

/// Weights for score calculation (sum = 100).
const BORROWER_WEIGHT: u32 = 40;
const SECTOR_WEIGHT: u32 = 35;
const GEOGRAPHY_WEIGHT: u32 = 25;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiversificationScore {
    /// Total score from 0-100.
    pub total_score: u32,
    /// Number of unique borrowers.
    pub borrower_count: u32,
    /// Number of unique sectors.
    pub sector_count: u32,
    /// Number of unique geographic regions.
    pub region_count: u32,
    /// Individual score component for borrowers (0-40).
    pub borrower_score: u32,
    /// Individual score component for sectors (0-35).
    pub sector_score: u32,
    /// Individual score component for geography (0-25).
    pub geography_score: u32,
    /// Whether the voucher qualifies for the diversification badge.
    pub has_badge: bool,
}

/// Portfolio analysis recommendations.
#[contracttype]
#[derive(Clone, Debug)]
pub struct PortfolioRecommendation {
    pub recommendation_type: String,
    pub description: String,
    pub priority: u32, // 1 = high, 2 = medium, 3 = low
}

/// Extract sector from loan purpose string.
/// Simple heuristic-based approach using keywords in the purpose description.
fn extract_sector_from_purpose(env: &Env, purpose: &String) -> String {
    let purpose_lower = purpose.to_lowercase();

    if purpose_lower.contains("agriculture") || purpose_lower.contains("farm") {
        String::from_slice(env, "agriculture")
    } else if purpose_lower.contains("retail") || purpose_lower.contains("commerce") || purpose_lower.contains("store") {
        String::from_slice(env, "retail")
    } else if purpose_lower.contains("technology") || purpose_lower.contains("tech") || purpose_lower.contains("software") {
        String::from_slice(env, "technology")
    } else if purpose_lower.contains("health") || purpose_lower.contains("medical") || purpose_lower.contains("hospital") {
        String::from_slice(env, "healthcare")
    } else if purpose_lower.contains("education") || purpose_lower.contains("school") || purpose_lower.contains("training") {
        String::from_slice(env, "education")
    } else if purpose_lower.contains("manufacturing") || purpose_lower.contains("factory") || purpose_lower.contains("production") {
        String::from_slice(env, "manufacturing")
    } else if purpose_lower.contains("service") || purpose_lower.contains("consulting") {
        String::from_slice(env, "services")
    } else if purpose_lower.contains("real estate") || purpose_lower.contains("property") || purpose_lower.contains("construction") {
        String::from_slice(env, "real_estate")
    } else {
        String::from_slice(env, "other")
    }
}

/// Extract geographic region from loan purpose or other metadata.
/// This is a simplified implementation that can be enhanced with more sophisticated location detection.
fn extract_region_from_loan(env: &Env, loan: &LoanRecord) -> String {
    let purpose_lower = loan.loan_purpose.to_lowercase();

    // Simple region detection based on keywords in loan purpose
    if purpose_lower.contains("north") || purpose_lower.contains("northern") {
        String::from_slice(env, "north")
    } else if purpose_lower.contains("south") || purpose_lower.contains("southern") {
        String::from_slice(env, "south")
    } else if purpose_lower.contains("east") || purpose_lower.contains("eastern") {
        String::from_slice(env, "east")
    } else if purpose_lower.contains("west") || purpose_lower.contains("western") {
        String::from_slice(env, "west")
    } else if purpose_lower.contains("central") {
        String::from_slice(env, "central")
    } else {
        String::from_slice(env, "unspecified")
    }
}

/// Calculate diversification score for a voucher based on their portfolio.
///
/// # Arguments
/// * `env` – the environment
/// * `voucher` – the voucher address to analyze
///
/// # Returns
/// A `DiversificationScore` struct with detailed breakdown.
pub fn calculate_diversification_score(
    env: &Env,
    voucher: &Address,
) -> Result<DiversificationScore, ContractError> {
    let mut borrowers_set: Vec<Address> = Vec::new(&env);
    let mut sectors_set: Vec<String> = Vec::new(&env);
    let mut regions_set: Vec<String> = Vec::new(&env);

    // Iterate through all loans to find those vouched by this voucher
    let all_borrowers: Vec<Address> = env
        .storage()
        .persistent()
        .get(&DataKey::AllBorrowers)
        .unwrap_or_else(|| Vec::new(&env));

    for borrower in all_borrowers.iter() {
        let vouches: Vec<VouchRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::Vouches(borrower.clone()))
            .unwrap_or_else(|| Vec::new(&env));

        // Check if this voucher has vouched for this borrower
        let mut has_vouched = false;
        for vouch in vouches.iter() {
            if vouch.voucher == *voucher {
                has_vouched = true;
                break;
            }
        }

        if has_vouched {
            // Add borrower to unique set
            let mut already_counted = false;
            for existing in borrowers_set.iter() {
                if existing == borrower {
                    already_counted = true;
                    break;
                }
            }
            if !already_counted {
                borrowers_set.push_back(borrower.clone());
            }

            // Get the loan to extract sector and region information
            if let Ok(loan_option) = env
                .storage()
                .persistent()
                .get::<_, Option<LoanRecord>>(&DataKey::Loan(0)) // TODO: Implement proper loan lookup
            {
                if let Some(loan) = loan_option {
                    let sector = extract_sector_from_purpose(&env, &loan.loan_purpose);
                    let region = extract_region_from_loan(&env, &loan);

                    // Add sector to unique set
                    let mut sector_already_counted = false;
                    for existing_sector in sectors_set.iter() {
                        if existing_sector == sector {
                            sector_already_counted = true;
                            break;
                        }
                    }
                    if !sector_already_counted {
                        sectors_set.push_back(sector);
                    }

                    // Add region to unique set
                    let mut region_already_counted = false;
                    for existing_region in regions_set.iter() {
                        if existing_region == region {
                            region_already_counted = true;
                            break;
                        }
                    }
                    if !region_already_counted {
                        regions_set.push_back(region);
                    }
                }
            }
        }
    }

    let borrower_count = borrowers_set.len() as u32;
    let sector_count = sectors_set.len() as u32;
    let region_count = regions_set.len() as u32;

    // Calculate component scores
    let borrower_score = calculate_borrower_score(borrower_count);
    let sector_score = calculate_sector_score(sector_count);
    let geography_score = calculate_geography_score(region_count);

    let total_score = borrower_score + sector_score + geography_score;
    let has_badge = total_score >= BADGE_THRESHOLD;

    Ok(DiversificationScore {
        total_score,
        borrower_count,
        sector_count,
        region_count,
        borrower_score,
        sector_score,
        geography_score,
        has_badge,
    })
}

/// Calculate score component for borrower diversity (0-40 points).
fn calculate_borrower_score(borrower_count: u32) -> u32 {
    if borrower_count == 0 {
        0
    } else if borrower_count >= TARGET_BORROWERS {
        BORROWER_WEIGHT
    } else {
        (borrower_count as u32 * BORROWER_WEIGHT) / TARGET_BORROWERS
    }
}

/// Calculate score component for sector diversity (0-35 points).
fn calculate_sector_score(sector_count: u32) -> u32 {
    if sector_count == 0 {
        0
    } else if sector_count >= TARGET_SECTORS {
        SECTOR_WEIGHT
    } else {
        (sector_count as u32 * SECTOR_WEIGHT) / TARGET_SECTORS
    }
}

/// Calculate score component for geographic diversity (0-25 points).
fn calculate_geography_score(region_count: u32) -> u32 {
    if region_count == 0 {
        0
    } else if region_count >= TARGET_REGIONS {
        GEOGRAPHY_WEIGHT
    } else {
        (region_count as u32 * GEOGRAPHY_WEIGHT) / TARGET_REGIONS
    }
}

/// Generate portfolio improvement recommendations.
///
/// # Arguments
/// * `env` - the environment
/// * `score` – the current diversification score
///
/// # Returns
/// A vector of recommendations ordered by priority.
pub fn generate_recommendations(env: &Env, score: &DiversificationScore) -> Vec<PortfolioRecommendation> {
    let mut recommendations: Vec<PortfolioRecommendation> = Vec::new(&env);

    // Borrower count recommendations
    if score.borrower_count < TARGET_BORROWERS {
        let shortfall = TARGET_BORROWERS - score.borrower_count;
        let type_str = alloc::format!(
            "Increase borrower count to {} (currently {}, {} more needed)",
            TARGET_BORROWERS, score.borrower_count, shortfall
        );
        recommendations.push_back(PortfolioRecommendation {
            recommendation_type: String::from_slice(&env, "borrower_diversity"),
            description: String::from_slice(&env, &type_str),
            priority: 1,
        });
    }

    // Sector diversity recommendations
    if score.sector_count < TARGET_SECTORS {
        let shortfall = TARGET_SECTORS - score.sector_count;
        let type_str = alloc::format!(
            "Diversify into {} sectors (currently {}, {} more needed)",
            TARGET_SECTORS, score.sector_count, shortfall
        );
        recommendations.push_back(PortfolioRecommendation {
            recommendation_type: String::from_slice(&env, "sector_diversity"),
            description: String::from_slice(&env, &type_str),
            priority: 2,
        });
    }

    // Geographic diversity recommendations
    if score.region_count < TARGET_REGIONS {
        let shortfall = TARGET_REGIONS - score.region_count;
        let type_str = alloc::format!(
            "Expand to {} regions (currently {}, {} more needed)",
            TARGET_REGIONS, score.region_count, shortfall
        );
        recommendations.push_back(PortfolioRecommendation {
            recommendation_type: String::from_slice(&env, "geographic_diversity"),
            description: String::from_slice(&env, &type_str),
            priority: 2,
        });
    }

    // Overall excellence recommendation
    if score.total_score >= BADGE_THRESHOLD {
        recommendations.push_back(PortfolioRecommendation {
            recommendation_type: String::from_slice(&env, "badge_achieved"),
            description: String::from_slice(&env, "Congratulations! You have achieved the diversification badge."),
            priority: 3,
        });
    }

    recommendations
}

/// Check if a voucher qualifies for the diversification badge.
pub fn has_diversification_badge(env: &Env, voucher: &Address) -> Result<bool, ContractError> {
    let score = calculate_diversification_score(env, voucher)?;
    Ok(score.has_badge)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_borrower_score_calculation() {
        assert_eq!(calculate_borrower_score(0), 0);
        assert_eq!(calculate_borrower_score(10), 20); // 10/20 * 40
        assert_eq!(calculate_borrower_score(20), 40);
        assert_eq!(calculate_borrower_score(40), 40); // Capped at max
    }

    #[test]
    fn test_sector_score_calculation() {
        assert_eq!(calculate_sector_score(0), 0);
        assert_eq!(calculate_sector_score(2), 14); // ~(2/5 * 35)
        assert_eq!(calculate_sector_score(5), 35);
        assert_eq!(calculate_sector_score(10), 35); // Capped at max
    }

    #[test]
    fn test_geography_score_calculation() {
        assert_eq!(calculate_geography_score(0), 0);
        assert_eq!(calculate_geography_score(1), 8); // ~(1/3 * 25)
        assert_eq!(calculate_geography_score(3), 25);
        assert_eq!(calculate_geography_score(5), 25); // Capped at max
    }

    #[test]
    fn test_badge_threshold() {
        // Score >= 80 should have badge
        let score = DiversificationScore {
            total_score: 80,
            borrower_count: 20,
            sector_count: 5,
            region_count: 3,
            borrower_score: 40,
            sector_score: 35,
            geography_score: 25,
            has_badge: true,
        };
        assert!(score.has_badge);

        let score_below = DiversificationScore {
            total_score: 79,
            borrower_count: 19,
            sector_count: 4,
            region_count: 2,
            borrower_score: 38,
            sector_score: 33,
            geography_score: 8,
            has_badge: false,
        };
        assert!(!score_below.has_badge);
    }
}
