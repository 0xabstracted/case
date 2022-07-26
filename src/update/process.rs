use std::str::FromStr;

use anchor_client::solana_sdk::pubkey::Pubkey;
use anchor_lang::prelude::AccountMeta;
use anyhow::Result;
use console::style;
use tars::{
    accounts as nft_accounts, instruction as nft_instruction, TarsData,
};
use spl_associated_token_account::get_associated_token_address;

use crate::{
    cache::load_cache,
    tars::{get_tars_state, parse_config_price, TARS_ID},
    common::*,
    config::{
        data::{ConfigData, *},
        parser::get_config_data,
    },
    utils::{
        assert_correct_authority, check_spl_token, check_spl_token_account, spinner_with_style,
    },
};

pub struct UpdateArgs {
    pub keypair: Option<String>,
    pub rpc_url: Option<String>,
    pub cache: String,
    pub new_authority: Option<String>,
    pub config: String,
    pub tars: Option<String>,
}

pub fn process_update(args: UpdateArgs) -> Result<()> {
    let case_config = case_setup(args.keypair, args.rpc_url)?;
    let client = setup_client(&case_config)?;
    let config_data = get_config_data(&args.config)?;

    // the tars id specified takes precedence over the one from the cache
    let tars_id = match args.tars {
        Some(tars_id) => tars_id,
        None => {
            let cache = load_cache(&args.cache, false)?;
            cache.program.tars
        }
    };

    let tars_pubkey = match Pubkey::from_str(&tars_id) {
        Ok(tars_pubkey) => tars_pubkey,
        Err(_) => {
            let error = anyhow!("Failed to parse tars id: {}", tars_id);
            error!("{:?}", error);
            return Err(error);
        }
    };

    println!(
        "{} {}Loading tars",
        style("[1/2]").bold().dim(),
        LOOKING_GLASS_EMOJI
    );
    println!("{} {}", style("Tars ID:").bold(), tars_id);

    let pb = spinner_with_style();
    pb.set_message("Connecting...");

    let tars_state = get_tars_state(&case_config, &tars_pubkey)?;
    let tars_data =
        create_tars_data(&client, &config_data, &tars_state.data)?;

    pb.finish_with_message("Done");

    assert_correct_authority(
        &case_config.keypair.pubkey(),
        &tars_state.authority,
    )?;

    println!(
        "\n{} {}Updating configuration",
        style("[2/2]").bold().dim(),
        COMPUTER_EMOJI
    );

    let mut remaining_accounts: Vec<AccountMeta> = Vec::new();

    if config_data.spl_token.is_some() {
        if let Some(token) = config_data.spl_token {
            remaining_accounts.push(AccountMeta {
                pubkey: token,
                is_signer: false,
                is_writable: false,
            })
        }
    }

    let program = client.program(TARS_ID);

    let treasury_account = match config_data.spl_token {
        Some(spl_token) => {
            let spl_token_account_figured = if config_data.spl_token_account.is_some() {
                config_data.spl_token_account
            } else {
                Some(get_associated_token_address(&program.payer(), &spl_token))
            };

            if config_data.sol_treasury_account.is_some() {
                return Err(anyhow!("If spl-token-account or spl-token is set then sol-treasury-account cannot be set"));
            }

            // validates the mint address of the token accepted as payment
            check_spl_token(&program, &spl_token.to_string())?;

            if let Some(token_account) = spl_token_account_figured {
                // validates the spl token wallet to receive proceedings from SPL token payments
                check_spl_token_account(&program, &token_account.to_string())?;
                token_account
            } else {
                return Err(anyhow!(
                    "If spl-token is set, spl-token-account must also be set"
                ));
            }
        }
        None => match config_data.sol_treasury_account {
            Some(sol_treasury_account) => sol_treasury_account,
            None => case_config.keypair.pubkey(),
        },
    };

    let mut builder = program
        .request()
        .accounts(nft_accounts::UpdateTars {
            tars: tars_pubkey,
            authority: program.payer(),
            wallet: treasury_account,
        })
        .args(nft_instruction::UpdateTars {
            data: tars_data,
        });

    if !remaining_accounts.is_empty() {
        for account in remaining_accounts {
            builder = builder.accounts(account);
        }
    }

    let pb = spinner_with_style();
    pb.set_message("Sending update transaction...");

    let update_signature = builder.send()?;

    pb.finish_with_message(format!(
        "{} {}",
        style("Update signature:").bold(),
        update_signature
    ));

    if let Some(new_authority) = args.new_authority {
        let pb = spinner_with_style();
        pb.set_message("Sending update authority transaction...");

        let new_authority_pubkey = Pubkey::from_str(&new_authority)?;
        let builder = program
            .request()
            .accounts(nft_accounts::UpdateTars {
                tars: tars_pubkey,
                authority: program.payer(),
                wallet: treasury_account,
            })
            .args(nft_instruction::UpdateAuthority {
                new_authority: Some(new_authority_pubkey),
            });

        let authority_signature = builder.send()?;
        pb.finish_with_message(format!(
            "{} {}",
            style("Authority signature:").bold(),
            authority_signature
        ));
    }

    Ok(())
}

fn create_tars_data(
    client: &Client,
    config: &ConfigData,
    tars: &TarsData,
) -> Result<TarsData> {
    info!("{:?}", config.go_live_date);
    let go_live_date: Option<i64> = go_live_date_as_timestamp(&config.go_live_date)?;

    let end_settings = config.end_settings.as_ref().map(|s| s.to_tars_format());

    let whitelist_mint_settings = config
        .whitelist_mint_settings
        .as_ref()
        .map(|s| s.to_tars_format());

    let hidden_settings = config.hidden_settings.as_ref().map(|s| s.to_tars_format());

    let gatekeeper = config.gatekeeper.as_ref().map(|g| g.to_tars_format());

    let price = parse_config_price(client, config)?;

    let creators = config
        .creators
        .clone()
        .into_iter()
        .map(|c| c.to_tars_format())
        .collect::<Result<Vec<tars::Creator>>>()?;

    let data = TarsData {
        uuid: tars.uuid.clone(),
        price,
        symbol: config.symbol.clone(),
        seller_fee_basis_points: config.seller_fee_basis_points,
        max_supply: 0,
        is_mutable: config.is_mutable,
        retain_authority: config.retain_authority,
        go_live_date,
        end_settings,
        creators,
        whitelist_mint_settings,
        hidden_settings,
        items_available: config.number,
        gatekeeper,
    };
    Ok(data)
}
