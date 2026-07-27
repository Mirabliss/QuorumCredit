/// Loan Tokenization Module (Issue #1185)
/// Enables creation of transferable tokens representing rights to interest payment streams from loans.
/// This allows for secondary market trading of loan interests and yield farming opportunities.

use crate::errors::ContractError;
use crate::types::{DataKey, LoanRecord};
use soroban_sdk::{contracttype, symbol_short, token, Address, Env, String, Symbol, Vec};

/// Represents a loan token that can be traded in secondary markets
#[derive(Clone, Debug)]
#[contracttype]
pub struct LoanToken {
    /// Unique identifier for the loan token
    pub token_id: u64,
    /// The loan this token is backed by
    pub loan_id: u64,
    /// Address of the original borrower
    pub borrower: Address,
    /// Total supply of tokens issued
    pub total_supply: i128,
    /// Contract address of the token contract
    pub token_contract: Address,
    /// Timestamp when token was created
    pub created_at: u64,
    /// Total interest distributed so far
    pub interest_distributed: i128,
}

/// Price tracking entry for historical analysis
#[derive(Clone, Debug)]
#[contracttype]
pub struct TokenPriceRecord {
    /// Token identifier
    pub token_id: u64,
    /// Price at this point in time
    pub price: i128,
    /// Total transaction volume
    pub volume: i128,
    /// Timestamp of price record
    pub timestamp: u64,
}

/// Interest distribution record
#[derive(Clone, Debug)]
#[contracttype]
pub struct InterestDistribution {
    /// Token identifier
    pub token_id: u64,
    /// Total interest distributed in this period
    pub interest_amount: i128,
    /// Number of token holders at distribution time
    pub token_holders: u32,
    /// Interest per token
    pub interest_per_token: i128,
    /// Timestamp of distribution
    pub timestamp: u64,
}

/// Secondary market order for trading loan tokens
#[derive(Clone, Debug)]
#[contracttype]
pub struct MarketOrder {
    /// Order identifier
    pub order_id: u64,
    /// Token being traded
    pub token_id: u64,
    /// Seller address
    pub seller: Address,
    /// Price per token
    pub price_per_token: i128,
    /// Amount of tokens for sale
    pub amount: i128,
    /// Total order value
    pub total_value: i128,
    /// Whether order is active
    pub is_active: bool,
    /// Creation timestamp
    pub created_at: u64,
}

const NEXT_TOKEN_ID_KEY: Symbol = symbol_short!("nxt_tok");
const LOAN_TOKENS_KEY: Symbol = symbol_short!("ltn_map");
const TOKEN_PRICE_HISTORY_KEY: Symbol = symbol_short!("prc_his");
const INTEREST_DISTRIBUTIONS_KEY: Symbol = symbol_short!("int_dis");
const MARKET_ORDERS_KEY: Symbol = symbol_short!("mkt_ord");
const TOKEN_HOLDERS_KEY: Symbol = symbol_short!("tok_hld");
const NEXT_ORDER_ID_KEY: Symbol = symbol_short!("nxt_ord");

/// Create a new loan token representing ownership of loan interest streams
pub fn tokenize_loan_interest(
    env: &Env,
    loan_id: u64,
    initial_supply: i128,
    token_contract: Address,
) -> Result<LoanToken, ContractError> {
    if initial_supply <= 0 {
        return Err(ContractError::InvalidInput);
    }

    let now = env.ledger().timestamp();

    // Get or initialize next token ID
    let next_id: u64 = env
        .storage()
        .instance()
        .get(&DataKey::Custom(NEXT_TOKEN_ID_KEY.into()))
        .unwrap_or(1u64);

    let loan_token = LoanToken {
        token_id: next_id,
        loan_id,
        borrower: Address::from_contract_id(&env, &env.current_contract_address()),
        total_supply: initial_supply,
        token_contract: token_contract.clone(),
        created_at: now,
        interest_distributed: 0,
    };

    // Store the loan token
    let mut tokens: Vec<LoanToken> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(LOAN_TOKENS_KEY.into()))
        .unwrap_or(Vec::new(env));
    tokens.push_back(loan_token.clone());
    env.storage()
        .persistent()
        .set(&DataKey::Custom(LOAN_TOKENS_KEY.into()), &tokens);

    // Increment next token ID
    env.storage()
        .instance()
        .set(&DataKey::Custom(NEXT_TOKEN_ID_KEY.into()), &(next_id + 1));

    Ok(loan_token)
}

/// Distribute interest to current token holders
pub fn distribute_interest_to_holders(
    env: &Env,
    token_id: u64,
    interest_amount: i128,
) -> Result<InterestDistribution, ContractError> {
    if interest_amount <= 0 {
        return Err(ContractError::InvalidInput);
    }

    let now = env.ledger().timestamp();

    // Get token holders count
    let holders: Vec<Address> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(Symbol::new(env, &format!("tok_hldr_{}", token_id))))
        .unwrap_or(Vec::new(env));

    let holder_count = holders.len() as u32;
    if holder_count == 0 {
        return Err(ContractError::InvalidInput);
    }

    let interest_per_token = interest_amount / (holder_count as i128);

    // Get total supply of token
    let tokens: Vec<LoanToken> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(LOAN_TOKENS_KEY.into()))
        .unwrap_or(Vec::new(env));

    let token_supply = tokens
        .iter()
        .find(|t| t.token_id == token_id)
        .map(|t| t.total_supply)
        .ok_or(ContractError::NotFound)?;

    let distribution = InterestDistribution {
        token_id,
        interest_amount,
        token_holders: holder_count,
        interest_per_token,
        timestamp: now,
    };

    // Track distribution history
    let mut distributions: Vec<InterestDistribution> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(INTEREST_DISTRIBUTIONS_KEY.into()))
        .unwrap_or(Vec::new(env));
    distributions.push_back(distribution.clone());
    env.storage()
        .persistent()
        .set(&DataKey::Custom(INTEREST_DISTRIBUTIONS_KEY.into()), &distributions);

    Ok(distribution)
}

/// Record price for historical tracking and market analysis
pub fn record_token_price(
    env: &Env,
    token_id: u64,
    price: i128,
    volume: i128,
) -> Result<TokenPriceRecord, ContractError> {
    if price < 0 || volume < 0 {
        return Err(ContractError::InvalidInput);
    }

    let now = env.ledger().timestamp();

    let price_record = TokenPriceRecord {
        token_id,
        price,
        volume,
        timestamp: now,
    };

    // Store price history
    let mut price_history: Vec<TokenPriceRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(TOKEN_PRICE_HISTORY_KEY.into()))
        .unwrap_or(Vec::new(env));
    price_history.push_back(price_record.clone());
    env.storage()
        .persistent()
        .set(&DataKey::Custom(TOKEN_PRICE_HISTORY_KEY.into()), &price_history);

    Ok(price_record)
}

/// Create a secondary market order to sell loan tokens
pub fn create_market_order(
    env: &Env,
    seller: Address,
    token_id: u64,
    price_per_token: i128,
    amount: i128,
) -> Result<MarketOrder, ContractError> {
    if price_per_token <= 0 || amount <= 0 {
        return Err(ContractError::InvalidInput);
    }

    seller.require_auth();

    let now = env.ledger().timestamp();

    // Get next order ID
    let next_order_id: u64 = env
        .storage()
        .instance()
        .get(&DataKey::Custom(NEXT_ORDER_ID_KEY.into()))
        .unwrap_or(1u64);

    let total_value = price_per_token.saturating_mul(amount);

    let order = MarketOrder {
        order_id: next_order_id,
        token_id,
        seller,
        price_per_token,
        amount,
        total_value,
        is_active: true,
        created_at: now,
    };

    // Store market order
    let mut orders: Vec<MarketOrder> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(MARKET_ORDERS_KEY.into()))
        .unwrap_or(Vec::new(env));
    orders.push_back(order.clone());
    env.storage()
        .persistent()
        .set(&DataKey::Custom(MARKET_ORDERS_KEY.into()), &orders);

    // Increment order ID
    env.storage()
        .instance()
        .set(&DataKey::Custom(NEXT_ORDER_ID_KEY.into()), &(next_order_id + 1));

    Ok(order)
}

/// Cancel a market order
pub fn cancel_market_order(env: &Env, order_id: u64) -> Result<(), ContractError> {
    let mut orders: Vec<MarketOrder> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(MARKET_ORDERS_KEY.into()))
        .unwrap_or(Vec::new(env));

    let mut found = false;
    for i in 0..orders.len() {
        if orders.get(i).unwrap().order_id == order_id {
            let mut order = orders.get(i).unwrap();
            order.is_active = false;
            orders.set(i, order);
            found = true;
            break;
        }
    }

    if !found {
        return Err(ContractError::NotFound);
    }

    env.storage()
        .persistent()
        .set(&DataKey::Custom(MARKET_ORDERS_KEY.into()), &orders);

    Ok(())
}

/// Get all active market orders for a token
pub fn get_active_market_orders(env: &Env, token_id: u64) -> Vec<MarketOrder> {
    let orders: Vec<MarketOrder> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(MARKET_ORDERS_KEY.into()))
        .unwrap_or(Vec::new(env));

    let mut active_orders = Vec::new(env);
    for order in orders.iter() {
        if order.token_id == token_id && order.is_active {
            active_orders.push_back(order);
        }
    }

    active_orders
}

/// Get price history for a token
pub fn get_token_price_history(env: &Env, token_id: u64, limit: u32) -> Vec<TokenPriceRecord> {
    let history: Vec<TokenPriceRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(TOKEN_PRICE_HISTORY_KEY.into()))
        .unwrap_or(Vec::new(env));

    let mut token_history = Vec::new(env);
    let start_idx = if history.len() > limit as usize {
        history.len() - (limit as usize)
    } else {
        0
    };

    for i in start_idx..history.len() {
        if history.get(i).unwrap().token_id == token_id {
            token_history.push_back(history.get(i).unwrap());
        }
    }

    token_history
}

/// Calculate average price from history
pub fn calculate_average_price(env: &Env, token_id: u64, periods: u32) -> Result<i128, ContractError> {
    let history = get_token_price_history(env, token_id, periods);

    if history.is_empty() {
        return Err(ContractError::NotFound);
    }

    let sum: i128 = history.iter().fold(0, |acc, record| {
        acc.saturating_add(record.price)
    });

    Ok(sum / (history.len() as i128))
}

/// Register a token holder for interest distribution
pub fn register_token_holder(env: &Env, token_id: u64, holder: Address) -> Result<(), ContractError> {
    let holder_key = Symbol::new(env, &format!("tok_hldr_{}", token_id));

    let mut holders: Vec<Address> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(holder_key.clone().into()))
        .unwrap_or(Vec::new(env));

    // Check if already registered
    if holders.iter().any(|h| h == &holder) {
        return Ok(());
    }

    holders.push_back(holder);
    env.storage()
        .persistent()
        .set(&DataKey::Custom(holder_key.into()), &holders);

    Ok(())
}

/// Get token by ID
pub fn get_loan_token(env: &Env, token_id: u64) -> Result<LoanToken, ContractError> {
    let tokens: Vec<LoanToken> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(LOAN_TOKENS_KEY.into()))
        .unwrap_or(Vec::new(env));

    tokens
        .iter()
        .find(|t| t.token_id == token_id)
        .ok_or(ContractError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_loan_token_creation() {
        // Basic structure verification
        let token = LoanToken {
            token_id: 1,
            loan_id: 100,
            borrower: Address::from_contract_id(&env, &env.current_contract_address()),
            total_supply: 1_000_000,
            token_contract: Address::from_contract_id(&env, &env.current_contract_address()),
            created_at: 0,
            interest_distributed: 0,
        };

        assert_eq!(token.token_id, 1);
        assert_eq!(token.loan_id, 100);
        assert_eq!(token.total_supply, 1_000_000);
    }
}
