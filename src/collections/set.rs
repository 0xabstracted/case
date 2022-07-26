use std::str::FromStr;

use anchor_client::solana_sdk::{pubkey::Pubkey, system_program, sysvar};
use anyhow::Result;
use console::style;
use tars::{accounts as nft_accounts, instruction as nft_instruction, TarsError};
use mpl_token_metadata::{
    error::MetadataError,
    pda::find_collection_authority_account,
    state::{MasterEditionV2, Metadata},
};

use crate::{
    cache::load_cache,
    tars::{TARS_ID, *},
    common::*,
    pdas::*,
    utils::{assert_correct_authority, spinner_with_style},
};

pub struct SetCollectionArgs {
    pub collection_mint: String,
    pub keypair: Option<String>,
    pub rpc_url: Option<String>,
    pub cache: String,
    pub tars: Option<String>,
}

pub fn process_set_collection(args: SetCollectionArgs) -> Result<()> {
    let case_config = case_setup(args.keypair, args.rpc_url)?;
    let client = setup_client(&case_config)?;
    let program = client.program(TARS_ID);
    let mut cache = Cache::new();

    // The tars id specified takes precedence over the one from the cache.
    let tars_id = match args.tars {
        Some(ref tars_id) => tars_id,
        None => {
            cache = load_cache(&args.cache, false)?;
            &cache.program.tars
        }
    };

    let collection_mint_pubkey = match Pubkey::from_str(&args.collection_mint) {
        Ok(tars_pubkey) => tars_pubkey,
        Err(_) => {
            let error = anyhow!(
                "Failed to parse collection mint id: {}",
                args.collection_mint
            );
            error!("{:?}", error);
            return Err(error);
        }
    };

    let tars_pubkey = match Pubkey::from_str(tars_id) {
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

    let tars_state =
        get_tars_state(&case_config, &Pubkey::from_str(tars_id)?)?;

    let collection_metadata_info = get_metadata_pda(&collection_mint_pubkey, &program)?;

    let collection_edition_info = get_master_edition_pda(&collection_mint_pubkey, &program)?;

    pb.finish_with_message("Done");

    assert_correct_authority(
        &case_config.keypair.pubkey(),
        &tars_state.authority,
    )?;

    println!(
        "\n{} {}Setting collection mint for tars",
        style("[2/2]").bold().dim(),
        COLLECTION_EMOJI
    );

    let pb = spinner_with_style();
    pb.set_message("Sending set collection transaction...");

    let set_signature = set_collection(
        &program,
        &tars_pubkey,
        &tars_state,
        &collection_mint_pubkey,
        &collection_metadata_info,
        &collection_edition_info,
    )?;

    // If a tars id wasn't manually specified we are operating on the tars in the cache
    // and so need to update the cache file.
    if args.tars.is_none() {
        cache.items.shift_remove("-1");
        cache.program.collection_mint = collection_mint_pubkey.to_string();
        cache.sync_file()?;
    }

    pb.finish_with_message(format!(
        "{} {}",
        style("Set collection signature:").bold(),
        set_signature
    ));

    Ok(())
}

pub fn set_collection(
    program: &Program,
    tars_pubkey: &Pubkey,
    tars_state: &Tars,
    collection_mint_pubkey: &Pubkey,
    collection_metadata_info: &PdaInfo<Metadata>,
    collection_edition_info: &PdaInfo<MasterEditionV2>,
) -> Result<Signature> {
    let payer = program.payer();

    let collection_pda_pubkey = find_collection_pda(tars_pubkey).0;
    let (collection_metadata_pubkey, collection_metadata) = collection_metadata_info;
    let (collection_edition_pubkey, collection_edition) = collection_edition_info;

    let collection_authority_record =
        find_collection_authority_account(collection_mint_pubkey, &collection_pda_pubkey).0;

    if !tars_state.data.retain_authority {
        return Err(anyhow!(TarsError::TarsCollectionRequiresRetainAuthority));
    }

    if collection_metadata.update_authority != payer {
        return Err(anyhow!(CustomTarsError::AuthorityMismatch(
            collection_metadata.update_authority.to_string(),
            payer.to_string()
        )));
    }

    if collection_edition.max_supply != Some(0) {
        return Err(anyhow!(MetadataError::CollectionMustBeAUniqueMasterEdition));
    }

    if tars_state.items_redeemed > 0 {
        return Err(anyhow!(
            "You can't modify the Tars collection after items have been minted."
        ));
    }

    let builder = program
        .request()
        .accounts(nft_accounts::SetCollection {
            tars: *tars_pubkey,
            authority: payer,
            collection_pda: collection_pda_pubkey,
            payer,
            system_program: system_program::id(),
            rent: sysvar::rent::ID,
            metadata: *collection_metadata_pubkey,
            mint: *collection_mint_pubkey,
            edition: *collection_edition_pubkey,
            collection_authority_record,
            token_metadata_program: mpl_token_metadata::ID,
        })
        .args(nft_instruction::SetCollection);

    let sig = builder.send()?;

    Ok(sig)
}
