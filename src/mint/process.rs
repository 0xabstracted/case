use std::{str::FromStr, sync::Arc};

use anchor_client::{
    solana_sdk::{
        program_pack::Pack,
        pubkey::Pubkey,
        signature::{Keypair, Signature, Signer},
        system_instruction, system_program, sysvar,
    },
    Client,
};
use anchor_lang::prelude::AccountMeta;
use anyhow::Result;
use chrono::Utc;
use console::style;
use tars::{
    accounts as nft_accounts, instruction as nft_instruction, TarsError, Tars,
    CollectionPDA, EndSettingType, WhitelistMintMode,
};
use mpl_token_metadata::pda::find_collection_authority_account;
use solana_client::rpc_response::Response;
use spl_associated_token_account::{create_associated_token_account, get_associated_token_address};
use spl_token::{
    instruction::{initialize_mint, mint_to},
    state::Account,
    ID as TOKEN_PROGRAM_ID,
};

use crate::{
    cache::load_cache,
    tars::{TARS_ID, *},
    common::*,
    config::Cluster,
    pdas::*,
    utils::*,
};

pub struct MintArgs {
    pub keypair: Option<String>,
    pub rpc_url: Option<String>,
    pub cache: String,
    pub number: Option<u64>,
    pub tars: Option<String>,
}

pub fn process_mint(args: MintArgs) -> Result<()> {
    let case_config = case_setup(args.keypair, args.rpc_url)?;
    let client = Arc::new(setup_client(&case_config)?);

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

    let tars_state = Arc::new(get_tars_state(&case_config, &tars_pubkey)?);

    let collection_pda_info =
        Arc::new(get_collection_pda(&tars_pubkey, &client.program(TARS_ID)).ok());

    pb.finish_with_message("Done");

    println!(
        "\n{} {}Minting from tars",
        style("[2/2]").bold().dim(),
        TARS_EMOJI
    );

    let number = args.number.unwrap_or(1);
    let available = tars_state.data.items_available - tars_state.items_redeemed;

    if number > available || number == 0 {
        let error = anyhow!("{} item(s) available, requested {}", available, number);
        error!("{:?}", error);
        return Err(error);
    }

    info!("Minting NFT from tars: {}", &tars_id);
    info!("Tars program id: {:?}", TARS_ID);

    if number == 1 {
        let pb = spinner_with_style();
        pb.set_message(format!(
            "{} item(s) remaining",
            tars_state.data.items_available - tars_state.items_redeemed
        ));

        let result = match mint(
            Arc::clone(&client),
            tars_pubkey,
            Arc::clone(&tars_state),
            Arc::clone(&collection_pda_info),
        ) {
            Ok(signature) => format!("{} {}", style("Signature:").bold(), signature),
            Err(err) => {
                pb.abandon_with_message(format!("{}", style("Mint failed ").red().bold()));
                error!("{:?}", err);
                return Err(err);
            }
        };

        pb.finish_with_message(result);
    } else {
        let pb = progress_bar_with_style(number);

        for _i in 0..number {
            if let Err(err) = mint(
                Arc::clone(&client),
                tars_pubkey,
                Arc::clone(&tars_state),
                Arc::clone(&collection_pda_info),
            ) {
                pb.abandon_with_message(format!("{}", style("Mint failed ").red().bold()));
                error!("{:?}", err);
                return Err(err);
            }

            pb.inc(1);
        }

        pb.finish();
    }

    Ok(())
}

pub fn mint(
    client: Arc<Client>,
    tars_id: Pubkey,
    tars_state: Arc<Tars>,
    collection_pda_info: Arc<Option<PdaInfo<CollectionPDA>>>,
) -> Result<Signature> {
    let program = client.program(TARS_ID);
    let payer = program.payer();
    let wallet = tars_state.wallet;
    let authority = tars_state.authority;

    let tars_data = &tars_state.data;

    if let Some(_gatekeeper) = &tars_data.gatekeeper {
        return Err(anyhow!(
            "Command-line mint disabled (gatekeeper settings in use)"
        ));
    } else if tars_state.items_redeemed >= tars_data.items_available {
        return Err(anyhow!(TarsError::TarsEmpty));
    }

    if tars_state.authority != payer {
        // we are not authority, we need to follow the rules
        // 1. go_live_date
        // 2. whitelist mint settings
        // 3. end settings
        let mint_date = Utc::now().timestamp();
        let mut mint_enabled = if let Some(date) = tars_data.go_live_date {
            // mint will be enabled only if the go live date is earlier
            // than the current date
            date < mint_date
        } else {
            // this is the case that go live date is null
            false
        };

        if let Some(wl_mint_settings) = &tars_data.whitelist_mint_settings {
            if wl_mint_settings.presale {
                // we (temporarily) enable the mint - we will validate if the user
                // has the wl token when creating the transaction
                mint_enabled = true;
            } else if !mint_enabled {
                return Err(anyhow!(TarsError::TarsNotLive));
            }
        }

        if !mint_enabled {
            // no whitelist mint settings (or no presale) and we are earlier than
            // go live date
            return Err(anyhow!(TarsError::TarsNotLive));
        }

        if let Some(end_settings) = &tars_data.end_settings {
            match end_settings.end_setting_type {
                EndSettingType::Date => {
                    if (end_settings.number as i64) < mint_date {
                        return Err(anyhow!(TarsError::TarsNotLive));
                    }
                }
                EndSettingType::Amount => {
                    if tars_state.items_redeemed >= end_settings.number {
                        return Err(anyhow!(
                            "Tars is not live (end settings amount reached)"
                        ));
                    }
                }
            }
        }
    }

    let nft_mint = Keypair::new();
    let metaplex_program_id = Pubkey::from_str(METAPLEX_PROGRAM_ID)?;

    // Allocate memory for the account
    let min_rent = program
        .rpc()
        .get_minimum_balance_for_rent_exemption(MINT_LAYOUT as usize)?;

    // Create mint account
    let create_mint_account_ix = system_instruction::create_account(
        &payer,
        &nft_mint.pubkey(),
        min_rent,
        MINT_LAYOUT,
        &TOKEN_PROGRAM_ID,
    );

    // Initialize mint ix
    let init_mint_ix = initialize_mint(
        &TOKEN_PROGRAM_ID,
        &nft_mint.pubkey(),
        &payer,
        Some(&payer),
        0,
    )?;

    // Derive associated token account
    let assoc = get_associated_token_address(&payer, &nft_mint.pubkey());

    // Create associated account instruction
    let create_assoc_account_ix =
        create_associated_token_account(&payer, &payer, &nft_mint.pubkey());

    // Mint to instruction
    let mint_to_ix = mint_to(
        &TOKEN_PROGRAM_ID,
        &nft_mint.pubkey(),
        &assoc,
        &payer,
        &[],
        1,
    )?;

    let mut additional_accounts: Vec<AccountMeta> = Vec::new();

    // Check whitelist mint settings
    if let Some(wl_mint_settings) = &tars_data.whitelist_mint_settings {
        let whitelist_token_account = get_associated_token_address(&payer, &wl_mint_settings.mint);

        additional_accounts.push(AccountMeta {
            pubkey: whitelist_token_account,
            is_signer: false,
            is_writable: true,
        });

        if wl_mint_settings.mode == WhitelistMintMode::BurnEveryTime {
            let mut token_found = false;

            match program.rpc().get_account_data(&whitelist_token_account) {
                Ok(ata_data) => {
                    if !ata_data.is_empty() {
                        let account = Account::unpack_unchecked(&ata_data)?;

                        if account.amount > 0 {
                            additional_accounts.push(AccountMeta {
                                pubkey: wl_mint_settings.mint,
                                is_signer: false,
                                is_writable: true,
                            });

                            additional_accounts.push(AccountMeta {
                                pubkey: payer,
                                is_signer: true,
                                is_writable: false,
                            });

                            token_found = true;
                        }
                    }
                }
                Err(err) => {
                    error!("Invalid whitelist token account: {}", err);
                    return Err(anyhow!(
                        "Uninitialized whitelist token account: {whitelist_token_account}.
                         Check that you provided a valid SPL token mint for the whitelist."
                    ));
                }
            }

            if !token_found {
                return Err(anyhow!(TarsError::NoWhitelistToken));
            }
        }
    }

    if let Some(token_mint) = tars_state.token_mint {
        let user_token_account_info = get_associated_token_address(&payer, &token_mint);

        additional_accounts.push(AccountMeta {
            pubkey: user_token_account_info,
            is_signer: false,
            is_writable: true,
        });

        additional_accounts.push(AccountMeta {
            pubkey: payer,
            is_signer: true,
            is_writable: false,
        });
    }

    let metadata_pda = find_metadata_pda(&nft_mint.pubkey());
    let master_edition_pda = find_master_edition_pda(&nft_mint.pubkey());
    let (tars_creator_pda, creator_bump) =
        find_tars_creator_pda(&tars_id);

    let mut mint_ix = program
        .request()
        .accounts(nft_accounts::MintNFT {
            tars: tars_id,
            tars_creator: tars_creator_pda,
            payer,
            wallet,
            metadata: metadata_pda,
            mint: nft_mint.pubkey(),
            mint_authority: payer,
            update_authority: payer,
            master_edition: master_edition_pda,
            token_metadata_program: metaplex_program_id,
            token_program: TOKEN_PROGRAM_ID,
            system_program: system_program::id(),
            rent: sysvar::rent::ID,
            clock: sysvar::clock::ID,
            recent_blockhashes: sysvar::recent_blockhashes::ID,
            instruction_sysvar_account: sysvar::instructions::ID,
        })
        .args(nft_instruction::MintNft { creator_bump });

    // Add additional accounts directly to the mint instruction otherwise it won't work.
    if !additional_accounts.is_empty() {
        mint_ix = mint_ix.accounts(additional_accounts);
    }
    let mint_ix = mint_ix.instructions()?;

    let mut builder = program
        .request()
        .instruction(create_mint_account_ix)
        .instruction(init_mint_ix)
        .instruction(create_assoc_account_ix)
        .instruction(mint_to_ix)
        .instruction(mint_ix[0].clone())
        .signer(&nft_mint);

    if let Some((collection_pda_pubkey, collection_pda)) = collection_pda_info.as_ref() {
        let collection_authority_record =
            find_collection_authority_account(&collection_pda.mint, collection_pda_pubkey).0;
        builder = builder
            .accounts(nft_accounts::SetCollectionDuringMint {
                tars: tars_id,
                metadata: metadata_pda,
                payer,
                collection_pda: *collection_pda_pubkey,
                token_metadata_program: mpl_token_metadata::ID,
                instructions: sysvar::instructions::ID,
                collection_mint: collection_pda.mint,
                collection_metadata: find_metadata_pda(&collection_pda.mint),
                collection_master_edition: find_master_edition_pda(&collection_pda.mint),
                authority,
                collection_authority_record,
            })
            .args(nft_instruction::SetCollectionDuringMint {});
    }

    let sig = builder.send()?;

    if let Err(_) | Ok(Response { value: None, .. }) = program
        .rpc()
        .get_account_with_commitment(&metadata_pda, CommitmentConfig::processed())
    {
        let cluster_param = match get_cluster(program.rpc()).unwrap_or(Cluster::Mainnet) {
            Cluster::Devnet => "?devnet",
            Cluster::Mainnet => "",
        };
        return Err(anyhow!(
            "Minting most likely failed with a bot tax. Check the transaction link for more details: https://explorer.solana.com/tx/{}{}",
            sig.to_string(),
            cluster_param,
        ));
    }

    info!("Minted! TxId: {}", sig);

    Ok(sig)
}
