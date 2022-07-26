use anchor_client::{solana_sdk::pubkey::Pubkey, Client, ClientError};
use anyhow::{anyhow, Result};
pub use tars::ID as TARS_ID;
use tars::{Tars, TarsData, WhitelistMintMode, WhitelistMintSettings};
use spl_token::id as token_program_id;

use crate::{
    config::{data::CaseConfig, price_as_lamports, ConfigData},
    setup::setup_client,
    utils::check_spl_token,
};

// To test a custom tars program, comment the tars::ID line
// above and use the following lines to declare the id to use:
//
//use solana_program::declare_id;
//declare_id!("<YOUR TARS ID>");
//pub use self::ID as TARS_ID;

#[derive(Debug)]
pub struct ConfigStatus {
    pub index: u32,
    pub on_chain: bool,
}

pub fn parse_config_price(client: &Client, config: &ConfigData) -> Result<u64> {
    let parsed_price = if let Some(spl_token) = config.spl_token {
        let token_program = client.program(token_program_id());
        let token_mint = check_spl_token(&token_program, &spl_token.to_string())?;

        match (config.price as u64).checked_mul(10u64.pow(token_mint.decimals.into())) {
            Some(price) => price,
            None => return Err(anyhow!("Price math overflow")),
        }
    } else {
        price_as_lamports(config.price)
    };

    Ok(parsed_price)
}

pub fn get_tars_state(
    case_config: &CaseConfig,
    tars_id: &Pubkey,
) -> Result<Tars> {
    let client = setup_client(case_config)?;
    let program = client.program(TARS_ID);

    program.account(*tars_id).map_err(|e| match e {
        ClientError::AccountNotFound => anyhow!("Tars does not exist!"),
        _ => anyhow!(
            "Failed to deserialize Tars account {}: {}",
            tars_id.to_string(),
            e
        ),
    })
}

pub fn get_tars_data(
    case_config: &CaseConfig,
    tars_id: &Pubkey,
) -> Result<TarsData> {
    let tars = get_tars_state(case_config, tars_id)?;
    Ok(tars.data)
}

pub fn print_tars_state(state: Tars) {
    println!("Authority {:?}", state.authority);
    println!("Wallet {:?}", state.wallet);
    println!("Token mint: {:?}", state.token_mint);
    println!("Items redeemed: {:?}", state.items_redeemed);
    print_tars_data(&state.data);
}

pub fn print_tars_data(data: &TarsData) {
    println!("Uuid: {:?}", data.uuid);
    println!("Price: {:?}", data.price);
    println!("Symbol: {:?}", data.symbol);
    println!(
        "Seller fee basis points: {:?}",
        data.seller_fee_basis_points
    );
    println!("Max supply: {:?}", data.max_supply);
    println!("Is mutable: {:?}", data.is_mutable);
    println!("Retain Authority: {:?}", data.retain_authority);
    println!("Go live date: {:?}", data.go_live_date);
    println!("Items available: {:?}", data.items_available);

    print_whitelist_mint_settings(&data.whitelist_mint_settings);
}

fn print_whitelist_mint_settings(settings: &Option<WhitelistMintSettings>) {
    if let Some(settings) = settings {
        match settings.mode {
            WhitelistMintMode::BurnEveryTime => println!("Mode: Burn every time"),
            WhitelistMintMode::NeverBurn => println!("Mode: Never burn"),
        }
        println!("Mint: {:?}", settings.mint);
        println!("Presale: {:?}", settings.presale);
        println!("Discount price: {:?}", settings.discount_price);
    } else {
        println!("No whitelist mint settings");
    }
}
