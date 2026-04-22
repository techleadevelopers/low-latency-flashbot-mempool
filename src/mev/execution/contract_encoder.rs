use ethers::abi::{self, ParamType, Token};
use ethers::types::{Address, Bytes, U256};

const START_V2_FLASH_SWAP: [u8; 4] = [0x00, 0x00, 0x00, 0x00];
const EXECUTE_WITH_CAPITAL: [u8; 4] = [0x00, 0x00, 0x00, 0x00];

#[derive(Debug, Clone)]
pub struct EncodedSwapStep {
    pub router: Address,
    pub path: Vec<Address>,
    pub amount_in: U256,
    pub min_out: U256,
}

pub fn encode_start_v2_flash_swap(
    pair: Address,
    borrow_token: Address,
    borrow_amount: U256,
    min_profit: U256,
    profit_token: Address,
    steps: &[EncodedSwapStep],
) -> Bytes {
    let selector = selector(
        "startV2FlashSwap(address,address,uint256,uint256,address,(address,address[],uint256,uint256)[])",
    );
    encode_with_selector(
        selector,
        &[
            Token::Address(pair),
            Token::Address(borrow_token),
            Token::Uint(borrow_amount),
            Token::Uint(min_profit),
            Token::Address(profit_token),
            Token::Array(steps.iter().map(step_token).collect()),
        ],
    )
}

pub fn encode_execute_with_capital(
    input_token: Address,
    amount_in: U256,
    min_profit: U256,
    steps: &[EncodedSwapStep],
) -> Bytes {
    let selector = selector(
        "executeWithCapital(address,uint256,uint256,(address,address[],uint256,uint256)[])",
    );
    encode_with_selector(
        selector,
        &[
            Token::Address(input_token),
            Token::Uint(amount_in),
            Token::Uint(min_profit),
            Token::Array(steps.iter().map(step_token).collect()),
        ],
    )
}

pub fn decode_revert_reason(data: &[u8]) -> Option<String> {
    if data.len() < 4 || data[0..4] != [0x08, 0xc3, 0x79, 0xa0] {
        return None;
    }
    abi::decode(&[ParamType::String], &data[4..])
        .ok()
        .and_then(|tokens| match tokens.first() {
            Some(Token::String(reason)) => Some(reason.clone()),
            _ => None,
        })
}

fn step_token(step: &EncodedSwapStep) -> Token {
    Token::Tuple(vec![
        Token::Address(step.router),
        Token::Array(step.path.iter().copied().map(Token::Address).collect()),
        Token::Uint(step.amount_in),
        Token::Uint(step.min_out),
    ])
}

fn encode_with_selector(selector: [u8; 4], tokens: &[Token]) -> Bytes {
    let mut data = Vec::with_capacity(4 + 32 * tokens.len());
    data.extend_from_slice(&selector);
    data.extend(abi::encode(tokens));
    Bytes::from(data)
}

fn selector(signature: &str) -> [u8; 4] {
    let hash = ethers::utils::keccak256(signature.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
}

#[allow(dead_code)]
fn _selector_placeholders() -> ([u8; 4], [u8; 4]) {
    (START_V2_FLASH_SWAP, EXECUTE_WITH_CAPITAL)
}
