use std::{
    collections::HashSet,
    fmt::Write as _,
    str::FromStr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use anchor_client::solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};
use anyhow::Result;
use console::style;
use spl_associated_token_account::get_associated_token_address;

use crate::{
    cache::*,
    tars::{get_tars_state, TARS_ID},
    common::*,
    config::parser::get_config_data,
    deploy::{
        create_and_set_collection, create_tars_data, errors::*, generate_config_lines,
        initialize_tars, upload_config_lines,
    },
    setup::{setup_client, case_setup},
    utils::*,
    validate::parser::{check_name, check_seller_fee_basis_points, check_symbol, check_url},
};

pub struct DeployArgs {
    pub config: String,
    pub cache: String,
    pub keypair: Option<String>,
    pub rpc_url: Option<String>,
    pub interrupted: Arc<AtomicBool>,
}

pub async fn process_deploy(args: DeployArgs) -> Result<()> {
    // loads the cache file (this needs to have been created by
    // the upload command)
    let mut cache = load_cache(&args.cache, false)?;

    if cache.items.is_empty() {
        println!(
            "{}",
            style("No cache items found - run 'upload' to create the cache file first.")
                .red()
                .bold()
        );

        // nothing else to do, just tell that the cache file was not found (or empty)
        return Err(CacheError::CacheFileNotFound(args.cache).into());
    }

    // checks that all metadata information are present and have the
    // correct length

    for (index, item) in &cache.items.0 {
        if item.name.is_empty() {
            return Err(DeployError::MissingName(index.to_string()).into());
        } else {
            check_name(&item.name)?;
        }

        if item.metadata_link.is_empty() {
            return Err(DeployError::MissingMetadataLink(index.to_string()).into());
        } else {
            check_url(&item.metadata_link)?;
        }
    }

    let case_config = Arc::new(case_setup(args.keypair, args.rpc_url)?);
    let client = setup_client(&case_config)?;
    let config_data = get_config_data(&args.config)?;

    let tars_address = &cache.program.tars;

    // checks the tars data

    let num_items = config_data.number;
    let hidden = config_data.hidden_settings.is_some();
    let collection_in_cache = cache.items.get("-1").is_some();
    let mut item_redeemed = false;

    let cache_items_sans_collection = (cache.items.len() - collection_in_cache as usize) as u64;

    if num_items != cache_items_sans_collection {
        return Err(anyhow!(
            "Number of items ({}) do not match cache items ({}). 
            Item number in the config should only include asset files, not the collection file.",
            num_items,
            cache_items_sans_collection
        ));
    } else {
        check_symbol(&config_data.symbol)?;
        check_seller_fee_basis_points(config_data.seller_fee_basis_points)?;
    }

    let total_steps = 2 + (collection_in_cache as u8) - (hidden as u8);

    let tars_pubkey = if tars_address.is_empty() {
        println!(
            "{} {}Creating tars",
            style(format!("[1/{}]", total_steps)).bold().dim(),
            TARS_EMOJI
        );
        info!("Tars address is empty, creating new tars...");

        let spinner = spinner_with_style();
        spinner.set_message("Creating tars...");

        let tars_keypair = Keypair::new();
        let tars_pubkey = tars_keypair.pubkey();

        let uuid = DEFAULT_UUID.to_string();
        let tars_data = create_tars_data(&client, &config_data, uuid)?;
        let program = client.program(TARS_ID);

        let treasury_wallet = match config_data.spl_token {
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

        // all good, let's create the tars

        let sig = initialize_tars(
            &config_data,
            &tars_keypair,
            tars_data,
            treasury_wallet,
            program,
        )?;
        info!("Tars initialized with sig: {}", sig);
        info!(
            "Tars created with address: {}",
            &tars_pubkey.to_string()
        );

        cache.program = CacheProgram::new_from_cm(&tars_pubkey);
        cache.sync_file()?;

        spinner.finish_and_clear();

        tars_pubkey
    } else {
        println!(
            "{} {}Loading tars",
            style(format!("[1/{}]", total_steps)).bold().dim(),
            TARS_EMOJI
        );

        let tars_pubkey = match Pubkey::from_str(tars_address) {
            Ok(pubkey) => pubkey,
            Err(_err) => {
                error!(
                    "Invalid tars address in cache file: {}!",
                    tars_address
                );
                return Err(CacheError::InvalidTarsAddress(
                    tars_address.to_string(),
                )
                .into());
            }
        };

        match get_tars_state(&Arc::clone(&case_config), &tars_pubkey) {
            Ok(tars_state) => {
                if tars_state.items_redeemed > 0 {
                    item_redeemed = true;
                }
            }
            Err(_) => {
                return Err(anyhow!("Tars from cache does't exist on chain!"));
            }
        }

        tars_pubkey
    };

    println!("{} {}", style("Tars ID:").bold(), tars_pubkey);

    if !hidden {
        println!(
            "\n{} {}Writing config lines",
            style(format!("[2/{}]", total_steps)).bold().dim(),
            PAPER_EMOJI
        );

        let config_lines = generate_config_lines(num_items, &cache.items)?;

        if config_lines.is_empty() {
            println!("\nAll config lines deployed.");
        } else {
            // clear the interruption handler value ahead of the upload
            args.interrupted.store(false, Ordering::SeqCst);

            let errors = upload_config_lines(
                Arc::clone(&case_config),
                tars_pubkey,
                &mut cache,
                config_lines,
                args.interrupted,
            )
            .await?;

            if !errors.is_empty() {
                let mut message = String::new();
                write!(
                    message,
                    "Failed to deploy all config lines, {0} error(s) occurred:",
                    errors.len()
                )?;

                let mut unique = HashSet::new();

                for err in errors {
                    unique.insert(err.to_string());
                }

                for u in unique {
                    message.push_str(&style("\n=> ").dim().to_string());
                    message.push_str(&u);
                }

                return Err(DeployError::AddConfigLineFailed(message).into());
            }
        }
    } else {
        println!("\nTars with hidden settings deployed.");
    }

    if let Some(collection_item) = cache.items.get_mut("-1") {
        println!(
            "\n{} {}Creating and setting the collection NFT for tars",
            style(format!("[3/{}]", total_steps)).bold().dim(),
            COLLECTION_EMOJI
        );

        if item_redeemed {
            println!("\nAn item has already been minted and thus cannot modify the tars collection. Skipping...");
        } else if collection_item.on_chain {
            println!("\nCollection mint already deployed.");
        } else {
            let pb = spinner_with_style();
            pb.set_message("Sending create and set collection NFT transaction...");

            let (_, collection_mint) =
                create_and_set_collection(client, tars_pubkey, &mut cache, config_data)?;

            pb.finish_and_clear();
            println!(
                "{} {}",
                style("Collection mint ID:").bold(),
                collection_mint
            );
        }
    }

    Ok(())
}
